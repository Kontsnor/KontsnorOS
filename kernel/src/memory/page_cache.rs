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
}

/// Number of independent page cache shards.
/// Must be a power of two so shard selection is a cheap bitwise AND.
const PAGE_CACHE_SHARD_COUNT: usize = 64;

/// A single page cache shard.
struct PageCacheShard {
    map: BTreeMap<(u64, u64, u64), PageCacheEntry>,
}

impl PageCacheShard {
    const fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }
}

/// Sharded page cache — 64 independent BTreeMaps behind separate TicketLocks.
/// Lookups for entries in different shards proceed concurrently without contention.
static PAGE_CACHE_SHARDS: [TicketLock<PageCacheShard>; PAGE_CACHE_SHARD_COUNT] = {
    const SHARD: TicketLock<PageCacheShard> = TicketLock::new(PageCacheShard::new());
    [SHARD; PAGE_CACHE_SHARD_COUNT]
};

/// Select the shard index for a given `(dev, ino, aligned_offset)` key.
#[inline(always)]
fn shard_index(dev: u64, ino: u64, aligned_offset: u64) -> usize {
    // Mix dev, ino, and offset so adjacent pages and different files land in distinct shards.
    let mixed = dev
        .wrapping_mul(1_140_071_481_932_319_8485)
        .wrapping_add(ino.wrapping_mul(2_654_435_761))
        .wrapping_add(aligned_offset >> 12);
    (mixed as usize) & (PAGE_CACHE_SHARD_COUNT - 1)
}

/// Look up an entry in the sharded page cache.
pub fn page_cache_get(dev: u64, ino: u64, aligned_offset: u64) -> Option<PageCacheEntry> {
    let shard = shard_index(dev, ino, aligned_offset);
    PAGE_CACHE_SHARDS[shard]
        .lock()
        .map
        .get(&(dev, ino, aligned_offset))
        .copied()
}

/// Insert an entry into the sharded page cache.
pub fn page_cache_insert(dev: u64, ino: u64, aligned_offset: u64, entry: PageCacheEntry) {
    let shard = shard_index(dev, ino, aligned_offset);
    PAGE_CACHE_SHARDS[shard]
        .lock()
        .map
        .insert((dev, ino, aligned_offset), entry);
}

/// Remove an entry from the sharded page cache.
pub fn page_cache_remove(dev: u64, ino: u64, aligned_offset: u64) {
    let shard = shard_index(dev, ino, aligned_offset);
    PAGE_CACHE_SHARDS[shard]
        .lock()
        .map
        .remove(&(dev, ino, aligned_offset));
}

/// Invalidate and remove all cached pages for the given inode on a filesystem device.
pub fn page_cache_invalidate_inode(dev: u64, ino: u64) {
    for shard in &PAGE_CACHE_SHARDS {
        let mut guard = shard.lock();
        guard.map.retain(|key, entry| {
            if key.0 == dev && key.1 == ino {
                crate::memory::physical::deallocate_frame(entry.phys_addr);
                false
            } else {
                true
            }
        });
    }
}

/// Invalidate and remove cached pages for the given inode beyond `size` on a filesystem device.
pub fn page_cache_truncate_inode(dev: u64, ino: u64, size: u64) {
    let aligned_size = (size + 4095) & !4095;
    for shard in &PAGE_CACHE_SHARDS {
        let mut guard = shard.lock();
        guard.map.retain(|key, entry| {
            if key.0 == dev && key.1 == ino && key.2 >= aligned_size {
                crate::memory::physical::deallocate_frame(entry.phys_addr);
                false
            } else {
                true
            }
        });
    }
}

/// Mark a cached page as dirty (needs write-back).
pub fn page_cache_mark_dirty(dev: u64, ino: u64, aligned_offset: u64) {
    let shard = shard_index(dev, ino, aligned_offset);
    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
    if let Some(entry) = guard.map.get_mut(&(dev, ino, aligned_offset)) {
        entry.dirty = true;
    }
}

/// Collect all unique `(dev, ino)` pairs that have dirty pages across all shards.
pub fn dirty_inodes() -> alloc::vec::Vec<(u64, u64)> {
    let mut inodes = alloc::vec::Vec::new();
    for shard in &PAGE_CACHE_SHARDS {
        let guard = shard.lock();
        for (key, entry) in guard.map.iter() {
            if entry.dirty {
                inodes.push((key.0, key.1));
            }
        }
    }
    inodes.sort_unstable();
    inodes.dedup();
    inodes
}

/// Collect all unique inode numbers for a specific filesystem device that have dirty pages.
pub fn dirty_inodes_for_dev(dev: u64) -> alloc::vec::Vec<u64> {
    let mut inodes = alloc::vec::Vec::new();
    for shard in &PAGE_CACHE_SHARDS {
        let guard = shard.lock();
        for (key, entry) in guard.map.iter() {
            if key.0 == dev && entry.dirty {
                inodes.push(key.1);
            }
        }
    }
    inodes.sort_unstable();
    inodes.dedup();
    inodes
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
        return Ok(entry.phys_addr);
    }

    // 2. Allocate and read WITHOUT holding the lock
    if DEBUG_PAGE_CACHE {
        crate::kprintln!(
            "[page_cache] allocating frame for dev={}, ino={}, offset={:#x}",
            dev,
            ino,
            aligned_offset
        );
    }
    let phys = crate::memory::physical::allocate_frame().ok_or(Errno::ENOMEM)?;
    if DEBUG_PAGE_CACHE {
        crate::kprintln!(
            "[page_cache] allocated frame: phys={:#x}, calling read_direct",
            phys
        );
    }

    // Read 4096 bytes from the inode at aligned_offset using read_direct
    let phys_offset = phys + crate::memory::r#virtual::phys_mem_offset();
    let dest_slice = unsafe { core::slice::from_raw_parts_mut(phys_offset as *mut u8, 4096) };
    dest_slice.fill(0);

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
    if DEBUG_PAGE_CACHE {
        crate::kprintln!(
            "[page_cache] read_direct finished, total_read={}",
            total_read
        );
    }

    // 3. Re-acquire shard lock and insert/check atomically (double-checked locking pattern)
    let shard = shard_index(dev, ino, aligned_offset);
    let mut guard = PAGE_CACHE_SHARDS[shard].lock();
    if let Some(entry) = guard.map.get(&(dev, ino, aligned_offset)) {
        // Someone else allocated and read it in the meantime!
        // Deallocate our frame and return the existing one.
        let phys_addr = entry.phys_addr;
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

    guard.map.insert(
        (dev, ino, aligned_offset),
        PageCacheEntry {
            phys_addr: phys,
            dirty: false,
        },
    );
    drop(guard);
    if DEBUG_PAGE_CACHE {
        crate::kprintln!("[page_cache] inserted into cache: phys={:#x}", phys);
    }

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

    // 1. Check if dirty and copy page info under shard lock
    let phys_to_write = {
        let shard = shard_index(dev, ino, aligned_offset);
        let guard = PAGE_CACHE_SHARDS[shard].lock();
        if let Some(entry) = guard.map.get(&(dev, ino, aligned_offset)) {
            if entry.dirty {
                Some(entry.phys_addr)
            } else {
                None
            }
        } else {
            None
        }
    };

    // 2. Perform write without lock
    if let Some(phys) = phys_to_write {
        let phys_offset = phys + crate::memory::r#virtual::phys_mem_offset();
        let src_slice = unsafe { core::slice::from_raw_parts(phys_offset as *const u8, 4096) };

        let size = inode.inode().size;
        if size > aligned_offset {
            let write_len = core::cmp::min(4096, (size - aligned_offset) as usize);
            inode
                .write_direct(aligned_offset, &src_slice[..write_len])
                .map_err(|_| Errno::EIO)?;
        }

        // 3. Clear dirty flag under shard lock
        let shard = shard_index(dev, ino, aligned_offset);
        let mut guard = PAGE_CACHE_SHARDS[shard].lock();
        if let Some(entry) = guard.map.get_mut(&(dev, ino, aligned_offset)) {
            // Only clear dirty if the physical page hasn't changed (it shouldn't have)
            if entry.phys_addr == phys {
                entry.dirty = false;
            }
        }
    }
    Ok(())
}

/// Flush all dirty pages for a given inode.
pub fn flush_all_for_inode(inode: &Arc<dyn InodeOps>) -> Result<(), Errno> {
    flush_all_for_inode_inner(&**inode)
}

/// Helper function implementing dirty page cache flushing for all pages of an inode using raw `&dyn InodeOps`.
pub fn flush_all_for_inode_inner(inode: &dyn InodeOps) -> Result<(), Errno> {
    let dev = inode.inode().dev;
    let ino = inode.inode().ino;

    // Collect Arc references to all active tasks under the read lock, then drop it immediately
    // to prevent cross-thread read-write lock deadlocks if a task exits/forks concurrently.
    let task_arcs: alloc::vec::Vec<(usize, Arc<spin::Mutex<crate::process::task::Task>>)> = {
        let tasks = crate::process::scheduler::TASKS.read();
        tasks
            .iter()
            .enumerate()
            .filter_map(|(idx, opt)| opt.as_ref().map(|arc| (idx, arc.clone())))
            .collect()
    };

    for (_idx, task_arc) in task_arcs {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let task = task_arc.lock();
            let addr_space = task.address_space.lock();
            for region in &addr_space.mmap_regions {
                if region.is_shared
                    && region
                        .inode
                        .as_ref()
                        .map(|i| (i.inode().dev, i.inode().ino))
                        == Some((dev, ino))
                {
                    let start_page = region.start & !4095;
                    let end_page = (region.start + region.len as u64 - 1) & !4095;
                    for vaddr in (start_page..=end_page).step_by(4096) {
                        let page_offset_in_mapping = vaddr - region.start;
                        let file_offset = region.offset + page_offset_in_mapping;

                        unsafe {
                            if let Some(pte) = get_page_table_entry(
                                addr_space.page_table_root,
                                VirtAddr::new(vaddr),
                            ) {
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
                }
            }
        });
    }

    let mut offsets = alloc::vec::Vec::new();
    for shard in &PAGE_CACHE_SHARDS {
        let guard = shard.lock();
        for (key, entry) in guard.map.iter() {
            if key.0 == dev && key.1 == ino && entry.dirty {
                offsets.push(key.2);
            }
        }
    }
    offsets.sort_unstable();
    offsets.dedup();

    for offset in offsets {
        flush_page_inner(inode, offset)?;
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
                let size = inode.inode().size;
                if size > aligned_file_offset {
                    let write_len = core::cmp::min(4096, (size - aligned_file_offset) as usize);
                    let _ = inode.write_direct(aligned_file_offset, &src_slice[..write_len]);
                }

                let shard = shard_index(dev, ino, aligned_file_offset);
                let mut guard = PAGE_CACHE_SHARDS[shard].lock();
                if let Some(entry) = guard.map.get_mut(&(dev, ino, aligned_file_offset)) {
                    if entry.phys_addr == phys {
                        entry.dirty = false;
                    }
                }
            }
        }
    }
}
