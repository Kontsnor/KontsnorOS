//! Process lifecycle and scheduler/memory control system calls.

use super::super::{Errno, SyscallResult};
use super::creds::calculate_exec_creds;
use crate::kprintln;
use crate::process::scheduler;
use crate::syscall::fs::copy_string_from_user_pub;
use crate::syscall::validation::{validate_user_ptr, validate_user_ptr_write};

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

    let (parent_cr3, mmap_regions) = match scheduler::get_task_arc(current_pid) {
        Some(task_arc) => {
            let task = task_arc.lock();
            (task.page_table_root, task.mmap_regions.clone())
        }
        None => return Errno::ESRCH.into(),
    };

    // Create a cloned user page table from the parent's page table root
    let child_page_table =
        match crate::memory::r#virtual::clone_parent_page_table(parent_cr3, &mmap_regions) {
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
            child_task.mmap_regions = parent_task.mmap_regions.clone();
            child_task.uid = parent_task.uid;
            child_task.gid = parent_task.gid;
            child_task.euid = parent_task.euid;
            child_task.egid = parent_task.egid;
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

/// Helper to map loadable ELF segments into the user page table.
///
/// Returns the maximum virtual address mapped on success.
fn map_elf_segments(
    new_page_table: u64,
    segments: &[crate::process::elf::LoadSegment],
    elf_buf: &[u8],
    bias: u64,
) -> Result<u64, Errno> {
    use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    let mut max_vaddr = 0;
    for segment in segments {
        if segment.mem_size == 0 {
            continue;
        }

        let vaddr = segment.vaddr + bias;
        let end = vaddr + segment.mem_size;
        if end > max_vaddr {
            max_vaddr = end;
        }

        let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(vaddr));
        let end_page =
            Page::<Size4KiB>::containing_address(VirtAddr::new(vaddr + segment.mem_size - 1));

        for page in Page::range_inclusive(start_page, end_page) {
            let phys = match crate::memory::physical::allocate_frame() {
                Some(p) => p,
                None => return Err(Errno::ENOMEM),
            };
            let frame = PhysFrame::containing_address(PhysAddr::new(phys));
            let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            if segment.flags.write {
                flags |= PageTableFlags::WRITABLE;
            }
            if !segment.flags.execute {
                flags |= PageTableFlags::NO_EXECUTE;
            }

            // SAFETY: The new_page_table is a valid PML4 page table root constructed for user space.
            // The allocated physical frame is valid and page boundaries are respected.
            unsafe {
                if crate::memory::r#virtual::map_user_page_no_shootdown(
                    new_page_table,
                    page,
                    frame,
                    flags,
                )
                .is_err()
                {
                    return Err(Errno::ENOMEM);
                }
            }

            // Copy segment data
            let dest = (phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            // SAFETY: dest is a valid kernel virtual mapping of the newly allocated physical frame.
            let dest_slice = unsafe { core::slice::from_raw_parts_mut(dest, 4096) };
            dest_slice.fill(0);

            let page_va = page.start_address().as_u64();
            let seg_start = vaddr;

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
    Ok(max_vaddr)
}

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
    let path = unsafe { copy_string_from_user_pub(pathname) };
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

    // Verify execute permission on the executable file
    if let Err(e) = crate::fs::inode::check_permission(inode.inode(), crate::fs::inode::MAY_EXEC) {
        return e as SyscallResult;
    }

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

    let main_max_vaddr = match map_elf_segments(new_page_table, &elf_info.segments, &elf_buf, 0) {
        Ok(addr) => addr,
        Err(e) => return e as SyscallResult,
    };

    let initial_brk = (main_max_vaddr + 4095) & !4095;

    let mut entry = elf_info.entry_point;
    let mut interpreter_base = 0;
    if let Some(ref interp_path) = elf_info.interpreter {
        let interp_inode = match crate::fs::vfs::lookup_follow(interp_path, true) {
            Some(i) => i,
            None => {
                kprintln!("[syscall] execve: interpreter not found: {}", interp_path);
                return Errno::ENOENT.into();
            }
        };

        if let Err(e) =
            crate::fs::inode::check_permission(interp_inode.inode(), crate::fs::inode::MAY_EXEC)
        {
            return e as SyscallResult;
        }

        let interp_size = interp_inode.inode().size as usize;
        if interp_size == 0 {
            return Errno::ENOEXEC.into();
        }
        let mut interp_elf_buf = alloc::vec![0u8; interp_size];
        match interp_inode.read(0, &mut interp_elf_buf) {
            Ok(_) => {}
            Err(e) => return e as SyscallResult,
        }

        let interp_info = match crate::process::elf::parse_elf(&interp_elf_buf) {
            Ok(e) => e,
            Err(_) => return Errno::ENOEXEC.into(),
        };

        interpreter_base = 0x0000_7FFF_F7F0_0000;
        match map_elf_segments(
            new_page_table,
            &interp_info.segments,
            &interp_elf_buf,
            interpreter_base,
        ) {
            Ok(_) => {}
            Err(e) => return e as SyscallResult,
        }
        entry = interp_info.entry_point + interpreter_base;
    }

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
        // SAFETY: The page table root points to a valid PML4 page table structure and frame/page parameters are valid.
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
        interpreter_base,
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

            // Set-UID and Set-GID executable support
            let exec_mode = inode.inode().permissions.mode;
            let (new_euid, new_egid) = calculate_exec_creds(
                exec_mode,
                inode.inode().uid,
                inode.inode().gid,
                task.uid,
                task.gid,
            );
            task.euid = new_euid;
            task.egid = new_egid;

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
            // Close O_CLOEXEC file descriptors
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
            task.mmap_regions.clear();
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
    use crate::process::task::TaskState;

    if !wstatus.is_null() && !validate_user_ptr(wstatus as *const u8, core::mem::size_of::<i32>()) {
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
    if !tidptr.is_null() && !validate_user_ptr(tidptr as *const u8, core::mem::size_of::<i32>()) {
        return Errno::EFAULT.into();
    }
    let pid = scheduler::current_pid().map(|p| p.as_u64()).unwrap_or(0);
    pid as i64
}

/// `prctl(option, ...)` — Process control (stub).
pub fn sys_prctl(_option: i32, _arg2: u64, _arg3: u64, _arg4: u64, _arg5: u64) -> SyscallResult {
    0
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

    let (parent_cr3, mmap_regions) = match scheduler::get_task_arc(current_pid) {
        Some(task_arc) => {
            let task = task_arc.lock();
            (task.page_table_root, task.mmap_regions.clone())
        }
        None => return Errno::ESRCH.into(),
    };

    let child_page_table =
        match crate::memory::r#virtual::clone_parent_page_table(parent_cr3, &mmap_regions) {
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
            child_task.mmap_regions = parent_task.mmap_regions.clone();
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
        if validate_user_ptr_write(parent_tidptr as *mut u8, core::mem::size_of::<i32>()).is_err() {
            return Errno::EFAULT.into();
        }
        unsafe {
            parent_tidptr.write_volatile(child_pid.as_u64() as i32);
        }
    }
    if flags & 0x01000000 != 0 && !child_tidptr.is_null() {
        if validate_user_ptr_write(child_tidptr as *mut u8, core::mem::size_of::<i32>()).is_err() {
            return Errno::EFAULT.into();
        }
        unsafe {
            child_tidptr.write_volatile(child_pid.as_u64() as i32);
        }
    }

    scheduler::add_task(child_task);

    child_pid.as_u64() as SyscallResult
}
