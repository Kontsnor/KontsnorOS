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

//! Dentry Cache (dcache) — O(1) VFS path component lookup.
//!
//! ## Purpose
//!
//! Every `vfs::lookup()` call must resolve path components one at a time,
//! which means acquiring and releasing filesystem locks for each component.
//! For a path like `/usr/bin/bash` this is four lock acquisitions minimum.
//! Under Ubuntu with apt/cargo/dpkg running, `open()` is the hottest syscall
//! by far, and naive per-component lookup is a major bottleneck.
//!
//! The dcache maps `(parent_inode_number, component_name)` → `Arc<dyn InodeOps>`
//! so that repeated lookups of the same path component are O(1) with no
//! filesystem lock needed.
//!
//! ## Design
//!
//! - Fixed-capacity hash table with `DCACHE_BUCKETS` buckets.
//! - Each bucket is an `Option<DcacheEntry>` — simple open addressing is
//!   avoided in favour of a flat bucket array for cache-friendliness.
//! - LRU eviction: each entry carries a `generation` counter. When a bucket
//!   is occupied and we need to insert a new entry, we always evict the
//!   existing entry (simplified LRU: last-write-wins per bucket).
//! - Invalidation: call `dcache_invalidate_inode(parent_ino)` on any
//!   `unlink`, `rename`, `mkdir`, `rmdir` — this scans and clears all
//!   entries whose parent matches, which is O(DCACHE_BUCKETS) but rare.
//!
//! ## Safety
//!
//! All access is guarded by a single `TicketLock` on the global dcache.
//! Fine-grained per-bucket locking is left as a future optimisation
//! (see plan item 3.3 for the analogous page cache sharding).

use crate::fs::inode::InodeOps;
use crate::sync::spinlock::TicketLock;
use alloc::sync::Arc;

/// Number of hash buckets in the dcache.
/// Must be a power of two for the cheap modulo via bit-AND.
const DCACHE_BUCKETS: usize = 4096;

/// A single dentry cache entry.
struct DcacheEntry {
    /// Device ID of the parent directory filesystem.
    parent_dev: u64,
    /// Inode number of the parent directory.
    parent_ino: u64,
    /// Hash of the file name (FNV-1a).
    name_hash: u64,
    /// The resolved inode (or `None` for a negative entry — the name does not exist).
    inode: Option<Arc<dyn InodeOps>>,
    /// Generation counter used for eviction decisions.
    generation: u64,
}

/// The global dentry cache.
struct Dcache {
    buckets: [Option<DcacheEntry>; DCACHE_BUCKETS],
    /// Monotonically increasing generation counter.
    generation: u64,
    /// Hits since last reset (diagnostic).
    hits: u64,
    /// Misses since last reset (diagnostic).
    misses: u64,
}

impl Dcache {
    /// Create an empty dcache. Uses `const` default since arrays of non-Copy
    /// types cannot be zero-initialised in Rust stable without unsafe.
    const fn new() -> Self {
        // SAFETY: `Option<DcacheEntry>` is valid when all bytes are zero
        // because `None` is represented as the null discriminant.
        Self {
            // SAFETY: DcacheEntry contains only primitive and Arc fields.
            // We initialise the array manually below because Rust requires
            // const-constructible elements for `[T; N]` array initialisers.
            buckets: [const { None }; DCACHE_BUCKETS],
            generation: 0,
            hits: 0,
            misses: 0,
        }
    }
}

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

/// Compute the bucket index for a `(parent_dev, parent_ino, name)` tuple.
#[inline(always)]
fn bucket_index(parent_dev: u64, parent_ino: u64, name_hash: u64) -> usize {
    let combined = parent_dev
        .wrapping_mul(0x9e3779b97f4a7c15)
        .wrapping_add(parent_ino.wrapping_mul(2_654_435_761))
        .wrapping_add(name_hash);
    (combined as usize) & (DCACHE_BUCKETS - 1)
}

/// Global dentry cache instance.
static DCACHE: TicketLock<Dcache> = TicketLock::new(Dcache::new());

/// Look up a path component in the dcache.
///
/// Returns:
/// - `Some(Some(inode))` — positive hit: the entry exists and its inode is cached.
/// - `Some(None)` — negative hit: the entry is known not to exist.
/// - `None` — cache miss: the caller must perform a filesystem lookup.
pub fn dcache_lookup(
    parent_dev: u64,
    parent_ino: u64,
    name: &str,
) -> Option<Option<Arc<dyn InodeOps>>> {
    let name_hash = fnv1a(name.as_bytes());
    let idx = bucket_index(parent_dev, parent_ino, name_hash);

    let mut cache = DCACHE.lock();
    if let Some(ref entry) = cache.buckets[idx] {
        if entry.parent_dev == parent_dev
            && entry.parent_ino == parent_ino
            && entry.name_hash == name_hash
        {
            let inode = entry.inode.clone();
            cache.hits += 1;
            return Some(inode);
        }
    }
    cache.misses += 1;
    None
}

/// Insert or update a positive dentry cache entry.
///
/// This is called after a successful filesystem lookup so subsequent
/// lookups of the same `(parent_dev, parent_ino, name)` tuple can be served from cache.
pub fn dcache_insert(parent_dev: u64, parent_ino: u64, name: &str, inode: Arc<dyn InodeOps>) {
    let name_hash = fnv1a(name.as_bytes());
    let idx = bucket_index(parent_dev, parent_ino, name_hash);

    let mut cache = DCACHE.lock();
    let gen = cache.generation;
    cache.generation = gen.wrapping_add(1);
    cache.buckets[idx] = Some(DcacheEntry {
        parent_dev,
        parent_ino,
        name_hash,
        inode: Some(inode),
        generation: gen,
    });
}

/// Insert a negative dentry cache entry.
///
/// Records that `name` does not exist under `(parent_dev, parent_ino)`, so subsequent
/// `open()` / `stat()` calls for non-existent paths can return `ENOENT`
/// without hitting the filesystem.
pub fn dcache_insert_negative(parent_dev: u64, parent_ino: u64, name: &str) {
    let name_hash = fnv1a(name.as_bytes());
    let idx = bucket_index(parent_dev, parent_ino, name_hash);

    let mut cache = DCACHE.lock();
    let gen = cache.generation;
    cache.generation = gen.wrapping_add(1);
    cache.buckets[idx] = Some(DcacheEntry {
        parent_dev,
        parent_ino,
        name_hash,
        inode: None,
        generation: gen,
    });
}

/// Invalidate all dcache entries whose parent directory is `(parent_dev, parent_ino)`.
///
/// Must be called after any bulk mutation to a directory:
/// `rmdir`, recursive deletion, etc.
pub fn dcache_invalidate_inode(parent_dev: u64, parent_ino: u64) {
    let mut cache = DCACHE.lock();
    for bucket in cache.buckets.iter_mut() {
        if let Some(ref entry) = *bucket {
            if entry.parent_dev == parent_dev && entry.parent_ino == parent_ino {
                *bucket = None;
            }
        }
    }
}

/// Invalidate a specific `(parent_dev, parent_ino, name)` entry.
///
/// More targeted than `dcache_invalidate_inode` when only a single name
/// changes (e.g., a single file is unlinked, renamed, or created).
pub fn dcache_invalidate_entry(parent_dev: u64, parent_ino: u64, name: &str) {
    let name_hash = fnv1a(name.as_bytes());
    let idx = bucket_index(parent_dev, parent_ino, name_hash);

    let mut cache = DCACHE.lock();
    if let Some(ref entry) = cache.buckets[idx] {
        if entry.parent_dev == parent_dev
            && entry.parent_ino == parent_ino
            && entry.name_hash == name_hash
        {
            cache.buckets[idx] = None;
        }
    }
}

/// Flush the entire dcache (e.g., after unmounting a filesystem).
pub fn dcache_flush_all() {
    let mut cache = DCACHE.lock();
    for bucket in cache.buckets.iter_mut() {
        *bucket = None;
    }
}

/// Return dcache hit/miss statistics: `(hits, misses)`.
pub fn dcache_stats() -> (u64, u64) {
    let cache = DCACHE.lock();
    (cache.hits, cache.misses)
}
