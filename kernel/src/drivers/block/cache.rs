// Copyright (C) 2026 KontsnorOS Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Thread-safe Least Recently Used (LRU) block buffer cache for KontsnorOS.

use crate::drivers::traits::{BlockDevice, DriverError, DriverInfo};
use crate::sync::mutex::KMutex;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

struct AlignedBuffer {
    ptr: *mut u8,
    layout: ::core::alloc::Layout,
}

impl AlignedBuffer {
    fn new(size: usize, align: usize) -> Option<Self> {
        let layout = ::core::alloc::Layout::from_size_align(size, align).ok()?;
        let ptr = unsafe { alloc::alloc::alloc(layout) };
        if ptr.is_null() {
            None
        } else {
            Some(Self { ptr, layout })
        }
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr, self.layout.size()) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe {
            alloc::alloc::dealloc(self.ptr, self.layout);
        }
    }
}

#[repr(align(512))]
struct AlignedStackBuf {
    data: [core::mem::MaybeUninit<u8>; 4096],
}

struct CacheEntry {
    data: Vec<u8>,
    last_access: u64,
}

struct BlockCacheInner {
    entries: BTreeMap<u64, CacheEntry>,
    counter: u64,
}

impl BlockCacheInner {
    #[inline]
    fn evict_lru(&mut self, max_blocks: usize) {
        if self.entries.len() >= max_blocks {
            let mut lru_block = None;
            let mut min_access = u64::MAX;
            for (&b, entry) in &self.entries {
                if entry.last_access < min_access {
                    min_access = entry.last_access;
                    lru_block = Some(b);
                }
            }
            if let Some(b) = lru_block {
                self.entries.remove(&b);
            }
        }
    }
}

/// A wrapper block device driver that caches reads and writes to an underlying block device.
pub struct BlockCache {
    device: Arc<dyn BlockDevice>,
    inner: KMutex<BlockCacheInner>,
    max_blocks: usize,
}

impl BlockCache {
    /// Create a new block cache wrapping the given device.
    pub fn new(device: Arc<dyn BlockDevice>, max_blocks: usize) -> Self {
        Self {
            device,
            inner: KMutex::new(BlockCacheInner {
                entries: BTreeMap::new(),
                counter: 0,
            }),
            max_blocks,
        }
    }
}

impl BlockDevice for BlockCache {
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let block_size = self.device.block_size() as usize;
        if buf.len() % block_size != 0 {
            return Err(DriverError::InvalidParam);
        }
        let num_blocks = buf.len() / block_size;

        // 1. Acquire lock to check for cache hits
        let mut inner = self.inner.lock();
        inner.counter += 1;
        let counter = inner.counter;

        let mut all_hits = true;
        for i in 0..num_blocks {
            if !inner.entries.contains_key(&(block + i as u64)) {
                all_hits = false;
                break;
            }
        }

        if all_hits {
            for i in 0..num_blocks {
                let curr_block = block + i as u64;
                let offset = i * block_size;
                let entry = inner.entries.get_mut(&curr_block).unwrap();
                entry.last_access = counter;
                buf[offset..offset + block_size].copy_from_slice(&entry.data);
            }
            return Ok(());
        }

        // 2. Cache miss: release lock and read from underlying device.
        // If buf is already 512-byte aligned, read directly into it to avoid heap allocation.
        drop(inner);
        let mut stack_buf;
        let mut heap_buf;
        let disk_data: &[u8] = if (buf.as_ptr() as usize) % 512 == 0 {
            self.device.read_block(block, buf)?;
            buf
        } else if buf.len() <= 4096 {
            stack_buf = AlignedStackBuf {
                data: [core::mem::MaybeUninit::uninit(); 4096],
            };
            // SAFETY: stack_buf.data is an aligned 4096-byte buffer.
            let slice = unsafe {
                core::slice::from_raw_parts_mut(stack_buf.data.as_mut_ptr() as *mut u8, buf.len())
            };
            self.device.read_block(block, slice)?;
            buf.copy_from_slice(slice);
            buf
        } else {
            heap_buf = AlignedBuffer::new(buf.len(), 512).ok_or(DriverError::IoError)?;
            self.device.read_block(block, heap_buf.as_mut_slice())?;
            buf.copy_from_slice(heap_buf.as_slice());
            buf
        };

        // 3. Re-acquire lock to insert new entries and update access times
        let mut inner = self.inner.lock();
        for i in 0..num_blocks {
            let curr_block = block + i as u64;
            let offset = i * block_size;
            let block_slice = &disk_data[offset..offset + block_size];

            if !inner.entries.contains_key(&curr_block) {
                inner.evict_lru(self.max_blocks);
                inner.entries.insert(
                    curr_block,
                    CacheEntry {
                        data: block_slice.to_vec(),
                        last_access: counter,
                    },
                );
            } else {
                let entry = inner.entries.get_mut(&curr_block).unwrap();
                entry.last_access = counter;
            }
        }

        Ok(())
    }

    fn write_block(&self, block: u64, data: &[u8]) -> Result<(), DriverError> {
        let block_size = self.device.block_size() as usize;
        if data.len() % block_size != 0 {
            return Err(DriverError::InvalidParam);
        }
        let num_blocks = data.len() / block_size;

        // Write-through: write range to physical device.
        // If data is already 512-byte aligned, pass directly to avoid intermediate buffer allocation and copy.
        if (data.as_ptr() as usize) % 512 == 0 {
            self.device.write_block(block, data)?;
        } else if data.len() <= 4096 {
            let mut stack_buf = AlignedStackBuf {
                data: [core::mem::MaybeUninit::uninit(); 4096],
            };
            // SAFETY: stack_buf.data is an aligned 4096-byte buffer.
            let slice = unsafe {
                core::slice::from_raw_parts_mut(stack_buf.data.as_mut_ptr() as *mut u8, data.len())
            };
            slice.copy_from_slice(data);
            self.device.write_block(block, slice)?;
        } else {
            let mut aligned_buf =
                AlignedBuffer::new(data.len(), 512).ok_or(DriverError::IoError)?;
            aligned_buf.as_mut_slice().copy_from_slice(data);
            self.device.write_block(block, aligned_buf.as_slice())?;
        }

        let mut inner = self.inner.lock();
        inner.counter += 1;
        let counter = inner.counter;

        for i in 0..num_blocks {
            let curr_block = block + i as u64;
            let offset = i * block_size;
            let block_slice = &data[offset..offset + block_size];

            if let Some(entry) = inner.entries.get_mut(&curr_block) {
                entry.last_access = counter;
                entry.data.copy_from_slice(block_slice);
            } else {
                inner.evict_lru(self.max_blocks);
                inner.entries.insert(
                    curr_block,
                    CacheEntry {
                        data: block_slice.to_vec(),
                        last_access: counter,
                    },
                );
            }
        }

        Ok(())
    }

    fn block_size(&self) -> u64 {
        self.device.block_size()
    }

    fn block_count(&self) -> u64 {
        self.device.block_count()
    }

    fn flush(&self) -> Result<(), DriverError> {
        self.device.flush()
    }

    fn info(&self) -> DriverInfo {
        self.device.info()
    }
}
