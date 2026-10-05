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

//! Page Cache subsystem for mapping and caching file inodes in virtual memory.

use crate::fs::inode::InodeOps;
use crate::sync::spinlock::TicketLock;
use crate::syscall::Errno;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use x86_64::structures::paging::{PageTable, PageTableFlags};
use x86_64::VirtAddr;

/// An entry in the Page Cache representing a single physical frame.
#[derive(Debug, Clone, Copy)]
pub struct PageCacheEntry {
    /// Physical address of the frame.
    pub phys_addr: u64,
    /// Whether the frame has been modified and needs to be written back to disk.
    pub dirty: bool,
    /// Reference flag for CLOCK clean-page eviction.
    pub referenced: bool,
}

/// Global dirty pages counter for O(1) checks.
pub static GLOBAL_DIRTY_PAGES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Number of independent page cache shards.
/// Must be a power of two so shard selection is a cheap bitwise AND.
const PAGE_CACHE_SHARD_COUNT: usize = 64;

/// Cache state for a single inode within a shard.
struct InodeCacheState {
    pages: BTreeMap<u64, PageCacheEntry>,
    dirty_count: usize,
}

impl InodeCacheState {
    fn new() -> Self {
        Self {
            pages: BTreeMap::new(),
            dirty_count: 0,
        }
    }
}

/// A single page cache shard.
struct PageCacheShard {
    inodes: BTreeMap<(u64, u64), InodeCacheState>,
    clock: alloc::collections::VecDeque<(u64, u64, u64)>,
}

impl PageCacheShard {
    const fn new() -> Self {
        Self {
            inodes: BTreeMap::new(),
            clock: alloc::collections::VecDeque::new(),
        }
    }
}

/// Sharded page cache — 64 independent shards partitioned by (dev, ino).
/// Lookups for entries in different inodes proceed concurrently without contention,
/// and per-file operations (invalidate, truncate, flush) only touch a single shard.
static PAGE_CACHE_SHARDS: [TicketLock<PageCacheShard>; PAGE_CACHE_SHARD_COUNT] = {
    const SHARD: TicketLock<PageCacheShard> = TicketLock::new(PageCacheShard::new());
    [SHARD; PAGE_CACHE_SHARD_COUNT]
};

/// Select the shard index for a given `(dev, ino)` key.
#[inline(always)]
fn shard_index(dev: u64, ino: u64) -> usize {
    let mixed = dev
        .wrapping_mul(1_140_071_481_932_319_8485)
        .wrapping_add(ino.wrapping_mul(2_654_435_761));
    (mixed as usize) & (PAGE_CACHE_SHARD_COUNT - 1)
}

/// Look up an entry in the sharded page cache.
pub fn page_cache_get(dev: u64, ino: u64, aligned_offset: u64) -> Option<PageCacheEntry> {
    let shard = shard_index(dev, ino);
    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
    if let Some(inode_state) = guard.inodes.get_mut(&(dev, ino)) {
        if let Some(entry) = inode_state.pages.get_mut(&aligned_offset) {
            entry.referenced = true;
            return Some(*entry);
        }
    }
    None
}

/// Insert an entry into the sharded page cache.
pub fn page_cache_insert(dev: u64, ino: u64, aligned_offset: u64, entry: PageCacheEntry) {
    let shard = shard_index(dev, ino);
    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
    let inode_state = guard
        .inodes
        .entry((dev, ino))
        .or_insert_with(InodeCacheState::new);
    if let Some(old) = inode_state.pages.insert(aligned_offset, entry) {
        if old.dirty && !entry.dirty {
            inode_state.dirty_count = inode_state.dirty_count.saturating_sub(1);
            GLOBAL_DIRTY_PAGES.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
        } else if !old.dirty && entry.dirty {
            inode_state.dirty_count += 1;
            GLOBAL_DIRTY_PAGES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
    } else {
        if entry.dirty {
            inode_state.dirty_count += 1;
            GLOBAL_DIRTY_PAGES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        guard.clock.push_back((dev, ino, aligned_offset));
    }
}

/// Atomically insert a page if not present; if already present, returns the existing phys_addr.
/// If inserted, returns None.
pub fn page_cache_insert_or_get(
    dev: u64,
    ino: u64,
    aligned_offset: u64,
    entry: PageCacheEntry,
) -> Option<u64> {
    let shard = shard_index(dev, ino);
    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
    let inode_state = guard
        .inodes
        .entry((dev, ino))
        .or_insert_with(InodeCacheState::new);
    if let Some(existing) = inode_state.pages.get_mut(&aligned_offset) {
        existing.referenced = true;
        Some(existing.phys_addr)
    } else {
        if entry.dirty {
            inode_state.dirty_count += 1;
            GLOBAL_DIRTY_PAGES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        inode_state.pages.insert(aligned_offset, entry);
        guard.clock.push_back((dev, ino, aligned_offset));
        None
    }
}

/// Remove an entry from the sharded page cache.
pub fn page_cache_remove(dev: u64, ino: u64, aligned_offset: u64) {
    let shard = shard_index(dev, ino);
    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
    if let Some(inode_state) = guard.inodes.get_mut(&(dev, ino)) {
        if let Some(entry) = inode_state.pages.remove(&aligned_offset) {
            if entry.dirty {
                inode_state.dirty_count = inode_state.dirty_count.saturating_sub(1);
                GLOBAL_DIRTY_PAGES.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
        if inode_state.pages.is_empty() {
            guard.inodes.remove(&(dev, ino));
        }
    }
}

/// Invalidate and remove all cached pages for the given inode on a filesystem device in O(pages of inode).
pub fn page_cache_invalidate_inode(dev: u64, ino: u64) {
    let shard = shard_index(dev, ino);
    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
    if let Some(inode_state) = guard.inodes.remove(&(dev, ino)) {
        for (_off, entry) in inode_state.pages {
            if entry.dirty {
                GLOBAL_DIRTY_PAGES.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
            }
            crate::memory::physical::deallocate_frame(entry.phys_addr);
        }
    }
}

/// Invalidate and remove cached pages for the given inode beyond `size` on a filesystem device in O(truncated pages).
pub fn page_cache_truncate_inode(dev: u64, ino: u64, size: u64) {
    let aligned_size = (size + 4095) & !4095;
    let shard = shard_index(dev, ino);
    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
    if let Some(inode_state) = guard.inodes.get_mut(&(dev, ino)) {
        let to_remove: alloc::vec::Vec<u64> = inode_state
            .pages
            .range(aligned_size..)
            .map(|(&off, _)| off)
            .collect();
        for off in to_remove {
            if let Some(entry) = inode_state.pages.remove(&off) {
                if entry.dirty {
                    inode_state.dirty_count = inode_state.dirty_count.saturating_sub(1);
                    GLOBAL_DIRTY_PAGES.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
                }
                crate::memory::physical::deallocate_frame(entry.phys_addr);
            }
        }
        if inode_state.pages.is_empty() {
            guard.inodes.remove(&(dev, ino));
        }
    }
}

/// Mark a cached page as dirty (needs write-back).
pub fn page_cache_mark_dirty(dev: u64, ino: u64, aligned_offset: u64) {
    let shard = shard_index(dev, ino);
    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
    if let Some(inode_state) = guard.inodes.get_mut(&(dev, ino)) {
        if let Some(entry) = inode_state.pages.get_mut(&aligned_offset) {
            if !entry.dirty {
                entry.dirty = true;
                inode_state.dirty_count += 1;
                GLOBAL_DIRTY_PAGES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

/// Collect all unique `(dev, ino)` pairs that have dirty pages across all shards.
pub fn dirty_inodes() -> alloc::vec::Vec<(u64, u64)> {
    if !has_dirty_pages() {
        return alloc::vec::Vec::new();
    }
    let mut inodes = alloc::vec::Vec::new();
    for shard in &PAGE_CACHE_SHARDS {
        let guard = shard.lock();
        for (&(dev, ino), state) in guard.inodes.iter() {
            if state.dirty_count > 0 {
                inodes.push((dev, ino));
            }
        }
    }
    inodes.sort_unstable();
    inodes.dedup();
    inodes
}

/// Collect all unique inode numbers for a specific filesystem device that have dirty pages.
pub fn dirty_inodes_for_dev(dev: u64) -> alloc::vec::Vec<u64> {
    if !has_dirty_pages() {
        return alloc::vec::Vec::new();
    }
    let mut inodes = alloc::vec::Vec::new();
    for shard in &PAGE_CACHE_SHARDS {
        let guard = shard.lock();
        for (&(d, ino), state) in guard.inodes.iter() {
            if d == dev && state.dirty_count > 0 {
                inodes.push(ino);
            }
        }
    }
    inodes.sort_unstable();
    inodes.dedup();
    inodes
}

/// Check if any page cache entry across all shards is marked dirty in O(1).
#[inline]
pub fn has_dirty_pages() -> bool {
    GLOBAL_DIRTY_PAGES.load(core::sync::atomic::Ordering::Relaxed) > 0
}

/// Check if any page cache entry for a specific filesystem device is marked dirty.
pub fn has_dirty_pages_for_dev(dev: u64) -> bool {
    if !has_dirty_pages() {
        return false;
    }
    for shard in &PAGE_CACHE_SHARDS {
        let guard = shard.lock();
        for (&(d, _), state) in guard.inodes.iter() {
            if d == dev && state.dirty_count > 0 {
                return true;
            }
        }
    }
    false
}

/// Check if any page cache entry for a specific inode is marked dirty in O(1).
pub fn has_dirty_pages_for_inode(dev: u64, ino: u64) -> bool {
    if !has_dirty_pages() {
        return false;
    }
    let shard = shard_index(dev, ino);
    let guard = PAGE_CACHE_SHARDS[shard].lock();
    if let Some(state) = guard.inodes.get(&(dev, ino)) {
        state.dirty_count > 0
    } else {
        false
    }
}

/// Evict clean, unmapped pages under memory pressure.
/// Returns the number of frames successfully evicted.
pub fn page_cache_evict_clean_pages(target_pages: usize) -> usize {
    let mut evicted = 0;
    for shard in &PAGE_CACHE_SHARDS {
        let mut guard = shard.lock();
        let max_check = guard.clock.len() * 2;
        let mut checked = 0;

        while checked < max_check && evicted < target_pages {
            checked += 1;
            if let Some((dev, ino, offset)) = guard.clock.pop_front() {
                let mut should_evict = false;
                let mut phys_to_free = 0;

                if let Some(inode_state) = guard.inodes.get_mut(&(dev, ino)) {
                    if let Some(entry) = inode_state.pages.get_mut(&offset) {
                        if !entry.dirty {
                            let frame_idx = (entry.phys_addr >> 12) as usize;
                            let refs = if frame_idx < crate::memory::physical::FRAME_REFS.len() {
                                crate::memory::physical::FRAME_REFS[frame_idx]
                                    .load(core::sync::atomic::Ordering::Relaxed)
                            } else {
                                2
                            };

                            // Only evict clean pages not mapped into user address space (refs <= 1)
                            if refs <= 1 {
                                if entry.referenced {
                                    entry.referenced = false;
                                    guard.clock.push_back((dev, ino, offset));
                                } else {
                                    phys_to_free = entry.phys_addr;
                                    should_evict = true;
                                }
                            } else {
                                guard.clock.push_back((dev, ino, offset));
                            }
                        } else {
                            guard.clock.push_back((dev, ino, offset));
                        }
                    }
                }

                if should_evict {
                    if let Some(inode_state) = guard.inodes.get_mut(&(dev, ino)) {
                        inode_state.pages.remove(&offset);
                        if inode_state.pages.is_empty() {
                            guard.inodes.remove(&(dev, ino));
                        }
                    }
                    crate::memory::physical::deallocate_frame(phys_to_free);
                    evicted += 1;
                }
            } else {
                break;
            }
        }

        if evicted >= target_pages {
            break;
        }
    }
    evicted
}

/// Walk the page table of a task to get a mutable reference to the target page table entry.
///
/// # Safety
///
/// The caller must ensure that the pml4_phys root address is valid.
pub unsafe fn get_page_table_entry(
    pml4_phys: u64,
    addr: VirtAddr,
) -> Option<&'static mut x86_64::structures::paging::page_table::PageTableEntry> {
    let phys_mem_offset = crate::memory::r#virtual::phys_mem_offset();
    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset);
    let pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };

    let pml4_entry = &mut pml4[addr.p4_index()];
    if pml4_entry.is_unused() {
        return None;
    }

    let pdpt_phys = pml4_entry.frame().ok()?.start_address().as_u64();
    let pdpt_virt = VirtAddr::new(pdpt_phys + phys_mem_offset);
    let pdpt: &mut PageTable = unsafe { &mut *pdpt_virt.as_mut_ptr() };

    let pdpt_entry = &mut pdpt[addr.p3_index()];
    if pdpt_entry.is_unused() {
        return None;
    }

    let pd_phys = pdpt_entry.frame().ok()?.start_address().as_u64();
    let pd_virt = VirtAddr::new(pd_phys + phys_mem_offset);
    let pd: &mut PageTable = unsafe { &mut *pd_virt.as_mut_ptr() };

    let pd_entry = &mut pd[addr.p2_index()];
    if pd_entry.is_unused() {
        return None;
    }

    let pt_phys = pd_entry.frame().ok()?.start_address().as_u64();
    let pt_virt = VirtAddr::new(pt_phys + phys_mem_offset);
    let pt: &mut PageTable = unsafe { &mut *pt_virt.as_mut_ptr() };

    Some(&mut pt[addr.p1_index()])
}

/// Retrieve the physical address of the page cache entry for `(inode_ino, offset)`.
/// If it does not exist, it allocates a frame, reads the content from the inode using `read_direct`,
/// and inserts it into the cache.
pub fn get_or_create_page(inode: &Arc<dyn InodeOps>, offset: u64) -> Result<u64, Errno> {
    get_or_create_page_inner(&**inode, offset)
}

const DEBUG_PAGE_CACHE: bool = false;

/// Helper function implementing page cache retrieval using raw `&dyn InodeOps`.
pub fn get_or_create_page_inner(inode: &dyn InodeOps, offset: u64) -> Result<u64, Errno> {
    let dev = inode.inode().dev;
    let ino = inode.inode().ino;
    let aligned_offset = offset & !4095;

    // 1. Quick check: sharded O(1) lookup — no global lock needed.
    if let Some(entry) = page_cache_get(dev, ino, aligned_offset) {
        crate::fs::kstats::KSTATS
            .page_cache_hits
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        return Ok(entry.phys_addr);
    }
    crate::fs::kstats::KSTATS
        .page_cache_misses
        .fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    // 2. Allocate and read WITHOUT holding the lock
    if DEBUG_PAGE_CACHE {
        crate::kprintln!(
            "[page_cache] allocating frame for dev={}, ino={}, offset={:#x}",
            dev,
            ino,
            aligned_offset
        );
    }
    let phys = match crate::memory::physical::allocate_frame() {
        Some(f) => f,
        None => {
            page_cache_evict_clean_pages(32);
            crate::memory::physical::allocate_frame().ok_or(Errno::ENOMEM)?
        }
    };
    if DEBUG_PAGE_CACHE {
        crate::kprintln!(
            "[page_cache] allocated frame: phys={:#x}, calling read_direct",
            phys
        );
    }

    // Read 4096 bytes from the inode at aligned_offset using read_direct
    let phys_offset = phys + crate::memory::r#virtual::phys_mem_offset();
    let dest_slice = unsafe { core::slice::from_raw_parts_mut(phys_offset as *mut u8, 4096) };

    let mut total_read = 0;
    while total_read < 4096 {
        if DEBUG_PAGE_CACHE {
            crate::kprintln!(
                "[page_cache] read_direct offset={:#x}, remaining={}",
                aligned_offset + total_read as u64,
                4096 - total_read
            );
        }
        match inode.read_direct(
            aligned_offset + total_read as u64,
            &mut dest_slice[total_read..],
        ) {
            Ok(0) => {
                if DEBUG_PAGE_CACHE {
                    crate::kprintln!("[page_cache] read_direct returned EOF");
                }
                break;
            }
            Ok(n) => {
                if DEBUG_PAGE_CACHE {
                    crate::kprintln!("[page_cache] read_direct read {} bytes", n);
                }
                total_read += n;
            }
            Err(e) => {
                if DEBUG_PAGE_CACHE {
                    crate::kprintln!("[page_cache] read_direct error: {}", e);
                }
                crate::memory::physical::deallocate_frame(phys);
                return Err(Errno::EIO);
            }
        }
    }

    // Only zero the trailing remainder if read_direct did not fill the entire 4KB frame (e.g. at EOF)
    if total_read < 4096 {
        dest_slice[total_read..].fill(0);
    }
    if DEBUG_PAGE_CACHE {
        crate::kprintln!(
            "[page_cache] read_direct finished, total_read={}",
            total_read
        );
    }

    // 3. Re-acquire shard lock and insert/check atomically (double-checked locking pattern)
    let shard = shard_index(dev, ino);
    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
    let state = guard
        .inodes
        .entry((dev, ino))
        .or_insert_with(InodeCacheState::new);
    if let Some(entry) = state.pages.get_mut(&aligned_offset) {
        // Someone else allocated and read it in the meantime!
        // Deallocate our frame and return the existing one.
        let phys_addr = entry.phys_addr;
        entry.referenced = true;
        drop(guard);
        crate::memory::physical::deallocate_frame(phys);
        if DEBUG_PAGE_CACHE {
            crate::kprintln!(
                "[page_cache] already present in cache: phys={:#x}",
                phys_addr
            );
        }
        return Ok(phys_addr);
    }

    state.pages.insert(
        aligned_offset,
        PageCacheEntry {
            phys_addr: phys,
            dirty: false,
            referenced: true,
        },
    );
    guard.clock.push_back((dev, ino, aligned_offset));
    drop(guard);
    if DEBUG_PAGE_CACHE {
        crate::kprintln!("[page_cache] inserted into cache: phys={:#x}", phys);
    }

    Ok(phys)
}

/// Helper function implementing page cache retrieval for writes.
/// When `is_full_page` is true, skips calling `read_direct` and zeroing memory,
/// since the entire 4KB frame will be overwritten immediately by the caller.
pub fn get_or_create_page_for_write(
    inode: &dyn InodeOps,
    offset: u64,
    skip_read: bool,
    is_full_page: bool,
) -> Result<u64, Errno> {
    let dev = inode.inode().dev;
    let ino = inode.inode().ino;
    let aligned_offset = offset & !4095;

    let shard = shard_index(dev, ino);
    {
        let mut guard = PAGE_CACHE_SHARDS[shard].lock();
        if let Some(state) = guard.inodes.get_mut(&(dev, ino)) {
            if let Some(entry) = state.pages.get_mut(&aligned_offset) {
                if !entry.dirty {
                    entry.dirty = true;
                    state.dirty_count += 1;
                    GLOBAL_DIRTY_PAGES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                }
                entry.referenced = true;
                return Ok(entry.phys_addr);
            }
        }
    }

    let file_size = inode.inode().size;
    let beyond_eof = aligned_offset >= file_size;

    if skip_read || beyond_eof {
        // Allocate frame directly without reading from disk
        let phys = match crate::memory::physical::allocate_frame() {
            Some(f) => f,
            None => {
                page_cache_evict_clean_pages(32);
                crate::memory::physical::allocate_frame().ok_or(Errno::ENOMEM)?
            }
        };
        let phys_offset = phys + crate::memory::r#virtual::phys_mem_offset();
        // SAFETY: phys_offset points to a newly allocated 4KB physical frame in the direct physical mapping.
        let dest_slice = unsafe { core::slice::from_raw_parts_mut(phys_offset as *mut u8, 4096) };
        if !is_full_page {
            dest_slice.fill(0);
        }

        // Insert into cache with dirty marked true
        let mut guard = PAGE_CACHE_SHARDS[shard].lock();
        let state = guard
            .inodes
            .entry((dev, ino))
            .or_insert_with(InodeCacheState::new);
        if let Some(entry) = state.pages.get_mut(&aligned_offset) {
            let phys_addr = entry.phys_addr;
            if !entry.dirty {
                entry.dirty = true;
                state.dirty_count += 1;
                GLOBAL_DIRTY_PAGES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
            entry.referenced = true;
            drop(guard);
            crate::memory::physical::deallocate_frame(phys);
            return Ok(phys_addr);
        }

        state.dirty_count += 1;
        GLOBAL_DIRTY_PAGES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        state.pages.insert(
            aligned_offset,
            PageCacheEntry {
                phys_addr: phys,
                dirty: true,
                referenced: true,
            },
        );
        guard.clock.push_back((dev, ino, aligned_offset));
        drop(guard);

        return Ok(phys);
    }

    let phys = get_or_create_page_inner(inode, offset)?;
    page_cache_mark_dirty(dev, ino, aligned_offset);
    Ok(phys)
}

/// Flush a dirty page cache frame back to disk using VFS writes.
pub fn flush_page(inode: &Arc<dyn InodeOps>, offset: u64) -> Result<(), Errno> {
    flush_page_inner(&**inode, offset)
}

/// Helper function implementing dirty page cache frame flushing using raw `&dyn InodeOps`.
pub fn flush_page_inner(inode: &dyn InodeOps, offset: u64) -> Result<(), Errno> {
    let dev = inode.inode().dev;
    let ino = inode.inode().ino;
    let aligned_offset = offset & !4095;

    // 1. Check if dirty and clear dirty flag under shard lock BEFORE writing
    let phys_to_write = {
        let shard = shard_index(dev, ino);
        let mut guard = PAGE_CACHE_SHARDS[shard].lock();
        if let Some(state) = guard.inodes.get_mut(&(dev, ino)) {
            if let Some(entry) = state.pages.get_mut(&aligned_offset) {
                if entry.dirty {
                    entry.dirty = false;
                    state.dirty_count = state.dirty_count.saturating_sub(1);
                    GLOBAL_DIRTY_PAGES.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
                    Some(entry.phys_addr)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    };

    // 2. Snapshot the frame into a local buffer and perform write without holding shard lock.
    //    A concurrent write_page_cache may be modifying the live frame between the dirty-flag
    //    clear above and the write_direct below, so reading directly from the physical frame
    //    can produce a torn (partially-updated) snapshot.  Copying into a local buffer under
    //    a brief volatile read prevents handing a half-written page to write_direct.
    if let Some(phys) = phys_to_write {
        let phys_offset = phys + crate::memory::r#virtual::phys_mem_offset();
        // SAFETY: phys_offset points to a valid 4KB physical frame in the direct mapping.
        let src_slice = unsafe { core::slice::from_raw_parts(phys_offset as *const u8, 4096) };
        let mut snapshot = [0u8; 4096];
        snapshot.copy_from_slice(src_slice);

        let size = inode.inode().size;
        if size > aligned_offset {
            let write_len = core::cmp::min(4096, (size - aligned_offset) as usize);
            if let Err(_e) = inode.write_direct(aligned_offset, &snapshot[..write_len]) {
                let shard = shard_index(dev, ino);
                let mut guard = PAGE_CACHE_SHARDS[shard].lock();
                if let Some(state) = guard.inodes.get_mut(&(dev, ino)) {
                    if let Some(entry) = state.pages.get_mut(&aligned_offset) {
                        if entry.phys_addr == phys && !entry.dirty {
                            entry.dirty = true;
                            state.dirty_count += 1;
                            GLOBAL_DIRTY_PAGES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
                return Err(Errno::EIO);
            }
        }
    }
    Ok(())
}

/// Flush all dirty pages for a given inode.
pub fn flush_all_for_inode(inode: &Arc<dyn InodeOps>) -> Result<(), Errno> {
    flush_all_for_inode_inner(&**inode)
}

/// Record of a shared memory mapping for scalable reverse-lookup during page cache sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedMmapRecord {
    pub page_table_root: u64,
    pub dev: u64,
    pub ino: u64,
    pub start: u64,
    pub len: usize,
    pub offset: u64,
}

pub static SHARED_MMAP_RECORDS: spin::Mutex<alloc::vec::Vec<SharedMmapRecord>> =
    spin::Mutex::new(alloc::vec::Vec::new());

/// Register a shared file mapping for reverse-mapping tracking.
pub fn register_shared_mmap(record: SharedMmapRecord) {
    let mut list = SHARED_MMAP_RECORDS.lock();
    if !list
        .iter()
        .any(|r| r.page_table_root == record.page_table_root && r.start == record.start)
    {
        list.push(record);
    }
}

/// Unregister any shared mappings overlapping [start, end) for a given page table.
pub fn unregister_shared_mmap_range(page_table_root: u64, start: u64, end: u64) {
    let mut list = SHARED_MMAP_RECORDS.lock();
    list.retain(|r| {
        if r.page_table_root != page_table_root {
            return true;
        }
        let r_end = r.start + r.len as u64;
        r_end <= start || r.start >= end
    });
}

/// Unregister all shared mappings for an address space being freed.
pub fn unregister_shared_mmaps_for_page_table(page_table_root: u64) {
    let mut list = SHARED_MMAP_RECORDS.lock();
    list.retain(|r| r.page_table_root != page_table_root);
}

/// Helper function implementing dirty page cache flushing for all pages of an inode using raw `&dyn InodeOps`.
pub fn flush_all_for_inode_inner(inode: &dyn InodeOps) -> Result<(), Errno> {
    let dev = inode.inode().dev;
    let ino = inode.inode().ino;

    // Use reverse mapping registry to query only shared mappings for this inode,
    // completely eliminating lock contention on TASKS and O(N) full task scans.
    let matching_records: alloc::vec::Vec<SharedMmapRecord> = {
        let list = SHARED_MMAP_RECORDS.lock();
        list.iter()
            .filter(|r| r.dev == dev && r.ino == ino)
            .copied()
            .collect()
    };

    for record in matching_records {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let start_page = record.start & !4095;
            let end_page = (record.start + record.len as u64 - 1) & !4095;
            for vaddr in (start_page..=end_page).step_by(4096) {
                let page_offset_in_mapping = vaddr - record.start;
                let file_offset = record.offset + page_offset_in_mapping;

                // SAFETY: get_page_table_entry safely traverses the page table root.
                unsafe {
                    if let Some(pte) =
                        get_page_table_entry(record.page_table_root, VirtAddr::new(vaddr))
                    {
                        let mut flags = pte.flags();
                        if flags.contains(PageTableFlags::DIRTY) {
                            flags.remove(PageTableFlags::DIRTY);
                            pte.set_addr(pte.addr(), flags);
                            x86_64::instructions::tlb::flush(VirtAddr::new(vaddr));

                            // Mark dirty in cache
                            let aligned_file_offset = file_offset & !4095;
                            page_cache_mark_dirty(dev, ino, aligned_file_offset);
                        }
                    }
                }
            }
        });
    }

    let shard = shard_index(dev, ino);
    let offsets: alloc::vec::Vec<u64> = {
        let guard = PAGE_CACHE_SHARDS[shard].lock();
        if let Some(state) = guard.inodes.get(&(dev, ino)) {
            state
                .pages
                .iter()
                .filter_map(|(&off, entry)| if entry.dirty { Some(off) } else { None })
                .collect()
        } else {
            alloc::vec::Vec::new()
        }
    };

    if offsets.is_empty() {
        return Ok(());
    }

    // Merge adjacent dirty offsets into contiguous multi-page chunks
    let mut i = 0;
    while i < offsets.len() {
        let run_start = offsets[i];
        let mut run_pages = 1;
        while i + run_pages < offsets.len()
            && offsets[i + run_pages] == run_start + (run_pages as u64) * 4096
            && run_pages < 32
        {
            run_pages += 1;
        }

        // Snapshot dirty frames and clear dirty under shard lock
        let mut frames_to_write = alloc::vec::Vec::with_capacity(run_pages);
        {
            let mut guard = PAGE_CACHE_SHARDS[shard].lock();
            if let Some(state) = guard.inodes.get_mut(&(dev, ino)) {
                for p in 0..run_pages {
                    let page_off = run_start + (p as u64) * 4096;
                    if let Some(entry) = state.pages.get_mut(&page_off) {
                        if entry.dirty {
                            entry.dirty = false;
                            state.dirty_count = state.dirty_count.saturating_sub(1);
                            GLOBAL_DIRTY_PAGES.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
                            frames_to_write.push((page_off, entry.phys_addr));
                        }
                    }
                }
            }
        }

        // Perform merged write without holding shard lock
        if !frames_to_write.is_empty() {
            let total_bytes = frames_to_write.len() * 4096;
            let mut write_buf = alloc::vec![0u8; total_bytes];
            let phys_mem_off = crate::memory::r#virtual::phys_mem_offset();

            for (idx, &(_page_off, phys)) in frames_to_write.iter().enumerate() {
                let phys_offset = phys + phys_mem_off;
                // SAFETY: phys points to a valid physical frame in the direct mapping.
                let src_slice =
                    unsafe { core::slice::from_raw_parts(phys_offset as *const u8, 4096) };
                write_buf[idx * 4096..(idx + 1) * 4096].copy_from_slice(src_slice);
            }

            let size = inode.inode().size;
            if size > run_start {
                let valid_bytes = core::cmp::min(total_bytes, (size - run_start) as usize);
                if let Err(_e) = inode.write_direct(run_start, &write_buf[..valid_bytes]) {
                    // Restore dirty flags on write error
                    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
                    if let Some(state) = guard.inodes.get_mut(&(dev, ino)) {
                        for (page_off, _phys) in frames_to_write {
                            if let Some(entry) = state.pages.get_mut(&page_off) {
                                if !entry.dirty {
                                    entry.dirty = true;
                                    state.dirty_count += 1;
                                    GLOBAL_DIRTY_PAGES
                                        .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    return Err(Errno::EIO);
                }
            }
        }

        i += run_pages;
    }
    Ok(())
}

/// Mark a page cache page as dirty.
pub fn mark_dirty(dev: u64, ino: u64, offset: u64) {
    let aligned_offset = offset & !4095;
    page_cache_mark_dirty(dev, ino, aligned_offset);
}

/// Sync dirty pages in a virtual memory range `[unmap_start, unmap_end)` for a task's address space.
pub fn sync_mapped_region(
    page_table_root: u64,
    region: &crate::process::task::MappedRegion,
    unmap_start: u64,
    unmap_end: u64,
) {
    if !region.is_shared {
        return;
    }
    let inode = match region.inode {
        Some(ref inode) => inode,
        None => return,
    };
    let dev = inode.inode().dev;
    let ino = inode.inode().ino;

    let range_start = core::cmp::max(region.start, unmap_start);
    let range_end = core::cmp::min(region.start.saturating_add(region.len as u64), unmap_end);
    if range_start >= range_end {
        return;
    }

    let start_page = range_start & !4095;
    let end_page = (range_end - 1) & !4095;

    for vaddr in (start_page..=end_page).step_by(4096) {
        let page_offset = vaddr - region.start;
        let file_offset = region.offset + page_offset;
        let aligned_file_offset = file_offset & !4095;

        let mut should_flush = false;
        let mut phys_addr_to_write = None;

        // SAFETY: Walking page table under valid root address.
        unsafe {
            if let Some(pte) = get_page_table_entry(page_table_root, VirtAddr::new(vaddr)) {
                let mut flags = pte.flags();
                if flags.contains(PageTableFlags::DIRTY) || (region.prot & 2) != 0 {
                    if let Ok(frame) = pte.frame() {
                        phys_addr_to_write = Some(frame.start_address().as_u64());
                        should_flush = true;
                    }
                    if flags.contains(PageTableFlags::DIRTY) {
                        flags.remove(PageTableFlags::DIRTY);
                        pte.set_addr(pte.addr(), flags);
                        x86_64::instructions::tlb::flush(VirtAddr::new(vaddr));
                    }
                }
            }
        }

        // Also check if marked dirty in sharded page cache
        if !should_flush {
            if let Some(entry) = page_cache_get(dev, ino, aligned_file_offset) {
                if entry.dirty {
                    phys_addr_to_write = Some(entry.phys_addr);
                    should_flush = true;
                }
            }
        }

        if should_flush {
            if let Some(phys) = phys_addr_to_write {
                let phys_offset = phys + crate::memory::r#virtual::phys_mem_offset();
                // SAFETY: phys is a valid allocated physical page frame mapped into virtual memory.
                let src_slice =
                    unsafe { core::slice::from_raw_parts(phys_offset as *const u8, 4096) };
                let mut snapshot = [0u8; 4096];
                snapshot.copy_from_slice(src_slice);
                let size = inode.inode().size;
                if size > aligned_file_offset {
                    let write_len = core::cmp::min(4096, (size - aligned_file_offset) as usize);
                    let _ = inode.write_direct(aligned_file_offset, &snapshot[..write_len]);
                }

                let shard = shard_index(dev, ino);
                let mut guard = PAGE_CACHE_SHARDS[shard].lock();
                if let Some(state) = guard.inodes.get_mut(&(dev, ino)) {
                    if let Some(entry) = state.pages.get_mut(&aligned_file_offset) {
                        if entry.phys_addr == phys && entry.dirty {
                            entry.dirty = false;
                            state.dirty_count = state.dirty_count.saturating_sub(1);
                            GLOBAL_DIRTY_PAGES.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }
        }
    }
}
