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

/// Global Page Cache mapping (inode_number, file_offset_aligned_to_4096) to a PageCacheEntry.
pub static PAGE_CACHE: TicketLock<BTreeMap<(u64, u64), PageCacheEntry>> =
    TicketLock::new(BTreeMap::new());

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

/// Helper function implementing page cache retrieval using raw `&dyn InodeOps`.
pub fn get_or_create_page_inner(inode: &dyn InodeOps, offset: u64) -> Result<u64, Errno> {
    let ino = inode.inode().ino;
    let aligned_offset = offset & !4095;

    // 1. Quick check under lock
    {
        let cache = PAGE_CACHE.lock();
        if let Some(entry) = cache.get(&(ino, aligned_offset)) {
            return Ok(entry.phys_addr);
        }
    }

    // 2. Allocate and read WITHOUT holding the lock
    crate::kprintln!(
        "[page_cache] allocating frame for ino={}, offset={:#x}",
        ino,
        aligned_offset
    );
    let phys = crate::memory::physical::allocate_frame().ok_or(Errno::ENOMEM)?;
    crate::kprintln!(
        "[page_cache] allocated frame: phys={:#x}, calling read_direct",
        phys
    );

    // Read 4096 bytes from the inode at aligned_offset using read_direct
    let phys_offset = phys + crate::memory::r#virtual::phys_mem_offset();
    let dest_slice = unsafe { core::slice::from_raw_parts_mut(phys_offset as *mut u8, 4096) };
    dest_slice.fill(0);

    let mut total_read = 0;
    while total_read < 4096 {
        crate::kprintln!(
            "[page_cache] read_direct offset={:#x}, remaining={}",
            aligned_offset + total_read as u64,
            4096 - total_read
        );
        match inode.read_direct(
            aligned_offset + total_read as u64,
            &mut dest_slice[total_read..],
        ) {
            Ok(0) => {
                crate::kprintln!("[page_cache] read_direct returned EOF");
                break;
            }
            Ok(n) => {
                crate::kprintln!("[page_cache] read_direct read {} bytes", n);
                total_read += n;
            }
            Err(e) => {
                crate::kprintln!("[page_cache] read_direct error: {}", e);
                crate::memory::physical::deallocate_frame(phys);
                return Err(Errno::EIO);
            }
        }
    }
    crate::kprintln!(
        "[page_cache] read_direct finished, total_read={}",
        total_read
    );

    // 3. Re-acquire lock and insert/check
    crate::kprintln!("[page_cache] acquiring lock for insert");
    let mut cache = PAGE_CACHE.lock();
    crate::kprintln!("[page_cache] acquired lock for insert");
    if let Some(entry) = cache.get(&(ino, aligned_offset)) {
        // Someone else allocated and read it in the meantime!
        // Deallocate our frame and return the existing one.
        let phys_addr = entry.phys_addr;
        drop(cache);
        crate::memory::physical::deallocate_frame(phys);
        crate::kprintln!(
            "[page_cache] already present in cache: phys={:#x}",
            phys_addr
        );
        return Ok(phys_addr);
    }

    cache.insert(
        (ino, aligned_offset),
        PageCacheEntry {
            phys_addr: phys,
            dirty: false,
        },
    );
    crate::kprintln!("[page_cache] inserted into cache: phys={:#x}", phys);

    Ok(phys)
}

/// Flush a dirty page cache frame back to disk using VFS writes.
pub fn flush_page(inode: &Arc<dyn InodeOps>, offset: u64) -> Result<(), Errno> {
    flush_page_inner(&**inode, offset)
}

/// Helper function implementing dirty page cache frame flushing using raw `&dyn InodeOps`.
pub fn flush_page_inner(inode: &dyn InodeOps, offset: u64) -> Result<(), Errno> {
    let ino = inode.inode().ino;
    let aligned_offset = offset & !4095;

    // 1. Check if dirty and copy page info under lock
    let phys_to_write = {
        let cache = PAGE_CACHE.lock();
        if let Some(entry) = cache.get(&(ino, aligned_offset)) {
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

        // 3. Clear dirty flag under lock
        let mut cache = PAGE_CACHE.lock();
        if let Some(entry) = cache.get_mut(&(ino, aligned_offset)) {
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
    let ino = inode.inode().ino;

    // First, scan page tables of all tasks to mark pages as dirty from shared mappings
    let tasks = crate::process::scheduler::TASKS.read();
    for task_opt in tasks.iter() {
        if let Some(task_arc) = task_opt {
            x86_64::instructions::interrupts::without_interrupts(|| {
                let mut task = task_arc.lock();
                let addr_space = task.address_space.lock();
                for region in &addr_space.mmap_regions {
                    if region.is_shared && region.inode.as_ref().map(|i| i.inode().ino) == Some(ino)
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
                                        let mut cache = PAGE_CACHE.lock();
                                        if let Some(entry) =
                                            cache.get_mut(&(ino, aligned_file_offset))
                                        {
                                            entry.dirty = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }
    }

    let mut offsets = alloc::vec::Vec::new();
    {
        let cache = PAGE_CACHE.lock();
        for (key, entry) in cache.iter() {
            if key.0 == ino && entry.dirty {
                offsets.push(key.1);
            }
        }
    }

    for offset in offsets {
        flush_page_inner(inode, offset)?;
    }
    Ok(())
}

/// Mark a page cache page as dirty.
pub fn mark_dirty(ino: u64, offset: u64) {
    let aligned_offset = offset & !4095;
    let mut cache = PAGE_CACHE.lock();
    if let Some(entry) = cache.get_mut(&(ino, aligned_offset)) {
        entry.dirty = true;
    }
}
