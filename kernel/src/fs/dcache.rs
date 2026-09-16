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

//! Dentry Cache (dcache) — Sharded O(1) VFS path component lookup.
//!
//! ## Purpose
//!
//! Every `vfs::lookup()` call must resolve path components one at a time,
//! which means acquiring and releasing filesystem locks for each component.
//! For a path like `/usr/bin/bash` this is four lock acquisitions minimum.
//! Under multi-threaded or multi-core workloads (e.g. running cargo, dpkg, or shell pipelines),
//! `open()` is the hottest syscall by far, and a single global lock on dcache
//! becomes a major contention bottleneck.
//!
//! The sharded dcache maps `(parent_inode_number, component_name)` → `Arc<dyn InodeOps>`
//! across 64 independent hash shards, so lookups for different paths proceed concurrently
//! without lock contention.
//!
//! ## Design
//!
//! - 64 independent shards behind separate `TicketLock`s.
//! - Each shard contains a flat bucket array of size `BUCKETS_PER_SHARD` (64 buckets per shard, 4096 total).
//! - Shard and bucket selection uses bitwise masking (`parent_ino` xor/mix with FNV-1a `name_hash`).
//! - Atomic `DCACHE_HITS` and `DCACHE_MISSES` counters allow lock-free diagnostics without lock contention.
//! - LRU eviction: each entry carries a `generation` counter. When a bucket
//!   is occupied and we need to insert a new entry, we overwrite the existing bucket (last-write-wins per bucket).
//! - Invalidation: call `dcache_invalidate_inode(parent_ino)` on directory mutations — this scans
//!   and clears matching entries across all shards.
//!
//! ## Performance & Concurrency Rationale
//!
//! - Sharding by 64 divides global lock contention by up to 64x under parallel VFS path lookups.
//! - Atomic hit/miss counters eliminate write cacheline bounces on the dcache lock during read-heavy workloads.

use crate::fs::inode::InodeOps;
use crate::sync::spinlock::TicketLock;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

/// Number of independent dcache shards.
/// Must be a power of two for cheap bitwise AND masking.
const DCACHE_SHARD_COUNT: usize = 64;

/// Number of hash buckets per shard.
/// Must be a power of two for cheap bitwise AND masking.
const BUCKETS_PER_SHARD: usize = 64;

/// A single dentry cache entry.
struct DcacheEntry {
    /// Inode number of the parent directory.
    parent_ino: u64,
    /// Hash of the file name (FNV-1a).
    name_hash: u64,
    /// The resolved inode (or `None` for a negative entry — the name does not exist).
    inode: Option<Arc<dyn InodeOps>>,
    /// Generation counter used for eviction decisions.
    generation: u64,
}

/// A single dcache shard protecting `BUCKETS_PER_SHARD` entries.
struct DcacheShard {
    buckets: [Option<DcacheEntry>; BUCKETS_PER_SHARD],
    /// Generation counter for the shard.
    generation: u64,
}

impl DcacheShard {
    /// Create an empty dcache shard.
    const fn new() -> Self {
        Self {
            buckets: [const { None }; BUCKETS_PER_SHARD],
            generation: 0,
        }
    }
}

/// Global sharded dentry cache instance (64 independent lock shards).
static DCACHE_SHARDS: [TicketLock<DcacheShard>; DCACHE_SHARD_COUNT] = {
    const SHARD: TicketLock<DcacheShard> = TicketLock::new(DcacheShard::new());
    [SHARD; DCACHE_SHARD_COUNT]
};

/// Lock-free atomic cache hit counter.
static DCACHE_HITS: AtomicU64 = AtomicU64::new(0);

/// Lock-free atomic cache miss counter.
static DCACHE_MISSES: AtomicU64 = AtomicU64::new(0);

/// FNV-1a 64-bit hash of a byte string.
#[inline(always)]
fn fnv1a(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
    const FNV_PRIME: u64 = 1_099_511_628_211;
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Compute the `(shard_index, bucket_index)` pair for a `(parent_ino, name_hash)` tuple.
#[inline(always)]
fn locate_bucket(parent_ino: u64, name_hash: u64) -> (usize, usize) {
    let mixed = parent_ino
        .wrapping_mul(2_654_435_761)
        .wrapping_add(name_hash);
    let shard_idx = (mixed as usize) & (DCACHE_SHARD_COUNT - 1);
    let bucket_idx = ((mixed >> 6) as usize) & (BUCKETS_PER_SHARD - 1);
    (shard_idx, bucket_idx)
}

/// Look up a path component in the dcache.
///
/// Returns:
/// - `Some(Some(inode))` — positive hit: the entry exists and its inode is cached.
/// - `Some(None)` — negative hit: the entry is known not to exist.
/// - `None` — cache miss: the caller must perform a filesystem lookup.
pub fn dcache_lookup(parent_ino: u64, name: &str) -> Option<Option<Arc<dyn InodeOps>>> {
    let name_hash = fnv1a(name.as_bytes());
    let (shard_idx, bucket_idx) = locate_bucket(parent_ino, name_hash);

    let shard_guard = DCACHE_SHARDS[shard_idx].lock();
    if let Some(ref entry) = shard_guard.buckets[bucket_idx] {
        if entry.parent_ino == parent_ino && entry.name_hash == name_hash {
            let inode = entry.inode.clone();
            drop(shard_guard);
            DCACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return Some(inode);
        }
    }
    drop(shard_guard);
    DCACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    None
}

/// Insert or update a positive dentry cache entry.
///
/// This is called after a successful filesystem lookup so subsequent
/// lookups of the same `(parent_ino, name)` pair can be served from cache.
pub fn dcache_insert(parent_ino: u64, name: &str, inode: Arc<dyn InodeOps>) {
    let name_hash = fnv1a(name.as_bytes());
    let (shard_idx, bucket_idx) = locate_bucket(parent_ino, name_hash);

    let mut shard_guard = DCACHE_SHARDS[shard_idx].lock();
    let gen = shard_guard.generation;
    shard_guard.generation = gen.wrapping_add(1);
    shard_guard.buckets[bucket_idx] = Some(DcacheEntry {
        parent_ino,
        name_hash,
        inode: Some(inode),
        generation: gen,
    });
}

/// Insert a negative dentry cache entry.
///
/// Records that `name` does not exist under `parent_ino`, so subsequent
/// `open()` / `stat()` calls for non-existent paths can return `ENOENT`
/// without hitting the filesystem.
pub fn dcache_insert_negative(parent_ino: u64, name: &str) {
    let name_hash = fnv1a(name.as_bytes());
    let (shard_idx, bucket_idx) = locate_bucket(parent_ino, name_hash);

    let mut shard_guard = DCACHE_SHARDS[shard_idx].lock();
    let gen = shard_guard.generation;
    shard_guard.generation = gen.wrapping_add(1);
    shard_guard.buckets[bucket_idx] = Some(DcacheEntry {
        parent_ino,
        name_hash,
        inode: None,
        generation: gen,
    });
}

/// Invalidate all dcache entries whose parent inode number is `parent_ino`.
///
/// Must be called after any mutation to a directory:
/// `unlink`, `rename`, `mkdir`, `rmdir`, `create`.
pub fn dcache_invalidate_inode(parent_ino: u64) {
    for shard in &DCACHE_SHARDS {
        let mut shard_guard = shard.lock();
        for bucket in shard_guard.buckets.iter_mut() {
            if let Some(ref entry) = *bucket {
                if entry.parent_ino == parent_ino {
                    *bucket = None;
                }
            }
        }
    }
}

/// Invalidate a specific `(parent_ino, name)` entry.
///
/// More targeted than `dcache_invalidate_inode` when only a single name
/// changes (e.g., a single file is unlinked).
pub fn dcache_invalidate_entry(parent_ino: u64, name: &str) {
    let name_hash = fnv1a(name.as_bytes());
    let (shard_idx, bucket_idx) = locate_bucket(parent_ino, name_hash);

    let mut shard_guard = DCACHE_SHARDS[shard_idx].lock();
    if let Some(ref entry) = shard_guard.buckets[bucket_idx] {
        if entry.parent_ino == parent_ino && entry.name_hash == name_hash {
            shard_guard.buckets[bucket_idx] = None;
        }
    }
}

/// Flush the entire dcache (e.g., after unmounting a filesystem).
pub fn dcache_flush_all() {
    for shard in &DCACHE_SHARDS {
        let mut shard_guard = shard.lock();
        for bucket in shard_guard.buckets.iter_mut() {
            *bucket = None;
        }
    }
}

/// Return dcache hit/miss statistics: `(hits, misses)`.
pub fn dcache_stats() -> (u64, u64) {
    (
        DCACHE_HITS.load(Ordering::Relaxed),
        DCACHE_MISSES.load(Ordering::Relaxed),
    )
}
