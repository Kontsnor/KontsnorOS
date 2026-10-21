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
    referenced: bool,
    dirty: bool,
}

struct BlockCacheInner {
    entries: BTreeMap<u64, CacheEntry>,
    fifo_queue: alloc::collections::VecDeque<u64>,
    dirty_count: usize,
}

impl BlockCacheInner {
    /// Evict one unreferenced entry when over capacity.
    /// Returns Some((block, data)) if the evicted entry is dirty so caller can write it back.
    fn evict_one(&mut self, max_blocks: usize) -> Option<(u64, Vec<u8>)> {
        while self.entries.len() >= max_blocks {
            let b = self.fifo_queue.pop_front()?;
            if let Some(entry) = self.entries.get_mut(&b) {
                if entry.referenced {
                    entry.referenced = false;
                    self.fifo_queue.push_back(b);
                } else {
                    let removed = self.entries.remove(&b).unwrap();
                    if removed.dirty {
                        self.dirty_count = self.dirty_count.saturating_sub(1);
                        return Some((b, removed.data));
                    }
                    return None;
                }
            }
        }
        None
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
                fifo_queue: alloc::collections::VecDeque::new(),
                dirty_count: 0,
            }),
            max_blocks,
        }
    }

    /// Helper to write an aligned buffer to the underlying physical device.
    fn write_device_aligned(&self, block: u64, data: &[u8]) -> Result<(), DriverError> {
        if (data.as_ptr() as usize) % 512 == 0 {
            self.device.write_block(block, data)
        } else if data.len() <= 4096 {
            let mut stack_buf = AlignedStackBuf {
                data: [core::mem::MaybeUninit::uninit(); 4096],
            };
            // SAFETY: stack_buf.data is an aligned 4096-byte buffer, and slice length matches data.len() <= 4096.
            let slice = unsafe {
                core::slice::from_raw_parts_mut(stack_buf.data.as_mut_ptr() as *mut u8, data.len())
            };
            slice.copy_from_slice(data);
            self.device.write_block(block, slice)
        } else {
            let mut aligned_buf =
                AlignedBuffer::new(data.len(), 512).ok_or(DriverError::IoError)?;
            aligned_buf.as_mut_slice().copy_from_slice(data);
            self.device.write_block(block, aligned_buf.as_slice())
        }
    }

    /// Check whether the cache currently holds any dirty blocks.
    pub fn has_dirty_blocks(&self) -> bool {
        let inner = self.inner.lock();
        inner.dirty_count > 0
    }
}

impl Drop for BlockCache {
    fn drop(&mut self) {
        let _ = self.flush();
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

        let all_hits = (0..num_blocks).all(|i| inner.entries.contains_key(&(block + i as u64)));

        if all_hits {
            for i in 0..num_blocks {
                let curr_block = block + i as u64;
                let offset = i * block_size;
                let entry = inner.entries.get_mut(&curr_block).unwrap();
                entry.referenced = true;
                buf[offset..offset + block_size].copy_from_slice(&entry.data);
            }
            return Ok(());
        }

        // 2. Cache miss: release lock and read from underlying device.
        // If buf is already 512-byte aligned, read directly into it to avoid heap allocation.
        drop(inner);
        if (buf.as_ptr() as usize) % 512 == 0 {
            self.device.read_block(block, buf)?;
        } else if buf.len() <= 4096 {
            let mut stack_buf = AlignedStackBuf {
                data: [core::mem::MaybeUninit::uninit(); 4096],
            };
            // SAFETY: stack_buf.data is an aligned 4096-byte buffer.
            let slice = unsafe {
                core::slice::from_raw_parts_mut(stack_buf.data.as_mut_ptr() as *mut u8, buf.len())
            };
            self.device.read_block(block, slice)?;
            buf.copy_from_slice(slice);
        } else {
            let mut heap_buf = AlignedBuffer::new(buf.len(), 512).ok_or(DriverError::IoError)?;
            self.device.read_block(block, heap_buf.as_mut_slice())?;
            buf.copy_from_slice(heap_buf.as_slice());
        }

        // 3. Re-acquire lock to insert new entries and update access times
        let mut inner = self.inner.lock();
        for i in 0..num_blocks {
            let curr_block = block + i as u64;
            let offset = i * block_size;

            if let Some(entry) = inner.entries.get_mut(&curr_block) {
                entry.referenced = true;
                // Cached entry (especially if modified/dirty) takes precedence over older disk data
                buf[offset..offset + block_size].copy_from_slice(&entry.data);
            } else {
                while let Some((evicted_block, evicted_data)) = inner.evict_one(self.max_blocks) {
                    self.write_device_aligned(evicted_block, &evicted_data)?;
                }
                let block_vec = buf[offset..offset + block_size].to_vec();
                inner.entries.insert(
                    curr_block,
                    CacheEntry {
                        data: block_vec,
                        referenced: false,
                        dirty: false,
                    },
                );
                inner.fifo_queue.push_back(curr_block);
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

        for i in 0..num_blocks {
            let curr_block = block + i as u64;
            let offset = i * block_size;
            let block_slice = &data[offset..offset + block_size];

            let was_clean = if let Some(entry) = inner.entries.get_mut(&curr_block) {
                entry.referenced = true;
                let clean = !entry.dirty;
                entry.dirty = true;
                entry.data.copy_from_slice(block_slice);
                Some(clean)
            } else {
                None
            };

            match was_clean {
                Some(true) => {
                    inner.dirty_count += 1;
                }
                Some(false) => {}
                None => {
                    while let Some((evicted_block, evicted_data)) = inner.evict_one(self.max_blocks)
                    {
                        self.write_device_aligned(evicted_block, &evicted_data)?;
                    }
                    inner.entries.insert(
                        curr_block,
                        CacheEntry {
                            data: block_slice.to_vec(),
                            referenced: false,
                            dirty: true,
                        },
                    );
                    inner.dirty_count += 1;
                    inner.fifo_queue.push_back(curr_block);
                }
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
        let dirty_block_nums: Vec<u64> = {
            let inner = self.inner.lock();
            inner
                .entries
                .iter()
                .filter(|(_, entry)| entry.dirty)
                .map(|(&block, _)| block)
                .collect()
        };

        if !dirty_block_nums.is_empty() {
            let mut sorted_blocks = dirty_block_nums;
            sorted_blocks.sort_unstable();
            sorted_blocks.dedup();

            let max_chunk_blocks = 128;
            let block_size = self.device.block_size() as usize;
            let mut staging_buf = AlignedBuffer::new(max_chunk_blocks * block_size, 512);

            let mut i = 0;
            while i < sorted_blocks.len() {
                let start_block = sorted_blocks[i];
                let mut chunk_blocks = 0;

                if let Some(ref mut staging) = staging_buf {
                    let staging_slice = staging.as_mut_slice();
                    {
                        let inner = self.inner.lock();
                        let mut j = i;
                        while j < sorted_blocks.len()
                            && sorted_blocks[j] == start_block + (j - i) as u64
                            && (j - i) < max_chunk_blocks
                        {
                            if let Some(entry) = inner.entries.get(&sorted_blocks[j]) {
                                let offset = (j - i) * block_size;
                                staging_slice[offset..offset + block_size]
                                    .copy_from_slice(&entry.data);
                                chunk_blocks += 1;
                                j += 1;
                            } else {
                                if j == i {
                                    j += 1;
                                }
                                break;
                            }
                        }
                        i = j;
                    }

                    if chunk_blocks > 0 {
                        let total_bytes = chunk_blocks * block_size;
                        self.device
                            .write_block(start_block, &staging.as_slice()[..total_bytes])?;

                        let mut inner = self.inner.lock();
                        for k in 0..chunk_blocks {
                            let curr_b = start_block + k as u64;
                            if let Some(entry) = inner.entries.get_mut(&curr_b) {
                                let offset = k * block_size;
                                if entry.data == staging.as_slice()[offset..offset + block_size] {
                                    if entry.dirty {
                                        entry.dirty = false;
                                        inner.dirty_count = inner.dirty_count.saturating_sub(1);
                                    }
                                }
                            }
                        }
                    }
                } else {
                    let mut write_buf = Vec::new();
                    {
                        let inner = self.inner.lock();
                        let mut j = i;
                        while j < sorted_blocks.len()
                            && sorted_blocks[j] == start_block + (j - i) as u64
                            && (j - i) < max_chunk_blocks
                        {
                            if let Some(entry) = inner.entries.get(&sorted_blocks[j]) {
                                write_buf.extend_from_slice(&entry.data);
                                chunk_blocks += 1;
                                j += 1;
                            } else {
                                if j == i {
                                    j += 1;
                                }
                                break;
                            }
                        }
                        i = j;
                    }

                    if chunk_blocks > 0 {
                        self.write_device_aligned(start_block, &write_buf)?;

                        let mut inner = self.inner.lock();
                        for k in 0..chunk_blocks {
                            let curr_b = start_block + k as u64;
                            if let Some(entry) = inner.entries.get_mut(&curr_b) {
                                let offset = k * block_size;
                                if entry.data == write_buf[offset..offset + block_size] {
                                    if entry.dirty {
                                        entry.dirty = false;
                                        inner.dirty_count = inner.dirty_count.saturating_sub(1);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            self.device.flush()
        } else {
            self.device.flush()
        }
    }

    fn has_dirty_blocks(&self) -> bool {
        self.inner.lock().dirty_count > 0
    }

    fn info(&self) -> DriverInfo {
        self.device.info()
    }
}
