//! Process management syscalls — fork, exec, exit, wait, getpid, brk.

use super::{Errno, SyscallResult};
use crate::kprintln;
use crate::process::scheduler;
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{Page, PhysFrame, PageTableFlags, Size4KiB};

/// `getpid()` — Get the process ID of the calling process.
pub fn sys_getpid() -> SyscallResult {
    match crate::process::scheduler::current_pid() {
        Some(pid) => pid.as_u64() as SyscallResult,
        None => 0,
    }
}

/// `fork()` — Create a child process.
///
/// Duplicates the current user address space, fd_table and CpuContext.
/// Returns:
/// - In the parent: PID of the child
/// - In the child: 0
/// - On error: negative errno
pub fn sys_fork(regs: *mut crate::syscall::SavedRegisters) -> SyscallResult {
    use crate::process::{pid, scheduler, task::Task};

    kprintln!("[syscall] fork()");

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let parent_cr3 = {
        let sched = scheduler::SCHEDULER.lock();
        if let Some(ref sched) = *sched {
            if let Some(task) = sched.get_task(current_pid) {
                task.page_table_root
            } else {
                return Errno::ESRCH.into();
            }
        } else {
            return Errno::ESRCH.into();
        }
    };

    // Create a cloned user page table from the parent's page table root
    let child_page_table = match crate::memory::r#virtual::clone_parent_page_table(parent_cr3) {
        Ok(pt) => pt,
        Err(_) => return Errno::ENOMEM.into(),
    };

    // Clone parent's user stack physically to give the child its own independent stack space!
    {
        use x86_64::structures::paging::Translate;
        let stack_size = 64 * 1024;
        let stack_bottom = crate::process::elf::USER_STACK_TOP - stack_size;
        let stack_start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(stack_bottom));
        let stack_end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(crate::process::elf::USER_STACK_TOP - 1));

        for page in Page::range_inclusive(stack_start_page, stack_end_page) {
            // Allocate a new physical frame for the child stack page
            let child_phys_addr = match crate::memory::physical::allocate_frame() {
                Some(addr) => addr,
                None => return Errno::ENOMEM.into(),
            };

            // Translate the parent's virtual stack page address to get its physical frame
            let parent_phys_addr = {
                let mapper = unsafe { crate::memory::r#virtual::active_page_table() };
                mapper.translate_addr(page.start_address())
                    .map(|p| p.as_u64())
            };

            // If the page is mapped in the parent, copy its contents; otherwise zero-initialize
            let dest_ptr = (child_phys_addr + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            let dest_slice = unsafe { core::slice::from_raw_parts_mut(dest_ptr, 4096) };

            if let Some(parent_phys) = parent_phys_addr {
                let src_ptr = (parent_phys + crate::memory::r#virtual::phys_mem_offset()) as *const u8;
                let src_slice = unsafe { core::slice::from_raw_parts(src_ptr, 4096) };
                dest_slice.copy_from_slice(src_slice);
            } else {
                dest_slice.fill(0);
            }

            // Map the page in the child's page table pointing to the new physical frame
            // Build flags: PRESENT, WRITABLE, USER_ACCESSIBLE, NO_EXECUTE
            let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
            
            unsafe {
                // Unmap the page from child's page table (since clone_parent_page_table cloned it pointing to parent's frame)
                let _ = crate::memory::r#virtual::unmap_user_page(child_page_table, page);
                
                crate::memory::r#virtual::map_user_page(
                    child_page_table,
                    page,
                    PhysFrame::containing_address(PhysAddr::new(child_phys_addr)),
                    flags
                ).expect("Failed to map cloned user stack page");
            }
        }
    }

    // ── Allocate new PID and build child TCB ──────────────────────────────────
    let child_pid = pid::allocate();

    let mut child_task = Task::new(child_pid, alloc::format!("fork:{}", child_pid), child_page_table);

    // Acquire lock and clone parent task properties directly (avoids allocating massive arrays on stack)
    {
        let mut sched = scheduler::SCHEDULER.lock();
        if let Some(ref mut sched) = *sched {
            if let Some(parent_task) = sched.get_task(current_pid) {
                child_task.fd_table = parent_task.fd_table.clone();
                child_task.fd_offsets = parent_task.fd_offsets.clone();
                child_task.sigactions = parent_task.sigactions.clone();
                child_task.blocked_signals = parent_task.blocked_signals;
                child_task.brk = parent_task.brk;
                child_task.cwd = parent_task.cwd.clone();
                child_task.mmap_bump = parent_task.mmap_bump;
            }
        }
    }
    child_task.pending_signals = 0; // Fork clears pending signals
    child_task.parent_pid = current_pid;

    // Allocate a 32 KiB kernel stack for the child
    let layout = alloc::alloc::Layout::from_size_align(32768, 16).unwrap();
    let kstack_base = unsafe { alloc::alloc::alloc(layout) } as u64;
    child_task.kernel_stack_base = kstack_base;
    child_task.kernel_stack_size = 32768;

    // Copy the parent's SavedRegisters to the top of the child's kernel stack.
    // SavedRegisters size is 128 bytes, which is 16-byte aligned.
    let child_regs_ptr = (kstack_base + 32768 - 128) as *mut crate::syscall::SavedRegisters;
    unsafe {
        core::ptr::write(child_regs_ptr, *regs);
    }

    use crate::process::context::CpuContext;
    let child_context = CpuContext::new(
        crate::process::context::fork_child_return as *const () as u64,
        child_regs_ptr as u64,
        child_page_table,
    );

    crate::kprintln!("[syscall] fork debug: rip = {:#x}, rsp = {:#x}, cr3 = {:#x}", 
        child_context.rip, child_context.rsp, child_context.cr3);

    child_task.context = child_context;


    scheduler::add_task(child_task);

    kprintln!("[syscall] fork() -> parent returns child PID {}", child_pid);
    child_pid.as_u64() as SyscallResult
}

/// `execve(pathname, argv, envp)` — Execute a program.
///
/// Loads a new ELF binary from the VFS, replacing the current process image.
/// On success this function does not return — it jumps directly into Ring 3.
/// `execve(pathname, argv, envp)` — Execute a program.
///
/// Loads a new ELF binary from the VFS, replacing the current process image.
/// On success this function does not return — it jumps directly into Ring 3.
pub fn sys_execve(pathname: *const u8, _argv: *const *const u8, _envp: *const *const u8) -> SyscallResult {
    // Copy the path string from user-space memory
    let path = unsafe {
        super::fs::copy_string_from_user_pub(pathname)
    };
    let path = match path {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    // Copy argv and envp arrays from user-space before changing address spaces
    let argv = match unsafe { crate::process::elf::copy_argv_from_user(_argv) } {
        Some(a) => a,
        None => return Errno::EFAULT.into(),
    };

    let envp = match unsafe { crate::process::elf::copy_argv_from_user(_envp) } {
        Some(e) => e,
        None => return Errno::EFAULT.into(),
    };

    kprintln!("[syscall] execve(\"{}\") with {} args, {} env vars", path, argv.len(), envp.len());

    // Look up the file in the VFS
    let inode = match crate::fs::vfs::lookup(&path) {
        Some(i) => i,
        None => {
            kprintln!("[syscall] execve: file not found: {}", path);
            return Errno::ENOENT.into();
        }
    };

    // Read the ELF binary
    let file_size = inode.inode().size as usize;
    if file_size == 0 {
        return Errno::ENOEXEC.into();
    }
    let mut elf_buf = alloc::vec![0u8; file_size];
    match inode.read(0, &mut elf_buf) {
        Ok(_) => {},
        Err(e) => return e as SyscallResult,
    }

    // Parse the ELF
    let elf_info = match crate::process::elf::parse_elf(&elf_buf) {
        Ok(e) => e,
        Err(_) => return Errno::ENOEXEC.into(),
    };

    // Create a fresh user page table
    let new_page_table = match crate::memory::r#virtual::create_user_page_table() {
        Ok(pt) => pt,
        Err(_) => return Errno::ENOMEM.into(),
    };

    // Map and load ELF segments
    use x86_64::structures::paging::{Page, PhysFrame, PageTableFlags, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    let mut max_vaddr = 0;
    for segment in &elf_info.segments {
        if segment.mem_size == 0 { continue; }

        let end = segment.vaddr + segment.mem_size;
        if end > max_vaddr {
            max_vaddr = end;
        }

        let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(segment.vaddr));
        let end_page   = Page::<Size4KiB>::containing_address(VirtAddr::new(segment.vaddr + segment.mem_size - 1));

        for page in Page::range_inclusive(start_page, end_page) {
            let phys = match crate::memory::physical::allocate_frame() {
                Some(p) => p,
                None => return Errno::ENOMEM.into(),
            };
            let frame = PhysFrame::containing_address(PhysAddr::new(phys));
            let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            if segment.flags.write   { flags |= PageTableFlags::WRITABLE; }
            if !segment.flags.execute { flags |= PageTableFlags::NO_EXECUTE; }

            unsafe {
                if crate::memory::r#virtual::map_user_page(new_page_table, page, frame, flags).is_err() {
                    return Errno::ENOMEM.into();
                }
            }

            // Copy segment data
            let dest = (phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            let dest_slice = unsafe { core::slice::from_raw_parts_mut(dest, 4096) };
            dest_slice.fill(0);

            let page_va = page.start_address().as_u64();
            let seg_start = segment.vaddr;
            let _seg_end  = segment.vaddr + segment.file_size;

            let page_offset_in_seg = if page_va > seg_start { page_va - seg_start } else { 0 };
            let seg_offset_in_page = if page_va < seg_start { seg_start - page_va } else { 0 };

            if page_offset_in_seg < segment.file_size {
                let copy_len = core::cmp::min(
                    4096 - seg_offset_in_page,
                    segment.file_size - page_offset_in_seg,
                );
                let src_start = (segment.file_offset + page_offset_in_seg) as usize;
                let dst_start = seg_offset_in_page as usize;
                dest_slice[dst_start..dst_start + copy_len as usize]
                    .copy_from_slice(&elf_buf[src_start..src_start + copy_len as usize]);
            }
        }
    }

    let initial_brk = (max_vaddr + 4095) & !4095;

    // Map user stack
    let stack_size: u64 = 64 * 1024;
    let stack_bottom = crate::process::elf::USER_STACK_TOP - stack_size;
    let stack_start = Page::<Size4KiB>::containing_address(VirtAddr::new(stack_bottom));
    let stack_end   = Page::<Size4KiB>::containing_address(VirtAddr::new(crate::process::elf::USER_STACK_TOP - 1));
    let mut highest_stack_phys = 0;
    for page in Page::range_inclusive(stack_start, stack_end) {
        let phys = match crate::memory::physical::allocate_frame() {
            Some(p) => p,
            None => return Errno::ENOMEM.into(),
        };
        if page.start_address().as_u64() == crate::process::elf::USER_STACK_TOP - 4096 {
            highest_stack_phys = phys;
        }
        let frame = PhysFrame::containing_address(PhysAddr::new(phys));
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
        unsafe {
            if crate::memory::r#virtual::map_user_page(new_page_table, page, frame, flags).is_err() {
                return Errno::ENOMEM.into();
            }
        }
    }

    // Construct System V ABI compliant stack
    let user_sp = match crate::process::elf::construct_user_stack(&argv, &envp, highest_stack_phys) {
        Ok(sp) => sp,
        Err(e) => return e.into(),
    };

    let entry = elf_info.entry_point;

    // Reset signal state and update page table root for execve
    {
        let current_pid = match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        };
        let mut sched_lock = scheduler::SCHEDULER.lock();
        if let Some(ref mut sched) = *sched_lock {
            if let Some(task) = sched.get_task_mut(current_pid) {
                for action in task.sigactions.iter_mut() {
                    if action.sa_handler != 1 { // If not SIG_IGN
                        *action = crate::process::task::SigAction::default();
                    }
                }
                task.pending_signals = 0;
                task.page_table_root = new_page_table;
                task.brk = initial_brk; // Dynamically calculated start of heap
            }
        }
    }

    kprintln!("[syscall] execve: loading OK, entry={:#x}, jumping to Ring 3...", entry);

    // Switch to the new address space and enter Ring 3 (never returns)
    unsafe {
        crate::process::context::enter_user_mode(entry, user_sp, new_page_table);
    }
}

/// `exit(status)` — Terminate the calling process.
pub fn sys_exit(status: i32) -> SyscallResult {
    kprintln!("[syscall] exit(status={})", status);
    crate::process::scheduler::exit_current_thread(status);
}

/// `wait4(pid, wstatus, options, rusage)` — Wait for a child process.
///
/// Cooperatively yields until a zombie child is found, then reaps it.
pub fn sys_wait4(pid: i32, wstatus: *mut i32, _options: i32, _rusage: *mut u8) -> SyscallResult {
    use crate::process::{scheduler, task::TaskState};

    if !wstatus.is_null() && !crate::syscall::fs::validate_user_ptr(wstatus as *const u8, core::mem::size_of::<i32>()) {
        return Errno::EFAULT.into();
    }

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    kprintln!("[syscall] wait4(pid={})", pid);

    loop {
        // Scan for a zombie child
        let result = {
            let mut sched = scheduler::SCHEDULER.lock();
            let sched = match sched.as_mut() {
                Some(s) => s,
                None => return Errno::ECHILD.into(),
            };

            let mut found = None;
            for slot in sched.tasks.iter_mut() {
                if let Some(task) = slot {
                    let is_child = task.parent_pid == current_pid;
                    let matches_pid = pid == -1 || task.pid.as_u64() as i32 == pid;
                    if is_child && matches_pid && task.state == TaskState::Zombie {
                        crate::kprintln!("[syscall] wait4: found zombie child PID {}", task.pid);
                        found = Some((task.pid, task.exit_code.unwrap_or(0)));
                        break;
                    }
                }
            }

            if let Some((child_pid, exit_code)) = found {
                // Write exit status to user-space if wstatus is non-null
                if !wstatus.is_null() {
                    unsafe { wstatus.write_volatile((exit_code & 0xFF) << 8); }
                }
                // Remove the zombie from the task list
                let idx = child_pid.as_u64() as usize;
                if let Some(slot) = sched.tasks.get_mut(idx) {
                    *slot = None;
                }
                Some(child_pid.as_u64() as SyscallResult)
            } else {
                None
            }
        };

        if let Some(ret) = result {
            return ret;
        }

        // No zombie child yet — yield and retry
        scheduler::yield_now();
    }
}

/// `brk(addr)` — Set the program break (end of data segment / heap top).
///
/// If `addr` is 0, returns the current break. Otherwise extends the heap
/// by mapping new pages up to `addr`.
pub fn sys_brk(addr: u64) -> SyscallResult {
    use crate::process::scheduler;
    use x86_64::structures::paging::{Page, PhysFrame, PageTableFlags, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    // Read current brk and page table root
    let (current_brk, page_table_root) = {
        let sched = scheduler::SCHEDULER.lock();
        let sched = match sched.as_ref() {
            Some(s) => s,
            None => return Errno::ESRCH.into(),
        };
        let task = match sched.get_task(current_pid) {
            Some(t) => t,
            None => return Errno::ESRCH.into(),
        };
        (task.brk, task.page_table_root)
    };

    if addr == 0 || addr <= current_brk {
        return current_brk as SyscallResult;
    }

    let old_brk = current_brk;
    let new_brk  = (addr + 4095) & !4095; // page-align up

    // Map pages from old_brk to new_brk
    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(old_brk));
    let end_page   = Page::<Size4KiB>::containing_address(VirtAddr::new(new_brk - 1));

    for page in Page::range_inclusive(start_page, end_page) {
        if let Some(phys) = crate::memory::physical::allocate_frame() {
            let frame = PhysFrame::containing_address(PhysAddr::new(phys));
            let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
            let _ = unsafe {
                crate::memory::r#virtual::map_user_page(page_table_root, page, frame, flags)
            };
            // Zero the new page
            let dest = (phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            unsafe { core::ptr::write_bytes(dest, 0, 4096); }
        } else {
            return Errno::ENOMEM.into();
        }
    }

    // Update the task's brk
    {
        let mut sched = scheduler::SCHEDULER.lock();
        if let Some(ref mut sched) = *sched {
            if let Some(task) = sched.get_task_mut(current_pid) {
                task.brk = new_brk;
            }
        }
    }

    new_brk as SyscallResult
}

/// `getuid()` — Get real user ID.
pub fn sys_getuid() -> SyscallResult { 0 }

/// `getgid()` — Get real group ID.
pub fn sys_getgid() -> SyscallResult { 0 }

/// `geteuid()` — Get effective user ID.
pub fn sys_geteuid() -> SyscallResult { 0 }

/// `getegid()` — Get effective group ID.
pub fn sys_getegid() -> SyscallResult { 0 }

/// `arch_prctl()` — Set thread base register (FS_BASE).
pub fn sys_arch_prctl(code: i32, addr: u64) -> SyscallResult {
    if code == 0x1002 { // ARCH_SET_FS
        x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(addr));
        0
    } else {
        Errno::EINVAL.into()
    }
}

/// `set_tid_address()` — Set thread ID pointer.
pub fn sys_set_tid_address(_tidptr: *mut i32) -> SyscallResult {
    let pid = crate::process::scheduler::current_pid().map(|p| p.as_u64()).unwrap_or(0);
    pid as i64
}
