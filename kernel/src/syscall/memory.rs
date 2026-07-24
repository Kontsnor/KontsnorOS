//! Memory management syscalls — mmap, munmap, brk, mprotect.

use super::{Errno, SyscallResult};
use crate::kprintln;

/// Helper to convert POSIX prot flags to x86_64 PageTableFlags.
fn prot_to_page_flags(prot: i32) -> x86_64::structures::paging::PageTableFlags {
    use x86_64::structures::paging::PageTableFlags;
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if (prot & 2) != 0 {
        // PROT_WRITE
        flags |= PageTableFlags::WRITABLE;
    }
    if (prot & 4) == 0 {
        // NOT PROT_EXEC
        flags |= PageTableFlags::NO_EXECUTE;
    }
    flags
}

/// `mmap(addr, length, prot, flags, fd, offset)` — Map memory.
///
/// Creates a new anonymous or file-backed private mapping in the virtual address space of the calling process.
pub fn sys_mmap(
    addr: u64,
    length: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: i64,
) -> SyscallResult {
    use crate::process::fd as proc_fd;
    use crate::process::scheduler;
    use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    if length == 0 {
        return Errno::EINVAL.into();
    }

    // We support anonymous private mappings and private/shared file mappings
    let is_anon = (flags & 0x20) != 0 || fd == -1;
    let is_shared = (flags & 0x01) != 0;

    let file_desc = if !is_anon {
        match proc_fd::current_task_get_file_desc(fd) {
            Some(d) => Some(d),
            None => return Errno::EBADF.into(),
        }
    } else {
        None
    };

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    // Align length up to page size
    let aligned_len = match length.checked_add(4095) {
        Some(len) => len & !4095,
        None => return Errno::EINVAL.into(),
    };

    // Get current mmap_bump and page table root
    let (resolved_addr, page_table_root) = {
        let task_arc = match scheduler::get_task_arc(current_pid) {
            Some(t) => t,
            None => return Errno::ESRCH.into(),
        };
        let mut task = task_arc.lock();
        let mut addr_space = task.address_space.lock();

        let resolved = if addr == 0 {
            let current_bump = addr_space.mmap_bump;
            let next_bump = match current_bump.checked_add(aligned_len as u64) {
                Some(b) => b,
                None => return Errno::EINVAL.into(),
            };
            if next_bump > 0x0000_7FFF_FFFF_FFFF {
                return Errno::EINVAL.into();
            }
            addr_space.mmap_bump = next_bump;
            current_bump
        } else {
            let aligned_addr = match addr.checked_add(4095) {
                Some(a) => a & !4095,
                None => return Errno::EINVAL.into(),
            };
            let end_addr = match aligned_addr.checked_add(aligned_len as u64) {
                Some(end) => end,
                None => return Errno::EINVAL.into(),
            };
            if end_addr > 0x0000_7FFF_FFFF_FFFF {
                return Errno::EINVAL.into();
            }
            if end_addr > addr_space.mmap_bump {
                addr_space.mmap_bump = end_addr;
            }
            aligned_addr
        };

        (resolved, addr_space.page_table_root)
    };

    // Unmap any existing overlapping regions/pages in this range
    let _ = sys_munmap(resolved_addr, aligned_len);

    // Add to task's mmap_regions
    {
        let task_arc = match scheduler::get_task_arc(current_pid) {
            Some(t) => t,
            None => return Errno::ESRCH.into(),
        };
        let mut task = task_arc.lock();
        let mut addr_space = task.address_space.lock();
        addr_space
            .mmap_regions
            .push(crate::process::task::MappedRegion {
                start: resolved_addr,
                len: aligned_len,
                inode: file_desc.as_ref().map(|d| d.inode.clone()),
                offset: offset as u64,
                is_shared,
                prot,
                pathname: file_desc.as_ref().and_then(|d| d.path.clone()),
            });
    }

    crate::arch::x86_64::smp::shootdown_tlb();
    resolved_addr as SyscallResult
}

/// `munmap(addr, length)` — Unmap memory.
pub fn sys_munmap(addr: u64, length: usize) -> SyscallResult {
    use crate::process::scheduler;
    use x86_64::structures::paging::{Page, Size4KiB};
    use x86_64::VirtAddr;

    if length == 0 || (addr & 4095) != 0 {
        return Errno::EINVAL.into(); // addr must be page-aligned
    }

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let page_table_root = {
        let task = task_arc.lock();
        let pt_root = task.address_space.lock().page_table_root;
        pt_root
    };

    let aligned_len = match length.checked_add(4095) {
        Some(len) => len & !4095,
        None => return Errno::EINVAL.into(),
    };
    let end_addr = match addr.checked_add(aligned_len as u64) {
        Some(end) => end,
        None => return Errno::EINVAL.into(),
    };
    if end_addr > 0x0000_7FFF_FFFF_FFFF {
        return Errno::EINVAL.into();
    }
    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
    let end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(end_addr - 1));

    let mut unmapped_count = 0;
    for page in Page::range_inclusive(start_page, end_page) {
        let result = unsafe {
            crate::memory::r#virtual::unmap_user_page_no_shootdown(page_table_root, page)
        };

        if let Ok(phys_addr) = result {
            crate::memory::physical::deallocate_frame(phys_addr);
            unmapped_count += 1;
        }
    }

    // unmap and remove/shrink task.mmap_regions
    {
        let mut task = task_arc.lock();
        let mut addr_space = task.address_space.lock();
        let mut new_regions = alloc::vec::Vec::new();
        let unmap_start = addr;
        let unmap_end = addr + aligned_len as u64;

        for r in addr_space.mmap_regions.iter() {
            let r_start = r.start;
            let r_end = r.start + r.len as u64;

            if r_end <= unmap_start || r_start >= unmap_end {
                new_regions.push(r.clone());
            } else {
                if r_start < unmap_start {
                    new_regions.push(crate::process::task::MappedRegion {
                        start: r_start,
                        len: (unmap_start - r_start) as usize,
                        inode: r.inode.clone(),
                        offset: r.offset,
                        is_shared: r.is_shared,
                        prot: r.prot,
                        pathname: r.pathname.clone(),
                    });
                }
                if r_end > unmap_end {
                    let diff = unmap_end - r_start;
                    new_regions.push(crate::process::task::MappedRegion {
                        start: unmap_end,
                        len: (r_end - unmap_end) as usize,
                        inode: r.inode.clone(),
                        offset: r.offset + diff,
                        is_shared: r.is_shared,
                        prot: r.prot,
                        pathname: r.pathname.clone(),
                    });
                }
            }
        }
        addr_space.mmap_regions = new_regions;
    }

    if unmapped_count > 0 {
        crate::arch::x86_64::smp::shootdown_tlb();
    }

    kprintln!(
        "[syscall] munmap: successfully unmapped {} pages",
        unmapped_count
    );
    0 // Success
}

/// `mprotect(addr, length, prot)` — Change memory protections.
pub fn sys_mprotect(addr: u64, length: usize, prot: i32) -> SyscallResult {
    use crate::process::scheduler;
    use x86_64::structures::paging::{Page, Size4KiB};
    use x86_64::VirtAddr;

    kprintln!(
        "[syscall] mprotect(addr={:#x}, len={}, prot={:#x})",
        addr,
        length,
        prot
    );

    if length == 0 || (addr & 4095) != 0 {
        return Errno::EINVAL.into();
    }

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let page_table_root = {
        let task = task_arc.lock();
        let pt_root = task.address_space.lock().page_table_root;
        pt_root
    };

    let aligned_len = match length.checked_add(4095) {
        Some(len) => len & !4095,
        None => return Errno::EINVAL.into(),
    };
    let end_addr = match addr.checked_add(aligned_len as u64) {
        Some(end) => end,
        None => return Errno::EINVAL.into(),
    };
    if end_addr > 0x0000_7FFF_FFFF_FFFF {
        return Errno::EINVAL.into();
    }
    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
    let end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(end_addr - 1));

    // 1. Verify that the entire range is covered by existing mappings
    {
        let task = task_arc.lock();
        let addr_space = task.address_space.lock();
        for page in Page::range_inclusive(start_page, end_page) {
            let page_addr = page.start_address().as_u64();
            let is_covered = addr_space
                .mmap_regions
                .iter()
                .any(|r| page_addr >= r.start && page_addr < r.start + r.len as u64);
            if !is_covered {
                kprintln!(
                    "[mprotect] ENOMEM: address {:#x} not covered. Regions: {:?}",
                    page_addr,
                    addr_space.mmap_regions
                );
                return Errno::ENOMEM.into(); // Gap in mapping
            }
        }
    }

    // 2. Split and update task.mmap_regions
    {
        let mut task = task_arc.lock();
        let mut addr_space = task.address_space.lock();
        let mut new_regions = alloc::vec::Vec::new();
        let mprotect_start = addr;
        let mprotect_end = addr + aligned_len as u64;

        for r in addr_space.mmap_regions.iter() {
            let r_start = r.start;
            let r_end = r.start + r.len as u64;

            if r_end <= mprotect_start || r_start >= mprotect_end {
                new_regions.push(r.clone());
            } else {
                // Left non-overlapping part
                if r_start < mprotect_start {
                    new_regions.push(crate::process::task::MappedRegion {
                        start: r_start,
                        len: (mprotect_start - r_start) as usize,
                        inode: r.inode.clone(),
                        offset: r.offset,
                        is_shared: r.is_shared,
                        prot: r.prot,
                        pathname: r.pathname.clone(),
                    });
                }
                // Overlapping part (gets new protection flags)
                let overlap_start = core::cmp::max(r_start, mprotect_start);
                let overlap_end = core::cmp::min(r_end, mprotect_end);
                let diff = overlap_start - r_start;
                new_regions.push(crate::process::task::MappedRegion {
                    start: overlap_start,
                    len: (overlap_end - overlap_start) as usize,
                    inode: r.inode.clone(),
                    offset: r.offset + diff,
                    is_shared: r.is_shared,
                    prot, // new protection flags
                    pathname: r.pathname.clone(),
                });
                // Right non-overlapping part
                if r_end > mprotect_end {
                    let diff_end = mprotect_end - r_start;
                    new_regions.push(crate::process::task::MappedRegion {
                        start: mprotect_end,
                        len: (r_end - mprotect_end) as usize,
                        inode: r.inode.clone(),
                        offset: r.offset + diff_end,
                        is_shared: r.is_shared,
                        prot: r.prot,
                        pathname: r.pathname.clone(),
                    });
                }
            }
        }
        addr_space.mmap_regions = new_regions;
    }

    // 3. Update active page table entries if present
    let mut updated_count = 0;
    for page in Page::range_inclusive(start_page, end_page) {
        let page_addr = page.start_address().as_u64();
        // Determine is_shared for this page's region
        let is_shared = {
            let task = task_arc.lock();
            let addr_space = task.address_space.lock();
            addr_space
                .mmap_regions
                .iter()
                .find(|r| page_addr >= r.start && page_addr < r.start + r.len as u64)
                .map(|r| r.is_shared)
                .unwrap_or(false)
        };

        // Get the PTE for this page
        let pte_opt = unsafe {
            crate::memory::page_cache::get_page_table_entry(page_table_root, page.start_address())
        };

        if let Some(pte) = pte_opt {
            if !pte.is_unused() {
                use x86_64::structures::paging::PageTableFlags;
                let mut pte_flags = pte.flags();

                // Handle WRITABLE and BIT_9 (COW)
                if (prot & 2) != 0 {
                    if is_shared {
                        pte_flags.insert(PageTableFlags::WRITABLE);
                        pte_flags.remove(PageTableFlags::BIT_9);
                    } else {
                        // Private mapping: if it is already mapped writable, keep it writable
                        if !pte_flags.contains(PageTableFlags::WRITABLE) {
                            pte_flags.insert(PageTableFlags::BIT_9);
                        }
                    }
                } else {
                    pte_flags.remove(PageTableFlags::WRITABLE);
                    pte_flags.remove(PageTableFlags::BIT_9);
                }

                // Handle NO_EXECUTE
                if (prot & 5) != 0 {
                    pte_flags.remove(PageTableFlags::NO_EXECUTE);
                } else {
                    pte_flags.insert(PageTableFlags::NO_EXECUTE);
                }

                let addr = pte.addr();
                pte.set_addr(addr, pte_flags);
                updated_count += 1;
            }
        }
    }

    if updated_count > 0 {
        crate::arch::x86_64::smp::shootdown_tlb();
    }

    0 // Success
}

/// `brk(addr)` — Change data segment size.
///
/// Sets the end of the data segment (the program break).
/// Used by malloc implementations. Delegates to the process syscall
/// implementation which handles per-task heap tracking and page mapping.
pub fn sys_brk(addr: u64) -> SyscallResult {
    crate::syscall::process::sys_brk(addr)
}

/// `mremap(old_address, old_size, new_size, flags, new_address)` — Resize virtual memory mapping.
pub fn sys_mremap(
    old_address: u64,
    old_size: usize,
    new_size: usize,
    flags: i32,
    new_address: u64,
) -> SyscallResult {
    use crate::process::scheduler;
    use x86_64::VirtAddr;

    kprintln!(
        "[syscall] mremap(old_addr={:#x}, old_size={}, new_size={}, flags={:#x}, new_addr={:#x})",
        old_address,
        old_size,
        new_size,
        flags,
        new_address
    );

    if (old_address & 4095) != 0 {
        return Errno::EINVAL.into();
    }

    let old_size_aligned = match old_size.checked_add(4095) {
        Some(s) => s & !4095,
        None => return Errno::EINVAL.into(),
    };
    let new_size_aligned = match new_size.checked_add(4095) {
        Some(s) => s & !4095,
        None => return Errno::EINVAL.into(),
    };

    if old_size_aligned == 0 || new_size_aligned == 0 {
        return Errno::EINVAL.into();
    }

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };

    // Constants
    const MREMAP_MAYMOVE: i32 = 1;
    const MREMAP_FIXED: i32 = 2;

    if new_size_aligned < old_size_aligned {
        // Shrinking
        let shrink_addr = old_address + new_size_aligned as u64;
        let shrink_len = old_size_aligned - new_size_aligned;
        let res = sys_munmap(shrink_addr, shrink_len);
        if res < 0 {
            return res;
        }
        return old_address as SyscallResult;
    }

    if new_size_aligned > old_size_aligned {
        // Check if we can grow in-place
        let mut can_grow_inplace = true;
        let grow_start = old_address + old_size_aligned as u64;
        let grow_end = old_address + new_size_aligned as u64;

        {
            let task = task_arc.lock();
            let addr_space = task.address_space.lock();
            for r in addr_space.mmap_regions.iter() {
                let covers_old = old_address >= r.start
                    && old_address + old_size_aligned as u64 <= r.start + r.len as u64;
                if covers_old {
                    continue;
                }
                let r_end = r.start + r.len as u64;
                if !(r_end <= grow_start || r.start >= grow_end) {
                    can_grow_inplace = false;
                    break;
                }
            }
        }

        if can_grow_inplace {
            let mut found = false;
            let task = task_arc.lock();
            let mut addr_space = task.address_space.lock();
            for r in addr_space.mmap_regions.iter_mut() {
                if old_address >= r.start
                    && old_address + old_size_aligned as u64 <= r.start + r.len as u64
                {
                    let offset_in_region = old_address - r.start;
                    let new_region_len = offset_in_region as usize + new_size_aligned;
                    if r.start + new_region_len as u64 > r.start + r.len as u64 {
                        let old_end = r.start + r.len as u64;
                        r.len = new_region_len;
                        let new_end = r.start + r.len as u64;
                        if old_end == addr_space.mmap_bump {
                            addr_space.mmap_bump = new_end;
                        }
                    }
                    found = true;
                    break;
                }
            }
            if found {
                return old_address as SyscallResult;
            }
        }

        // Cannot grow in-place, must move mapping
        if (flags & MREMAP_MAYMOVE) == 0 {
            return Errno::ENOMEM.into();
        }

        let mut target_addr = 0;

        if (flags & MREMAP_FIXED) != 0 {
            if (new_address & 4095) != 0 {
                return Errno::EINVAL.into();
            }
            if !(new_address + new_size_aligned as u64 <= old_address
                || new_address >= old_address + old_size_aligned as u64)
            {
                return Errno::EINVAL.into();
            }
            target_addr = new_address;
        } else {
            // Allocate from bump allocator
            let task = task_arc.lock();
            let mut addr_space = task.address_space.lock();
            let current_bump = addr_space.mmap_bump;
            let next_bump = match current_bump.checked_add(new_size_aligned as u64) {
                Some(b) => b,
                None => return Errno::ENOMEM.into(),
            };
            if next_bump > 0x0000_7FFF_FFFF_FFFF {
                return Errno::ENOMEM.into();
            }
            addr_space.mmap_bump = next_bump;
            target_addr = current_bump;
        }

        if (flags & MREMAP_FIXED) != 0 {
            let res = sys_munmap(target_addr, new_size_aligned);
            if res < 0 {
                return res;
            }
        }

        // Retrieve properties of the old region
        let mut old_region_prot = 3;
        let mut old_region_inode = None;
        let mut old_region_offset = 0;
        let mut old_region_is_shared = false;
        let mut old_region_pathname = None;
        let mut found_old = false;

        {
            let task = task_arc.lock();
            let addr_space = task.address_space.lock();
            for r in addr_space.mmap_regions.iter() {
                if old_address >= r.start
                    && old_address + old_size_aligned as u64 <= r.start + r.len as u64
                {
                    old_region_prot = r.prot;
                    old_region_inode = r.inode.clone();
                    old_region_offset = r.offset + (old_address - r.start);
                    old_region_is_shared = r.is_shared;
                    old_region_pathname = r.pathname.clone();
                    found_old = true;
                    break;
                }
            }
        }

        if !found_old {
            return Errno::EFAULT.into();
        }

        let page_table_root = {
            let task = task_arc.lock();
            let root = task.address_space.lock().page_table_root;
            root
        };

        // Move page table entries
        for offset in (0..old_size_aligned).step_by(4096) {
            let old_page_va = old_address + offset as u64;
            let new_page_va = target_addr + offset as u64;

            // SAFETY: Walking page tables requires a valid page_table_root.
            let pte_opt = unsafe {
                crate::memory::page_cache::get_page_table_entry(
                    page_table_root,
                    VirtAddr::new(old_page_va),
                )
            };

            if let Some(pte) = pte_opt {
                if !pte.is_unused() {
                    let phys = pte.addr().as_u64();
                    let flags = pte.flags();

                    let new_page = x86_64::structures::paging::Page::<
                        x86_64::structures::paging::Size4KiB,
                    >::containing_address(VirtAddr::new(
                        new_page_va,
                    ));
                    let frame = x86_64::structures::paging::PhysFrame::containing_address(
                        x86_64::PhysAddr::new(phys),
                    );

                    // SAFETY: Mapping the valid frame to the new address is safe since the parameters are valid.
                    let map_res = unsafe {
                        crate::memory::r#virtual::map_user_page_no_shootdown(
                            page_table_root,
                            new_page,
                            frame,
                            flags,
                        )
                    };

                    if map_res.is_ok() {
                        let old_page = x86_64::structures::paging::Page::<
                            x86_64::structures::paging::Size4KiB,
                        >::containing_address(VirtAddr::new(
                            old_page_va,
                        ));
                        // SAFETY: Unmapping the old page under task page table root is safe as we just mapped it to the new location.
                        let _ = unsafe {
                            crate::memory::r#virtual::unmap_user_page_no_shootdown(
                                page_table_root,
                                old_page,
                            )
                        };
                    }
                }
            }
        }

        // Clean up the old mapping
        let _ = sys_munmap(old_address, old_size_aligned);

        // Add the new moved/grown region to mmap_regions
        {
            let task = task_arc.lock();
            let mut addr_space = task.address_space.lock();
            addr_space
                .mmap_regions
                .push(crate::process::task::MappedRegion {
                    start: target_addr,
                    len: new_size_aligned,
                    inode: old_region_inode,
                    offset: old_region_offset,
                    is_shared: old_region_is_shared,
                    prot: old_region_prot,
                    pathname: old_region_pathname,
                });
        }

        crate::arch::x86_64::smp::shootdown_tlb();
        return target_addr as SyscallResult;
    }

    old_address as SyscallResult
}

/// `madvise(addr, length, advice)` — Give advice about use of memory.
pub fn sys_madvise(addr: u64, length: usize, advice: i32) -> SyscallResult {
    use crate::process::scheduler;
    use x86_64::structures::paging::{Page, Size4KiB};
    use x86_64::VirtAddr;

    kprintln!(
        "[syscall] madvise(addr={:#x}, len={}, advice={})",
        addr,
        length,
        advice
    );

    if (addr & 4095) != 0 {
        return Errno::EINVAL.into();
    }

    if length == 0 {
        return 0; // Success
    }

    let aligned_len = match length.checked_add(4095) {
        Some(len) => len & !4095,
        None => return Errno::EINVAL.into(),
    };

    let end_addr = match addr.checked_add(aligned_len as u64) {
        Some(end) => end,
        None => return Errno::EINVAL.into(),
    };
    if end_addr > 0x0000_7FFF_FFFF_FFFF {
        return Errno::EINVAL.into();
    }

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };

    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
    let end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(end_addr - 1));

    // Verify that the entire range is covered by existing mappings
    {
        let task = task_arc.lock();
        let addr_space = task.address_space.lock();
        for page in Page::range_inclusive(start_page, end_page) {
            let page_addr = page.start_address().as_u64();
            let is_covered = addr_space
                .mmap_regions
                .iter()
                .any(|r| page_addr >= r.start && page_addr < r.start + r.len as u64);
            if !is_covered {
                return Errno::ENOMEM.into();
            }
        }
    }

    const MADV_DONTNEED: i32 = 4;
    if advice == MADV_DONTNEED {
        let page_table_root = {
            let task = task_arc.lock();
            let pt_root = task.address_space.lock().page_table_root;
            pt_root
        };

        let mut unmapped_count = 0;
        for page in Page::range_inclusive(start_page, end_page) {
            // SAFETY: unmapping within a valid process's page table is safe.
            let result = unsafe {
                crate::memory::r#virtual::unmap_user_page_no_shootdown(page_table_root, page)
            };

            if let Ok(phys_addr) = result {
                crate::memory::physical::deallocate_frame(phys_addr);
                unmapped_count += 1;
            }
        }

        if unmapped_count > 0 {
            crate::arch::x86_64::smp::shootdown_tlb();
        }
    }

    0 // Success
}
