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

struct CacheEntry {
    data: Vec<u8>,
    last_access: u64,
    dirty: bool,
}

struct BlockCacheInner {
    entries: BTreeMap<u64, CacheEntry>,
    lru_map: BTreeMap<u64, u64>,
    counter: u64,
}

impl BlockCacheInner {
    fn update_access(&mut self, block: u64) {
        if let Some(entry) = self.entries.get_mut(&block) {
            let old_access = entry.last_access;
            self.lru_map.remove(&old_access);
            self.counter += 1;
            entry.last_access = self.counter;
            self.lru_map.insert(self.counter, block);
        }
    }
}

impl BlockCache {
    /// Flush a specific dirty cache block to the underlying device if present and dirty.
    fn flush_entry_internal(device: &dyn BlockDevice, block: u64, entry: &mut CacheEntry) -> Result<(), DriverError> {
        if entry.dirty {
            let mut aligned_buf = AlignedBuffer::new(entry.data.len(), 512).ok_or(DriverError::IoError)?;
            aligned_buf.as_mut_slice().copy_from_slice(&entry.data);
            device.write_block(block, aligned_buf.as_slice())?;
            entry.dirty = false;
        }
        Ok(())
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
                lru_map: BTreeMap::new(),
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

        // kprintln!("[cache] read_block: block={}, num_blocks={}", block, num_blocks);

        // 1. Acquire lock to check for cache hits
        let mut inner = self.inner.lock();
        inner.counter += 1;

        let mut all_hits = true;
        for i in 0..num_blocks {
            if !inner.entries.contains_key(&(block + i as u64)) {
                all_hits = false;
                break;
            }
        }

        if all_hits {
            // kprintln!("[cache] read_block hit: block={}", block);
            for i in 0..num_blocks {
                let curr_block = block + i as u64;
                let offset = i * block_size;
                let entry = inner.entries.get(&curr_block).unwrap();
                buf[offset..offset + block_size].copy_from_slice(&entry.data);
                inner.update_access(curr_block);
            }
            return Ok(());
        }

        // kprintln!("[cache] read_block miss: block={}", block);

        // 2. Cache miss: release lock and read the whole range from the underlying device into an aligned buffer
        drop(inner);
        let mut aligned_buf = AlignedBuffer::new(buf.len(), 512).ok_or(DriverError::IoError)?;
        self.device.read_block(block, aligned_buf.as_mut_slice())?;
        let disk_data = aligned_buf.as_slice();

        // 3. Re-acquire lock to insert new entries and update access times
        let mut inner = self.inner.lock();
        for i in 0..num_blocks {
            let curr_block = block + i as u64;
            let offset = i * block_size;
            let block_slice = &disk_data[offset..offset + block_size];

            if !inner.entries.contains_key(&curr_block) {
                while inner.entries.len() >= self.max_blocks {
                    if let Some((&lru_access, &lru_block)) = inner.lru_map.iter().next() {
                        inner.lru_map.remove(&lru_access);
                        if let Some(mut entry) = inner.entries.remove(&lru_block) {
                            let _ = Self::flush_entry_internal(&*self.device, lru_block, &mut entry);
                        }
                    } else {
                        break;
                    }
                }
                inner.counter += 1;
                let new_counter = inner.counter;
                inner.entries.insert(
                    curr_block,
                    CacheEntry {
                        data: block_slice.to_vec(),
                        last_access: new_counter,
                        dirty: false,
                    },
                );
                inner.lru_map.insert(new_counter, curr_block);
                buf[offset..offset + block_size].copy_from_slice(block_slice);
            } else {
                let entry = inner.entries.get(&curr_block).unwrap();
                buf[offset..offset + block_size].copy_from_slice(&entry.data);
                inner.update_access(curr_block);
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

        let mut inner = self.inner.lock();
        inner.counter += 1;

        for i in 0..num_blocks {
            let curr_block = block + i as u64;
            let offset = i * block_size;
            let block_slice = &data[offset..offset + block_size];

            if inner.entries.contains_key(&curr_block) {
                let entry = inner.entries.get_mut(&curr_block).unwrap();
                entry.data.copy_from_slice(block_slice);
                entry.dirty = true;
                inner.update_access(curr_block);
            } else {
                while inner.entries.len() >= self.max_blocks {
                    if let Some((&lru_access, &lru_block)) = inner.lru_map.iter().next() {
                        inner.lru_map.remove(&lru_access);
                        if let Some(mut entry) = inner.entries.remove(&lru_block) {
                            let _ = Self::flush_entry_internal(&*self.device, lru_block, &mut entry);
                        }
                    } else {
                        break;
                    }
                }
                inner.counter += 1;
                let new_counter = inner.counter;
                inner.entries.insert(
                    curr_block,
                    CacheEntry {
                        data: block_slice.to_vec(),
                        last_access: new_counter,
                        dirty: true,
                    },
                );
                inner.lru_map.insert(new_counter, curr_block);
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
        let mut inner = self.inner.lock();
        for (&b, entry) in inner.entries.iter_mut() {
            Self::flush_entry_internal(&*self.device, b, entry)?;
        }
        self.device.flush()
    }

    fn info(&self) -> DriverInfo {
        self.device.info()
    }
}
