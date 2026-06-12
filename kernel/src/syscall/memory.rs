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
    use x86_64::structures::paging::{Page, PhysFrame, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    kprintln!(
        "[syscall] mmap(addr={:#x}, len={}, prot={:#x}, flags={:#x}, fd={})",
        addr,
        length,
        prot,
        flags,
        fd
    );

    if length == 0 {
        return Errno::EINVAL.into();
    }

    // We support anonymous private mappings and private file mappings
    let is_anon = (flags & 0x20) != 0 || fd == -1;

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

        let resolved = if addr == 0 {
            let current_bump = task.mmap_bump;
            let next_bump = match current_bump.checked_add(aligned_len as u64) {
                Some(b) => b,
                None => return Errno::EINVAL.into(),
            };
            if next_bump > 0x0000_7FFF_FFFF_FFFF {
                return Errno::EINVAL.into();
            }
            task.mmap_bump = next_bump;
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
            aligned_addr
        };

        (resolved, task.page_table_root)
    };

    // Map pages in the resolved range
    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(resolved_addr));
    let end_page =
        Page::<Size4KiB>::containing_address(VirtAddr::new(resolved_addr + aligned_len as u64 - 1));

    let page_flags = prot_to_page_flags(prot);

    for page in Page::range_inclusive(start_page, end_page) {
        if let Some(phys) = crate::memory::physical::allocate_frame() {
            let frame = PhysFrame::containing_address(PhysAddr::new(phys));

            // Map the page
            let _ = unsafe {
                crate::memory::r#virtual::map_user_page_no_shootdown(
                    page_table_root,
                    page,
                    frame,
                    page_flags,
                )
            };

            // Write content
            let dest = (phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            let dest_slice = unsafe { core::slice::from_raw_parts_mut(dest, 4096) };
            dest_slice.fill(0);

            if let Some(ref desc) = file_desc {
                let page_file_offset =
                    offset as u64 + (page.start_address().as_u64() - resolved_addr);
                let _ = desc.inode.read(page_file_offset, dest_slice);
            }
        } else {
            // Eager rollback on OOM
            let unmap_end_page = page;
            for unmap_page in Page::range(start_page, unmap_end_page) {
                if let Ok(phys_addr) = unsafe {
                    crate::memory::r#virtual::unmap_user_page_no_shootdown(
                        page_table_root,
                        unmap_page,
                    )
                } {
                    crate::memory::physical::deallocate_frame(phys_addr);
                }
            }
            crate::arch::x86_64::smp::shootdown_tlb();
            return Errno::ENOMEM.into();
        }
    }

    crate::arch::x86_64::smp::shootdown_tlb();
    resolved_addr as SyscallResult
}

/// `munmap(addr, length)` — Unmap memory.
pub fn sys_munmap(addr: u64, length: usize) -> SyscallResult {
    use crate::process::scheduler;
    use x86_64::structures::paging::{Page, Size4KiB};
    use x86_64::VirtAddr;

    kprintln!("[syscall] munmap(addr={:#x}, len={})", addr, length);

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
    let page_table_root = task_arc.lock().page_table_root;

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
    let page_table_root = task_arc.lock().page_table_root;

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

    let flags = prot_to_page_flags(prot);

    let mut updated_count = 0;
    for page in Page::range_inclusive(start_page, end_page) {
        unsafe {
            if let Err(_) = crate::memory::r#virtual::update_user_page_flags_no_shootdown(
                page_table_root,
                page,
                flags,
            ) {
                if updated_count > 0 {
                    crate::arch::x86_64::smp::shootdown_tlb();
                }
                return Errno::ENOMEM.into();
            }
        }
        updated_count += 1;
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
