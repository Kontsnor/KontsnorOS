//! Thread-safe Least Recently Used (LRU) block buffer cache for KontsnorOS.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use crate::drivers::traits::{BlockDevice, DriverError, DriverInfo};
use crate::kprintln;

struct CacheEntry {
    data: Vec<u8>,
    last_access: u64,
}

struct BlockCacheInner {
    entries: BTreeMap<u64, CacheEntry>,
    counter: u64,
}

/// A wrapper block device driver that caches reads and writes to an underlying block device.
pub struct BlockCache {
    device: Arc<dyn BlockDevice>,
    inner: Mutex<BlockCacheInner>,
    max_blocks: usize,
}

impl BlockCache {
    /// Create a new block cache wrapping the given device.
    pub fn new(device: Arc<dyn BlockDevice>, max_blocks: usize) -> Self {
        Self {
            device,
            inner: Mutex::new(BlockCacheInner {
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

        // kprintln!("[cache] read_block: block={}, num_blocks={}", block, num_blocks);

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
            // kprintln!("[cache] read_block hit: block={}", block);
            for i in 0..num_blocks {
                let curr_block = block + i as u64;
                let offset = i * block_size;
                let entry = inner.entries.get_mut(&curr_block).unwrap();
                entry.last_access = counter;
                buf[offset..offset + block_size].copy_from_slice(&entry.data);
            }
            return Ok(());
        }

        // kprintln!("[cache] read_block miss: block={}", block);

        // 2. Cache miss: release lock and read the whole range from the underlying device
        drop(inner);
        let mut disk_data = alloc::vec![0u8; buf.len()];
        self.device.read_block(block, &mut disk_data)?;

        // 3. Re-acquire lock to insert new entries and update access times
        let mut inner = self.inner.lock();
        for i in 0..num_blocks {
            let curr_block = block + i as u64;
            let offset = i * block_size;
            let block_slice = &disk_data[offset..offset + block_size];

            if !inner.entries.contains_key(&curr_block) {
                if inner.entries.len() >= self.max_blocks {
                    // Evict LRU entry
                    let mut lru_block = None;
                    let mut min_access = u64::MAX;
                    for (&b, entry) in &inner.entries {
                        if entry.last_access < min_access {
                            min_access = entry.last_access;
                            lru_block = Some(b);
                        }
                    }
                    if let Some(b) = lru_block {
                        inner.entries.remove(&b);
                    }
                }
                inner.entries.insert(curr_block, CacheEntry {
                    data: block_slice.to_vec(),
                    last_access: counter,
                });
            } else {
                let entry = inner.entries.get_mut(&curr_block).unwrap();
                entry.last_access = counter;
            }
            buf[offset..offset + block_size].copy_from_slice(block_slice);
        }

        Ok(())
    }

    fn write_block(&self, block: u64, data: &[u8]) -> Result<(), DriverError> {
        let block_size = self.device.block_size() as usize;
        if data.len() % block_size != 0 {
            return Err(DriverError::InvalidParam);
        }
        let num_blocks = data.len() / block_size;

        // Write-through: write the entire range to the physical device first
        self.device.write_block(block, data)?;

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
                if inner.entries.len() >= self.max_blocks {
                    // Evict LRU entry
                    let mut lru_block = None;
                    let mut min_access = u64::MAX;
                    for (&b, entry) in &inner.entries {
                        if entry.last_access < min_access {
                            min_access = entry.last_access;
                            lru_block = Some(b);
                        }
                    }
                    if let Some(b) = lru_block {
                        inner.entries.remove(&b);
                    }
                }
                inner.entries.insert(curr_block, CacheEntry {
                    data: block_slice.to_vec(),
                    last_access: counter,
                });
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
