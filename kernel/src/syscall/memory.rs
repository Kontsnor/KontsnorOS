//! Memory management syscalls — mmap, munmap, brk.

use super::{Errno, SyscallResult};
use crate::kprintln;

/// `mmap(addr, length, prot, flags, fd, offset)` — Map memory.
///
/// Creates a new anonymous private mapping in the virtual address space of the calling process.
pub fn sys_mmap(
    addr: u64,
    length: usize,
    _prot: i32,
    flags: i32,
    fd: i32,
    _offset: i64,
) -> SyscallResult {
    use crate::process::scheduler;
    use x86_64::structures::paging::{Page, PhysFrame, PageTableFlags, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    kprintln!("[syscall] mmap(addr={:#x}, len={}, flags={:#x}, fd={})", addr, length, flags, fd);

    if length == 0 {
        return Errno::EINVAL.into();
    }

    // We only support anonymous private mappings for now
    let is_anon = (flags & 0x20) != 0 || fd == -1;
    if !is_anon {
        kprintln!("[syscall] mmap: non-anonymous file mappings not supported");
        return Errno::ENOSYS.into();
    }

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    // Align length up to page size
    let aligned_len = (length + 4095) & !4095;

    // Get current mmap_bump and page table root
    let (resolved_addr, page_table_root) = {
        let mut sched_lock = scheduler::SCHEDULER.lock();
        let scheduler = match sched_lock.as_mut() {
            Some(s) => s,
            None => return Errno::ESRCH.into(),
        };
        let task = match scheduler.get_task_mut(current_pid) {
            Some(t) => t,
            None => return Errno::ESRCH.into(),
        };

        let resolved = if addr == 0 {
            let current_bump = task.mmap_bump;
            task.mmap_bump += aligned_len as u64;
            current_bump
        } else {
            (addr + 4095) & !4095 // align user-requested address
        };

        (resolved, task.page_table_root)
    };

    // Map pages in the resolved range
    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(resolved_addr));
    let end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(resolved_addr + aligned_len as u64 - 1));

    for page in Page::range_inclusive(start_page, end_page) {
        if let Some(phys) = crate::memory::physical::allocate_frame() {
            let frame = PhysFrame::containing_address(PhysAddr::new(phys));
            let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
            
            // Map the page
            let _ = unsafe {
                crate::memory::r#virtual::map_user_page(page_table_root, page, frame, flags)
            };

            // Zero the physical frame
            let dest = (phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            unsafe {
                core::ptr::write_bytes(dest, 0, 4096);
            }
        } else {
            // Eager rollback on OOM
            let unmap_end_page = page;
            for unmap_page in Page::range(start_page, unmap_end_page) {
                if let Ok(phys_addr) = unsafe { crate::memory::r#virtual::unmap_user_page(page_table_root, unmap_page) } {
                    crate::memory::physical::deallocate_frame(phys_addr);
                }
            }
            return Errno::ENOMEM.into();
        }
    }

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

    let page_table_root = {
        let sched_lock = scheduler::SCHEDULER.lock();
        let scheduler = match sched_lock.as_ref() {
            Some(s) => s,
            None => return Errno::ESRCH.into(),
        };
        let task = match scheduler.get_task(current_pid) {
            Some(t) => t,
            None => return Errno::ESRCH.into(),
        };
        task.page_table_root
    };

    let aligned_len = (length + 4095) & !4095;
    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
    let end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr + aligned_len as u64 - 1));

    let mut unmapped_count = 0;
    for page in Page::range_inclusive(start_page, end_page) {
        let result = unsafe {
            crate::memory::r#virtual::unmap_user_page(page_table_root, page)
        };

        if let Ok(phys_addr) = result {
            crate::memory::physical::deallocate_frame(phys_addr);
            unmapped_count += 1;
        }
    }

    kprintln!("[syscall] munmap: successfully unmapped {} pages", unmapped_count);
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
