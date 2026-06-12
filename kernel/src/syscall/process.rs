//! Process management syscalls — fork, exec, exit, wait, getpid, brk.

use super::{Errno, SyscallResult};
use crate::kprintln;
use crate::process::scheduler;

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

    let parent_cr3 = match scheduler::get_task_arc(current_pid) {
        Some(task_arc) => task_arc.lock().page_table_root,
        None => return Errno::ESRCH.into(),
    };

    // Create a cloned user page table from the parent's page table root
    let child_page_table = match crate::memory::r#virtual::clone_parent_page_table(parent_cr3) {
        Ok(pt) => pt,
        Err(_) => return Errno::ENOMEM.into(),
    };

    // ── Allocate new PID and build child TCB ──────────────────────────────────
    let child_pid = pid::allocate();

    let mut child_task = Task::new(
        child_pid,
        alloc::format!("fork:{}", child_pid),
        child_page_table,
    );

    // Acquire lock and clone parent task properties directly (avoids allocating massive arrays on stack)
    {
        if let Some(parent_task_arc) = scheduler::get_task_arc(current_pid) {
            let parent_task = parent_task_arc.lock();
            child_task.fd_table = parent_task.fd_table.clone();
            for slot in &child_task.fd_table {
                if let Some(ref file_desc) = slot {
                    *file_desc.ref_count.lock() += 1;
                }
            }
            child_task.sigactions = parent_task.sigactions.clone();
            child_task.blocked_signals = parent_task.blocked_signals;
            child_task.brk = parent_task.brk;
            child_task.cwd = parent_task.cwd.clone();
            child_task.mmap_bump = parent_task.mmap_bump;
        } else {
            return Errno::ESRCH.into();
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
    let mut child_context = CpuContext::new(
        crate::process::context::fork_child_return as *const () as u64,
        child_regs_ptr as u64,
        child_page_table,
    );
    child_context.fs_base = x86_64::registers::model_specific::FsBase::read().as_u64();
    child_context.kernel_gs_base =
        unsafe { x86_64::registers::model_specific::Msr::new(0xC0000102).read() };

    debug_assert_eq!(child_context.rbx, 0);
    debug_assert_eq!(child_context.rbp, 0);
    debug_assert_eq!(child_context.r12, 0);
    debug_assert_eq!(child_context.r13, 0);
    debug_assert_eq!(child_context.r14, 0);
    debug_assert_eq!(child_context.r15, 0);

    crate::kprintln!("[syscall] fork debug: rip = {:#x}, rsp = {:#x}, cr3 = {:#x}, fs_base = {:#x}, gs_base = {:#x}", 
        child_context.rip, child_context.rsp, child_context.cr3, child_context.fs_base, child_context.kernel_gs_base);

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
pub fn sys_execve(
    pathname: *const u8,
    _argv: *const *const u8,
    _envp: *const *const u8,
) -> SyscallResult {
    // Copy the path string from user-space memory
    let path = unsafe { super::fs::copy_string_from_user_pub(pathname) };
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

    kprintln!(
        "[syscall] execve(\"{}\") with {} args, {} env vars",
        path,
        argv.len(),
        envp.len()
    );

    // Look up the file in the VFS
    let inode = match crate::fs::vfs::lookup_follow(&path, true) {
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
        Ok(_) => {}
        Err(e) => return e as SyscallResult,
    }

    let mut path = path;
    let mut argv = argv;
    let mut loop_count = 0;

    while elf_buf.starts_with(b"#!") {
        loop_count += 1;
        if loop_count > 4 {
            kprintln!("[syscall] execve: shebang loop limit exceeded");
            return Errno::ELOOP.into();
        }

        // Find the first line
        let first_line = match elf_buf.split(|&b| b == b'\n').next() {
            Some(line) => line,
            None => &elf_buf,
        };

        // Parse interpreter and optional argument
        let content = &first_line[2..];

        let mut parts = content
            .split(|&b| b == b' ' || b == b'\t')
            .filter(|part| !part.is_empty());
        let interp_bytes = match parts.next() {
            Some(i) => i,
            None => return Errno::ENOEXEC.into(),
        };
        let interp_str = match core::str::from_utf8(interp_bytes) {
            Ok(s) => s,
            Err(_) => return Errno::ENOEXEC.into(),
        };

        let mut new_argv = alloc::vec![alloc::string::String::from(interp_str)];

        // Find if there is an optional argument.
        // Skip interpreter in content
        let offset =
            interp_bytes.as_ptr() as usize - content.as_ptr() as usize + interp_bytes.len();
        let mut rest = &content[offset..];
        // Trim leading and trailing spaces/tabs/newlines
        while rest.starts_with(b" ") || rest.starts_with(b"\t") {
            rest = &rest[1..];
        }
        while rest.ends_with(b" ")
            || rest.ends_with(b"\t")
            || rest.ends_with(b"\r")
            || rest.ends_with(b"\n")
        {
            rest = &rest[..rest.len() - 1];
        }
        if !rest.is_empty() {
            let opt_arg = match core::str::from_utf8(rest) {
                Ok(s) => alloc::string::String::from(s),
                Err(_) => return Errno::ENOEXEC.into(),
            };
            new_argv.push(opt_arg);
        }

        // Add the script path as the next argument
        new_argv.push(path.clone());

        // Add the original script arguments (excluding argv[0])
        for arg in argv.iter().skip(1) {
            new_argv.push(arg.clone());
        }

        path = alloc::string::String::from(interp_str);
        argv = new_argv;

        // Load the interpreter file
        let interp_inode = match crate::fs::vfs::lookup_follow(&path, true) {
            Some(i) => i,
            None => {
                kprintln!("[syscall] execve: interpreter not found: {}", path);
                return Errno::ENOENT.into();
            }
        };

        let interp_size = interp_inode.inode().size as usize;
        if interp_size == 0 {
            return Errno::ENOEXEC.into();
        }
        let mut new_buf = alloc::vec![0u8; interp_size];
        match interp_inode.read(0, &mut new_buf) {
            Ok(_) => {
                elf_buf = new_buf;
            }
            Err(e) => return e as SyscallResult,
        }
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

    use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    let mut max_vaddr = 0;
    for segment in &elf_info.segments {
        if segment.mem_size == 0 {
            continue;
        }

        let end = segment.vaddr + segment.mem_size;
        if end > max_vaddr {
            max_vaddr = end;
        }

        let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(segment.vaddr));
        let end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(
            segment.vaddr + segment.mem_size - 1,
        ));

        for page in Page::range_inclusive(start_page, end_page) {
            let phys = match crate::memory::physical::allocate_frame() {
                Some(p) => p,
                None => return Errno::ENOMEM.into(),
            };
            let frame = PhysFrame::containing_address(PhysAddr::new(phys));
            let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            if segment.flags.write {
                flags |= PageTableFlags::WRITABLE;
            }
            if !segment.flags.execute {
                flags |= PageTableFlags::NO_EXECUTE;
            }

            unsafe {
                if crate::memory::r#virtual::map_user_page_no_shootdown(
                    new_page_table,
                    page,
                    frame,
                    flags,
                )
                .is_err()
                {
                    return Errno::ENOMEM.into();
                }
            }

            // Copy segment data
            let dest = (phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            let dest_slice = unsafe { core::slice::from_raw_parts_mut(dest, 4096) };
            dest_slice.fill(0);

            let page_va = page.start_address().as_u64();
            let seg_start = segment.vaddr;
            let _seg_end = segment.vaddr + segment.file_size;

            let page_offset_in_seg = if page_va > seg_start {
                page_va - seg_start
            } else {
                0
            };
            let seg_offset_in_page = if page_va < seg_start {
                seg_start - page_va
            } else {
                0
            };

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
    let stack_end = Page::<Size4KiB>::containing_address(VirtAddr::new(
        crate::process::elf::USER_STACK_TOP - 1,
    ));
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
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;
        unsafe {
            if crate::memory::r#virtual::map_user_page_no_shootdown(
                new_page_table,
                page,
                frame,
                flags,
            )
            .is_err()
            {
                return Errno::ENOMEM.into();
            }
        }
    }

    // Construct System V ABI compliant stack
    let user_sp = match crate::process::elf::construct_user_stack(
        &argv,
        &envp,
        highest_stack_phys,
        elf_info.entry_point,
        elf_info.phdr,
        elf_info.phnum,
        elf_info.phent,
    ) {
        Ok(sp) => sp,
        Err(e) => return e.into(),
    };

    let entry = elf_info.entry_point;

    // Reset signal state and update page table root for execve
    let old_page_table = {
        let current_pid = match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        };
        if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
            let mut task = task_arc.lock();
            for action in task.sigactions.iter_mut() {
                if action.sa_handler != 1 {
                    // If not SIG_IGN
                    *action = crate::process::task::SigAction::default();
                }
            }
            task.pending_signals = 0;
            let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
            unsafe {
                if apic_id < 32 {
                    crate::syscall::CPU_SCRATCHES[apic_id].signals_pending = 0;
                }
            }
            // Close O_CLOEXEC file descriptors (F-12)
            for slot in task.fd_table.iter_mut() {
                let mut close = false;
                if let Some(ref fd) = slot {
                    if fd.flags.lock().0 & crate::fs::file::OpenFlags::O_CLOEXEC != 0 {
                        close = true;
                    }
                }
                if close {
                    if let Some(desc) = slot.take() {
                        let mut rc = desc.ref_count.lock();
                        if *rc > 0 {
                            *rc -= 1;
                        }
                    }
                }
            }

            let old = task.page_table_root;
            task.page_table_root = new_page_table;
            task.brk = initial_brk; // Dynamically calculated start of heap
            task.context.fs_base = 0; // Clear TLS base for new process
            old
        } else {
            0
        }
    };

    if old_page_table != 0 && old_page_table != crate::memory::r#virtual::kernel_pml4_phys() {
        // Switch to the new page table first so the CPU is no longer using the old one.
        unsafe {
            x86_64::registers::control::Cr3::write(
                x86_64::structures::paging::PhysFrame::containing_address(x86_64::PhysAddr::new(
                    new_page_table,
                )),
                x86_64::registers::control::Cr3Flags::empty(),
            );
        }
        // Now safely free all resources of the old page table
        let _ = crate::memory::r#virtual::free_user_page_table(old_page_table);
    }

    kprintln!(
        "[syscall] execve: loading OK, entry={:#x}, jumping to Ring 3...",
        entry
    );

    // Clear the active CPU FS_BASE register to prevent inheriting parent's TLS
    x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(0));

    // Switch to the new address space and enter Ring 3 (never returns)
    unsafe {
        crate::process::context::enter_user_mode(
            entry,
            user_sp,
            new_page_table,
            (crate::arch::x86_64::gdt::user_code_selector().0 | 3) as u64,
            (crate::arch::x86_64::gdt::user_data_selector().0 | 3) as u64,
        );
    }
}

/// `exit(status)` — Terminate the calling process.
pub fn sys_exit(status: i32) -> SyscallResult {
    kprintln!("[syscall] exit(status={})", status);
    crate::process::scheduler::exit_current_thread(status);
}

/// `exit_group(status)` — Terminate all threads in the thread group.
pub fn sys_exit_group(status: i32) -> SyscallResult {
    sys_exit(status)
}

/// `wait4(pid, wstatus, options, rusage)` — Wait for a child process.
///
/// Cooperatively yields until a zombie child is found, then reaps it.
pub fn sys_wait4(pid: i32, wstatus: *mut i32, _options: i32, _rusage: *mut u8) -> SyscallResult {
    use crate::process::{scheduler, task::TaskState};

    if !wstatus.is_null()
        && !crate::syscall::fs::validate_user_ptr(wstatus as *const u8, core::mem::size_of::<i32>())
    {
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
            let tasks = scheduler::TASKS.read();
            let mut found = None;
            let mut has_children = false;
            for slot in tasks.iter() {
                if let Some(task_arc) = slot {
                    let task = task_arc.lock();
                    let is_child = task.parent_pid == current_pid;
                    let matches_pid = pid == -1 || task.pid.as_u64() as i32 == pid;
                    if is_child && matches_pid {
                        has_children = true;
                        if task.state == TaskState::Zombie {
                            crate::kprintln!(
                                "[syscall] wait4: found zombie child PID {}",
                                task.pid
                            );
                            found = Some((task.pid, task.exit_code.unwrap_or(0)));
                            break;
                        }
                    }
                }
            }
            drop(tasks);

            if let Some((child_pid, exit_code)) = found {
                // Write exit status to user-space if wstatus is non-null
                if !wstatus.is_null() {
                    unsafe {
                        wstatus.write_volatile((exit_code & 0xFF) << 8);
                    }
                }
                // Remove the zombie from the task list
                let idx = child_pid.as_u64() as usize;
                let mut tasks_write = scheduler::TASKS.write();
                if let Some(slot) = tasks_write.get_mut(idx) {
                    *slot = None;
                }
                Some(Ok(child_pid.as_u64() as SyscallResult))
            } else if !has_children {
                Some(Err(Errno::ECHILD))
            } else {
                None
            }
        };

        match result {
            Some(Ok(ret)) => return ret,
            Some(Err(err)) => return err.into(),
            None => {
                // Sleep on our child_wait_queue until a child exits
                let task_arc = match scheduler::get_task_arc(current_pid) {
                    Some(t) => t,
                    None => return Errno::ESRCH.into(),
                };
                let wait_queue = task_arc.lock().child_wait_queue.clone();
                wait_queue.wait();
            }
        }
    }
}

/// `brk(addr)` — Set the program break (end of data segment / heap top).
///
/// If `addr` is 0, returns the current break. Otherwise extends the heap
/// by mapping new pages up to `addr`.
pub fn sys_brk(addr: u64) -> SyscallResult {
    use crate::process::scheduler;
    use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    // Read current brk and page table root
    let (current_brk, page_table_root) = {
        if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
            let task = task_arc.lock();
            (task.brk, task.page_table_root)
        } else {
            return Errno::ESRCH.into();
        }
    };

    if addr == 0 || addr <= current_brk {
        return current_brk as SyscallResult;
    }

    let old_brk = current_brk;
    let new_brk = (addr + 4095) & !4095; // page-align up

    // Map pages from old_brk to new_brk
    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(old_brk));
    let end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(new_brk - 1));

    let mut mapped_count = 0;
    for page in Page::range_inclusive(start_page, end_page) {
        if let Some(phys) = crate::memory::physical::allocate_frame() {
            let frame = PhysFrame::containing_address(PhysAddr::new(phys));
            let flags = PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE
                | PageTableFlags::NO_EXECUTE;
            let _ = unsafe {
                crate::memory::r#virtual::map_user_page_no_shootdown(
                    page_table_root,
                    page,
                    frame,
                    flags,
                )
            };
            // Zero the new page
            let dest = (phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            unsafe {
                core::ptr::write_bytes(dest, 0, 4096);
            }
            mapped_count += 1;
        } else {
            if mapped_count > 0 {
                crate::arch::x86_64::smp::shootdown_tlb();
            }
            return Errno::ENOMEM.into();
        }
    }

    if mapped_count > 0 {
        crate::arch::x86_64::smp::shootdown_tlb();
    }

    // Update the task's brk
    {
        if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
            task_arc.lock().brk = new_brk;
        }
    }

    new_brk as SyscallResult
}

/// `getuid()` — Get real user ID.
pub fn sys_getuid() -> SyscallResult {
    0
}

/// `getgid()` — Get real group ID.
pub fn sys_getgid() -> SyscallResult {
    0
}

/// `geteuid()` — Get effective user ID.
pub fn sys_geteuid() -> SyscallResult {
    0
}

/// `getegid()` — Get effective group ID.
pub fn sys_getegid() -> SyscallResult {
    0
}

/// `arch_prctl()` — Set thread base register (FS_BASE).
pub fn sys_arch_prctl(code: i32, addr: u64) -> SyscallResult {
    kprintln!("[syscall] arch_prctl(code={:#x}, addr={:#x})", code, addr);
    if code == 0x1002 {
        // ARCH_SET_FS
        x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(addr));

        let current_pid = match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        };

        if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
            task_arc.lock().context.fs_base = addr;
        }
        0
    } else {
        Errno::EINVAL.into()
    }
}

/// `set_tid_address()` — Set thread ID pointer.
pub fn sys_set_tid_address(tidptr: *mut i32) -> SyscallResult {
    if !tidptr.is_null()
        && !super::fs::validate_user_ptr(tidptr as *const u8, core::mem::size_of::<i32>())
    {
        return Errno::EFAULT.into();
    }
    let pid = crate::process::scheduler::current_pid()
        .map(|p| p.as_u64())
        .unwrap_or(0);
    pid as i64
}

// ─────────────────────────────────────────────────────────────────────────────
// POSIX process identity / session syscalls (required by bash + musl-libc)
// ─────────────────────────────────────────────────────────────────────────────

/// `getppid()` — Return the parent PID of the calling process.
pub fn sys_getppid() -> SyscallResult {
    if let Some(pid) = scheduler::current_pid() {
        if let Some(task_arc) = scheduler::get_task_arc(pid) {
            return task_arc.lock().parent_pid.as_u64() as SyscallResult;
        }
    }
    0
}

/// `setpgid(pid, pgid)` — Set the process group ID of a process.
///
/// If pid == 0, set the calling process's pgid.
/// If pgid == 0, set pgid = pid.
pub fn sys_setpgid(pid: i32, pgid: i32) -> SyscallResult {
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        crate::process::pid::Pid::from_raw(pid as u64)
    };

    let new_pgid = if pgid == 0 {
        target_pid.as_u64()
    } else {
        pgid as u64
    };

    if let Some(task_arc) = scheduler::get_task_arc(target_pid) {
        task_arc.lock().pgid = new_pgid;
        return 0;
    }
    Errno::ESRCH.into()
}

/// `getpgid(pid)` — Get the process group ID of a process.
///
/// If pid == 0, returns the calling process's pgid.
pub fn sys_getpgid(pid: i32) -> SyscallResult {
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        crate::process::pid::Pid::from_raw(pid as u64)
    };

    if let Some(task_arc) = scheduler::get_task_arc(target_pid) {
        return task_arc.lock().pgid as SyscallResult;
    }
    Errno::ESRCH.into()
}

/// `setsid()` — Create a new session and set the process group ID.
///
/// The calling process becomes the session leader of a new session
/// with pgid == pid.
pub fn sys_setsid() -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        let mut task = task_arc.lock();
        task.pgid = current_pid.as_u64();
        return current_pid.as_u64() as SyscallResult;
    }
    Errno::ESRCH.into()
}

// ─────────────────────────────────────────────────────────────────────────────
// POSIX time / resource limit syscalls
// ─────────────────────────────────────────────────────────────────────────────

/// Linux `uname` struct (sys/utsname.h), each field is 65 bytes.
#[repr(C)]
struct UtsName {
    sysname: [u8; 65],
    nodename: [u8; 65],
    release: [u8; 65],
    version: [u8; 65],
    machine: [u8; 65],
    domainname: [u8; 65],
}

/// `uname(buf)` — Write kernel identity information into a `utsname` struct.
pub fn sys_uname(buf: *mut u8) -> SyscallResult {
    use super::fs::validate_user_ptr_write;

    if buf.is_null() {
        return Errno::EFAULT.into();
    }
    // UtsName is 6 × 65 = 390 bytes
    if validate_user_ptr_write(buf, core::mem::size_of::<UtsName>()).is_err() {
        return Errno::EFAULT.into();
    }

    let mut u = UtsName {
        sysname: [0u8; 65],
        nodename: [0u8; 65],
        release: [0u8; 65],
        version: [0u8; 65],
        machine: [0u8; 65],
        domainname: [0u8; 65],
    };

    // Helper: copy a &str into a fixed [u8;65], null-terminated.
    fn fill(dst: &mut [u8; 65], s: &[u8]) {
        let len = s.len().min(64);
        dst[..len].copy_from_slice(&s[..len]);
        dst[len] = 0;
    }

    fill(&mut u.sysname, b"Linux");
    fill(&mut u.nodename, b"kontsnoros");
    fill(&mut u.release, b"6.1.0-KontsnorOS");
    fill(&mut u.version, b"#1 SMP");
    fill(&mut u.machine, b"x86_64");
    fill(&mut u.domainname, b"(none)");

    unsafe {
        core::ptr::write(buf as *mut UtsName, u);
    }
    0
}

/// `timeval` struct used by `gettimeofday`.
#[repr(C)]
struct TimeVal {
    tv_sec: i64,
    tv_usec: i64,
}

/// `timezone` struct used by `gettimeofday`.
#[repr(C)]
struct TimeZone {
    tz_minuteswest: i32,
    tz_dsttime: i32,
}

/// `gettimeofday(tv, tz)` — Return current time-of-day.
///
/// We stub this to return a fixed point in time (epoch + 0).
/// Real wall-clock time requires an RTC driver.
pub fn sys_gettimeofday(tv: *mut u8, tz: *mut u8) -> SyscallResult {
    use super::fs::validate_user_ptr_write;
    if !tv.is_null() {
        if validate_user_ptr_write(tv, core::mem::size_of::<TimeVal>()).is_err() {
            return Errno::EFAULT.into();
        }
        let t = TimeVal {
            tv_sec: 0,
            tv_usec: 0,
        };
        unsafe {
            core::ptr::write(tv as *mut TimeVal, t);
        }
    }
    if !tz.is_null() {
        if validate_user_ptr_write(tz, core::mem::size_of::<TimeZone>()).is_err() {
            return Errno::EFAULT.into();
        }
        let z = TimeZone {
            tz_minuteswest: 0,
            tz_dsttime: 0,
        };
        unsafe {
            core::ptr::write(tz as *mut TimeZone, z);
        }
    }
    0
}

/// `timespec` struct used by `clock_gettime` and `nanosleep`.
#[repr(C)]
struct TimeSpec {
    tv_sec: i64,
    tv_nsec: i64,
}

/// `clock_gettime(clockid, tp)` — Return current clock value.
pub fn sys_clock_gettime(_clockid: i32, tp: *mut u8) -> SyscallResult {
    use super::fs::validate_user_ptr_write;
    if tp.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(tp, core::mem::size_of::<TimeSpec>()).is_err() {
        return Errno::EFAULT.into();
    }
    let ts = TimeSpec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        core::ptr::write(tp as *mut TimeSpec, ts);
    }
    0
}

/// `nanosleep(req, rem)` — High-resolution sleep.
///
/// We approximate with a scheduler yield. `rem` is zeroed on return.
pub fn sys_nanosleep(req: *const u8, rem: *mut u8) -> SyscallResult {
    use super::fs::{validate_user_ptr, validate_user_ptr_write};
    if !req.is_null() {
        if !validate_user_ptr(req, core::mem::size_of::<TimeSpec>()) {
            return Errno::EFAULT.into();
        }
    }
    // Yield to the scheduler (no real timer infrastructure yet).
    crate::process::scheduler::yield_now();
    if !rem.is_null() {
        if validate_user_ptr_write(rem, core::mem::size_of::<TimeSpec>()).is_err() {
            return Errno::EFAULT.into();
        }
        let ts = TimeSpec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            core::ptr::write(rem as *mut TimeSpec, ts);
        }
    }
    0
}

/// `tms` struct used by `times`.
#[repr(C)]
struct Tms {
    tms_utime: i64,
    tms_stime: i64,
    tms_cutime: i64,
    tms_cstime: i64,
}

/// `times(buf)` — Return process and children CPU usage times.
pub fn sys_times(buf: *mut u8) -> SyscallResult {
    use super::fs::validate_user_ptr_write;
    if !buf.is_null() {
        if validate_user_ptr_write(buf, core::mem::size_of::<Tms>()).is_err() {
            return Errno::EFAULT.into();
        }
        let t = Tms {
            tms_utime: 0,
            tms_stime: 0,
            tms_cutime: 0,
            tms_cstime: 0,
        };
        unsafe {
            core::ptr::write(buf as *mut Tms, t);
        }
    }
    0
}

/// `rlimit` struct used by `getrlimit`.
#[repr(C)]
struct RLimit {
    rlim_cur: u64, // soft limit
    rlim_max: u64, // hard limit
}

const RLIM_INFINITY: u64 = !0u64;

/// `getrlimit(resource, rlim)` — Get resource limits.
///
/// Returns sane defaults; KontsnorOS does not currently enforce limits.
pub fn sys_getrlimit(resource: i32, rlim: *mut u8) -> SyscallResult {
    use super::fs::validate_user_ptr_write;
    if rlim.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(rlim, core::mem::size_of::<RLimit>()).is_err() {
        return Errno::EFAULT.into();
    }

    // Provide generous defaults so bash/musl don't self-restrict.
    let limit = match resource {
        0 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_CPU
        1 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_FSIZE
        2 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_DATA
        3 => RLimit {
            rlim_cur: 8 * 1024 * 1024,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_STACK (8 MiB)
        4 => RLimit {
            rlim_cur: 0,
            rlim_max: 0,
        }, // RLIMIT_CORE (no core dumps)
        5 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_RSS
        6 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_NPROC
        7 => RLimit {
            rlim_cur: 1024,
            rlim_max: 4096,
        }, // RLIMIT_NOFILE
        8 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_MEMLOCK
        9 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_AS
        10 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_LOCKS
        _ => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        },
    };
    unsafe {
        core::ptr::write(rlim as *mut RLimit, limit);
    }
    0
}

/// `setrlimit(resource, rlim)` — Set resource limits (stub; KontsnorOS ignores limits).
pub fn sys_setrlimit(_resource: i32, _rlim: *const u8) -> SyscallResult {
    0 // Accept all limit changes silently.
}

/// `sysinfo` struct (linux/sysinfo.h).
#[repr(C)]
struct SysInfo {
    uptime: i64,
    loads: [u64; 3],
    totalram: u64,
    freeram: u64,
    sharedram: u64,
    bufferram: u64,
    totalswap: u64,
    freeswap: u64,
    procs: u16,
    pad: [u8; 22],
    totalhigh: u64,
    freehigh: u64,
    mem_unit: u32,
    _pad2: [u8; 8],
}

/// `sysinfo(info)` — Return overall system information.
pub fn sys_sysinfo(info: *mut u8) -> SyscallResult {
    use super::fs::validate_user_ptr_write;
    if info.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(info, core::mem::size_of::<SysInfo>()).is_err() {
        return Errno::EFAULT.into();
    }
    let si = SysInfo {
        uptime: 0,
        loads: [0, 0, 0],
        totalram: 128 * 1024 * 1024, // 128 MiB
        freeram: 64 * 1024 * 1024,   //  64 MiB
        sharedram: 0,
        bufferram: 0,
        totalswap: 0,
        freeswap: 0,
        procs: 1,
        pad: [0u8; 22],
        totalhigh: 0,
        freehigh: 0,
        mem_unit: 1,
        _pad2: [0u8; 8],
    };
    unsafe {
        core::ptr::write(info as *mut SysInfo, si);
    }
    0
}

/// `sigaltstack(ss, old_ss)` — Set/get alternate signal stack (stub).
pub fn sys_sigaltstack(_ss: *const u8, _old_ss: *mut u8) -> SyscallResult {
    0
}

/// `prctl(option, ...)` — Process control (stub).
///
/// Returns 0 for all recognized options bash uses (PR_SET_NAME, PR_GET_DUMPABLE, etc.).
pub fn sys_prctl(_option: i32, _arg2: u64, _arg3: u64, _arg4: u64, _arg5: u64) -> SyscallResult {
    0
}

/// `getrandom(buf, buflen, flags)` — Get random bytes.
pub fn sys_getrandom(buf: *mut u8, buflen: usize, _flags: u32) -> SyscallResult {
    use super::fs::validate_user_ptr;
    if buf.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(buf, buflen) {
        return Errno::EFAULT.into();
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf, buflen) };
    if !crate::crypto::prng::fill_bytes(slice) {
        return Errno::EAGAIN.into();
    }
    buflen as SyscallResult
}

/// `prlimit64(pid, resource, new_limit, old_limit)` — Get/set resource limits.
pub fn sys_prlimit64(
    _pid: i32,
    resource: i32,
    _new_limit: *const u8,
    old_limit: *mut u8,
) -> SyscallResult {
    if !old_limit.is_null() {
        let ret = sys_getrlimit(resource, old_limit);
        if ret < 0 {
            return ret;
        }
    }
    0
}

/// `gettid()` — Get thread ID (alias to getpid).
pub fn sys_gettid() -> SyscallResult {
    sys_getpid()
}

/// `tgkill(tgid, tid, sig)` — Send signal to thread.
pub fn sys_tgkill(_tgid: i32, tid: i32, sig: i32) -> SyscallResult {
    super::signal::sys_kill(tid, sig)
}

/// `clone(flags, child_stack, parent_tidptr, child_tidptr, newtls)`
pub fn sys_clone(
    flags: u64,
    child_stack: u64,
    parent_tidptr: *mut i32,
    child_tidptr: *mut i32,
    newtls: u64,
    regs: *mut crate::syscall::SavedRegisters,
) -> SyscallResult {
    use crate::process::{context::CpuContext, pid, scheduler, task::Task};

    kprintln!("[syscall] clone(flags={:#x}, child_stack={:#x}, parent_tid={:?}, child_tid={:?}, newtls={:#x})",
        flags, child_stack, parent_tidptr, child_tidptr, newtls);

    if child_stack != 0 && child_stack > 0x0000_7FFF_FFFF_FFFF {
        return Errno::EINVAL.into();
    }

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let parent_cr3 = match scheduler::get_task_arc(current_pid) {
        Some(task_arc) => task_arc.lock().page_table_root,
        None => return Errno::ESRCH.into(),
    };

    let child_page_table = match crate::memory::r#virtual::clone_parent_page_table(parent_cr3) {
        Ok(pt) => pt,
        Err(_) => return Errno::ENOMEM.into(),
    };

    let child_pid = pid::allocate();
    let mut child_task = Task::new(
        child_pid,
        alloc::format!("clone:{}", child_pid),
        child_page_table,
    );

    {
        if let Some(parent_task_arc) = scheduler::get_task_arc(current_pid) {
            let parent_task = parent_task_arc.lock();
            // TODO: F-13: Implement shared fd_table reference counting for CLONE_FILES.
            // Currently, CLONE_FILES is ignored and a deep clone of the fd_table is
            // always performed, which breaks POSIX thread fd sharing semantics (e.g.
            // close(fd) in one thread is not visible to the other thread).
            child_task.fd_table = parent_task.fd_table.clone();
            for slot in &child_task.fd_table {
                if let Some(ref file_desc) = slot {
                    *file_desc.ref_count.lock() += 1;
                }
            }
            child_task.sigactions = parent_task.sigactions.clone();
            child_task.blocked_signals = parent_task.blocked_signals;
            child_task.brk = parent_task.brk;
            child_task.cwd = parent_task.cwd.clone();
            child_task.mmap_bump = parent_task.mmap_bump;
        } else {
            return Errno::ESRCH.into();
        }
    }
    child_task.pending_signals = 0;
    child_task.parent_pid = current_pid;

    let layout = alloc::alloc::Layout::from_size_align(32768, 16).unwrap();
    let kstack_base = unsafe { alloc::alloc::alloc(layout) } as u64;
    child_task.kernel_stack_base = kstack_base;
    child_task.kernel_stack_size = 32768;

    let child_regs_ptr = (kstack_base + 32768 - 128) as *mut crate::syscall::SavedRegisters;
    unsafe {
        core::ptr::write(child_regs_ptr, *regs);
        if child_stack != 0 {
            (*child_regs_ptr).rsp = child_stack;
        }
    }

    let mut child_context = CpuContext::new(
        crate::process::context::fork_child_return as *const () as u64,
        child_regs_ptr as u64,
        child_page_table,
    );

    if flags & 0x00080000 != 0 {
        child_context.fs_base = newtls;
    } else {
        child_context.fs_base = x86_64::registers::model_specific::FsBase::read().as_u64();
    }
    child_context.kernel_gs_base =
        unsafe { x86_64::registers::model_specific::Msr::new(0xC0000102).read() };

    debug_assert_eq!(child_context.rbx, 0);
    debug_assert_eq!(child_context.rbp, 0);
    debug_assert_eq!(child_context.r12, 0);
    debug_assert_eq!(child_context.r13, 0);
    debug_assert_eq!(child_context.r14, 0);
    debug_assert_eq!(child_context.r15, 0);

    child_task.context = child_context;

    if flags & 0x00100000 != 0 && !parent_tidptr.is_null() {
        if super::fs::validate_user_ptr_write(parent_tidptr as *mut u8, core::mem::size_of::<i32>())
            .is_err()
        {
            return Errno::EFAULT.into();
        }
        unsafe {
            parent_tidptr.write_volatile(child_pid.as_u64() as i32);
        }
    }
    if flags & 0x01000000 != 0 && !child_tidptr.is_null() {
        if super::fs::validate_user_ptr_write(child_tidptr as *mut u8, core::mem::size_of::<i32>())
            .is_err()
        {
            return Errno::EFAULT.into();
        }
        unsafe {
            child_tidptr.write_volatile(child_pid.as_u64() as i32);
        }
    }

    scheduler::add_task(child_task);

    child_pid.as_u64() as SyscallResult
}
