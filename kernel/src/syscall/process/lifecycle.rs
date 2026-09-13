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

//! Process lifecycle and scheduler/memory control system calls.

use alloc::sync::Arc;

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

    // kprintln!("[syscall] fork()");

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let (parent_cr3, mmap_regions) = match scheduler::get_task_arc(current_pid) {
        Some(task_arc) => {
            let task = task_arc.lock();
            let addr_space = task.address_space.lock();
            (addr_space.page_table_root, addr_space.mmap_regions.clone())
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

            // Copy file descriptors (fork does not share FdTable)
            let parent_fds = parent_task.fd_table.lock();
            let mut child_fds = child_task.fd_table.lock();
            child_fds.entries = parent_fds.entries.clone();
            child_fds.cloexec = parent_fds.cloexec.clone();
            for slot in &child_fds.entries {
                if let Some(ref file_desc) = slot {
                    *file_desc.ref_count.lock() += 1;
                }
            }
            drop(child_fds);
            drop(parent_fds);

            // Copy address space metrics (fork does not share AddressSpace)
            let parent_vm = parent_task.address_space.lock();
            let mut child_vm = child_task.address_space.lock();
            child_vm.start_brk = parent_vm.start_brk;
            child_vm.brk = parent_vm.brk;
            child_vm.mmap_bump = parent_vm.mmap_bump;
            child_vm.mmap_regions = parent_vm.mmap_regions.clone();
            drop(child_vm);
            drop(parent_vm);

            // Clone sigactions Array into a new Mutex
            let parent_sigs = parent_task.sigactions.lock();
            child_task.sigactions = Arc::new(spin::Mutex::new(*parent_sigs));
            drop(parent_sigs);

            child_task.sigaltstack = parent_task.sigaltstack;
            child_task.blocked_signals = parent_task.blocked_signals;
            child_task.cwd = parent_task.cwd.clone();
            child_task.uid = parent_task.uid;
            child_task.gid = parent_task.gid;
            child_task.euid = parent_task.euid;
            child_task.egid = parent_task.egid;
            child_task.suid = parent_task.suid;
            child_task.sgid = parent_task.sgid;
            child_task.groups = parent_task.groups.clone();
            child_task.sid = parent_task.sid;
            child_task.pgid = parent_task.pgid;
            child_task.rlimit_nofile_cur = parent_task.rlimit_nofile_cur;
            child_task.rlimit_nofile_max = parent_task.rlimit_nofile_max;
            child_task.executable_path = parent_task.executable_path.clone();
            child_task.cmdline = parent_task.cmdline.clone();
            child_task.umask = parent_task.umask;

            // Inherit filesystem context: child shares the parent's mount namespace
            // (and jail root) via Arc clone.  On unshare(CLONE_NEWNS) the child
            // will replace its Arc with a private deep-cloned MountNamespace.
            child_task.fs_ctx = Arc::new(spin::RwLock::new(parent_task.fs_ctx.read().fork()));

            // Clone UTS namespace so child can independently change its hostname
            // after unshare(CLONE_NEWUTS).
            child_task.uts_ns = parent_task.uts_ns.clone();

            // Inherit the parent's PID namespace, or enter the unshared PID
            // namespace if parent previously called unshare(CLONE_NEWPID).
            if let Some(new_pid_ns) = parent_task.child_pid_ns_id {
                child_task.pid_ns_id = new_pid_ns;
                child_task.is_pid_ns_init = true;
                crate::kprintln!(
                    "[namespace] Forked child PID {} entered new PID namespace {}",
                    child_pid.as_u64(),
                    child_task.pid_ns_id
                );
            } else {
                child_task.pid_ns_id = parent_task.pid_ns_id;
                child_task.is_pid_ns_init = false;
            }
        } else {
            return Errno::ESRCH.into();
        }
    }
    child_task.pending_signals = 0; // Fork clears pending signals
    child_task.parent_pid = current_pid;

    // Allocate a kernel stack for the child
    let layout =
        alloc::alloc::Layout::from_size_align(crate::process::KERNEL_STACK_SIZE, 16).unwrap();
    let kstack_base = unsafe { alloc::alloc::alloc(layout) } as u64;
    if kstack_base == 0 {
        return Errno::ENOMEM.into();
    }
    child_task.kernel_stack_base = kstack_base;
    child_task.kernel_stack_size = crate::process::KERNEL_STACK_SIZE;

    // Copy the parent's SavedRegisters to the top of the child's kernel stack.
    // SavedRegisters size is 128 bytes, which is 16-byte aligned.
    let child_regs_ptr = (kstack_base + crate::process::KERNEL_STACK_SIZE as u64 - 128)
        as *mut crate::syscall::SavedRegisters;
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

    if crate::syscall::DEBUG_SYSCALLS {
        crate::kprintln!("[syscall] fork debug: rip = {:#x}, rsp = {:#x}, cr3 = {:#x}, fs_base = {:#x}, gs_base = {:#x}", 
            child_context.rip, child_context.rsp, child_context.cr3, child_context.fs_base, child_context.kernel_gs_base);
    }

    child_task.context = child_context;

    scheduler::add_task(child_task);

    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!("[syscall] fork() -> parent returns child PID {}", child_pid);
    }
    child_pid.as_u64() as SyscallResult
}

/// Helper to map loadable ELF segments into the user page table.
///
/// Returns the maximum virtual address mapped on success.
fn map_elf_segments(
    new_page_table: u64,
    segments: &[crate::process::elf::LoadSegment],
    inode: &alloc::sync::Arc<dyn crate::fs::inode::InodeOps>,
    bias: u64,
    mmap_regions: &mut alloc::vec::Vec<crate::process::task::MappedRegion>,
    pathname: Option<&str>,
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

        let _aligned_len = ((segment.mem_size + 4095) & !4095) as usize;
        let mut prot = 0;
        if segment.flags.read {
            prot |= 1;
        }
        if segment.flags.write {
            prot |= 2;
        }
        if segment.flags.execute {
            prot |= 4;
        }

        let page_start = vaddr & !4095;
        let page_offset = segment.file_offset & !4095;
        let page_end = (vaddr + segment.mem_size + 4095) & !4095;
        let page_len = (page_end - page_start) as usize;

        mmap_regions.push(crate::process::task::MappedRegion {
            start: page_start,
            len: page_len,
            inode: Some(inode.clone()),
            offset: page_offset,
            is_shared: false,
            prot,
            pathname: pathname.map(|p| alloc::string::String::from(p)),
            is_stack: false,
        });

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
                let src_offset = segment.file_offset + page_offset_in_seg;
                let dst_start = seg_offset_in_page as usize;

                let dst_slice_buf = &mut dest_slice[dst_start..dst_start + copy_len as usize];
                match inode.read(src_offset, dst_slice_buf) {
                    Ok(_) => {}
                    Err(_) => return Err(Errno::EIO),
                }
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

    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!(
            "[syscall] execve(\"{}\") with {} args, {} env vars",
            path,
            argv.len(),
            envp.len()
        );
    }

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

    // Read the ELF binary header (first 64KB)
    let file_size = inode.inode().size as usize;
    if file_size == 0 {
        crate::kprintln!("[syscall] execve: file_size is 0 for {}", path);
        return Errno::ENOEXEC.into();
    }

    let header_read_size = core::cmp::min(file_size, 65536);
    let mut elf_buf = alloc::vec![0u8; header_read_size];
    match inode.read(0, &mut elf_buf) {
        Ok(_) => {}
        Err(e) => {
            crate::kprintln!(
                "[syscall] execve: failed to read header for {}: {}",
                path,
                e
            );
            return e as SyscallResult;
        }
    }

    let mut path = path;
    let mut argv = argv;
    let mut loop_count = 0;
    let mut active_inode = inode.clone();
    let mut elf_total_size = file_size;

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
        let interp_header_size = core::cmp::min(interp_size, 65536);
        let mut new_buf = alloc::vec![0u8; interp_header_size];
        match interp_inode.read(0, &mut new_buf) {
            Ok(_) => {
                elf_buf = new_buf;
            }
            Err(e) => return e as SyscallResult,
        }
        active_inode = interp_inode;
        elf_total_size = interp_size;
    }

    // Parse the ELF
    let elf_info = match crate::process::elf::parse_elf(&elf_buf, elf_total_size) {
        Ok(e) => e,
        Err(e) => {
            crate::kprintln!("[syscall] execve: parse_elf failed for {}: {:?}", path, e);
            return Errno::ENOEXEC.into();
        }
    };

    // Resolve canonical absolute path for /proc/self/exe for the final binary
    let canonical_exec_path = crate::fs::vfs::resolve_canonical(&path)
        .unwrap_or_else(|| crate::fs::vfs::resolve_relative_path(&path));

    // Create a fresh user page table
    let new_page_table = match crate::memory::r#virtual::create_user_page_table() {
        Ok(pt) => pt,
        Err(_) => return Errno::ENOMEM.into(),
    };

    use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    let mut exec_mmap_regions = alloc::vec::Vec::new();

    let min_vaddr = elf_info.segments.iter().map(|s| s.vaddr).min().unwrap_or(0);
    let main_bias = if elf_info.is_pie || min_vaddr == 0 {
        crate::process::elf::USER_SPACE_BASE
    } else {
        0u64
    };

    let main_max_vaddr = match map_elf_segments(
        new_page_table,
        &elf_info.segments,
        &active_inode,
        main_bias,
        &mut exec_mmap_regions,
        Some(&path),
    ) {
        Ok(addr) => addr,
        Err(e) => return e as SyscallResult,
    };

    let initial_brk = (main_max_vaddr + 4095) & !4095;

    let mut entry = elf_info.entry_point + main_bias;
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
            crate::kprintln!("[syscall] execve: interp_size is 0 for {}", interp_path);
            return Errno::ENOEXEC.into();
        }
        let interp_header_size = core::cmp::min(interp_size, 65536);
        let mut interp_elf_buf = alloc::vec![0u8; interp_header_size];
        match interp_inode.read(0, &mut interp_elf_buf) {
            Ok(_) => {}
            Err(e) => return e as SyscallResult,
        }

        let interp_info = match crate::process::elf::parse_elf(&interp_elf_buf, interp_size) {
            Ok(e) => e,
            Err(e) => {
                crate::kprintln!(
                    "[syscall] execve: interp parse_elf failed for {}: {:?}",
                    interp_path,
                    e
                );
                return Errno::ENOEXEC.into();
            }
        };

        interpreter_base = 0x0000_7FFF_F7F0_0000;
        match map_elf_segments(
            new_page_table,
            &interp_info.segments,
            &interp_inode,
            interpreter_base,
            &mut exec_mmap_regions,
            elf_info.interpreter.as_deref(),
        ) {
            Ok(_) => {}
            Err(e) => return e as SyscallResult,
        }
        entry = interp_info.entry_point + interpreter_base;
    }

    // Map user stack
    let stack_size: u64 = 8 * 1024 * 1024; // 8 MiB stack size
    let stack_bottom = crate::process::elf::USER_STACK_TOP - stack_size;

    // Register full stack in mmap_regions for demand paging
    exec_mmap_regions.push(crate::process::task::MappedRegion {
        start: stack_bottom,
        len: stack_size as usize,
        inode: None,
        offset: 0,
        is_shared: false,
        prot: 3, // PROT_READ | PROT_WRITE
        pathname: Some(alloc::string::String::from("[stack]")),
        is_stack: true,
    });

    // Construct System V ABI compliant stack with multi-page support
    let init_stack = match crate::process::elf::construct_user_stack(
        &argv,
        &envp,
        elf_info.entry_point + main_bias,
        elf_info.phdr + main_bias,
        elf_info.phnum,
        elf_info.phent,
        interpreter_base,
    ) {
        Ok(stack) => stack,
        Err(e) => return e.into(),
    };

    let user_sp = init_stack.user_sp;

    // Allocate and map all physical pages containing initial stack data
    let num_pages = init_stack.stack_buf.len() / 4096;
    for i in 0..num_pages {
        let page_vaddr = init_stack.base_vaddr + (i as u64 * 4096);
        let phys = match crate::memory::physical::allocate_frame() {
            Some(p) => p,
            None => return Errno::ENOMEM.into(),
        };

        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(page_vaddr));
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
                crate::memory::physical::deallocate_frame(phys);
                return Errno::ENOMEM.into();
            }

            let dest = (phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            core::ptr::copy_nonoverlapping(
                init_stack.stack_buf[i * 4096..(i + 1) * 4096].as_ptr(),
                dest,
                4096,
            );
        }
    }

    // Reset signal state and update page table root for execve
    let _old_page_table = {
        let current_pid = match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        };
        if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
            let tgid = task_arc.lock().tgid;

            // Terminate other threads in the same thread group without taking Task locks
            let mut other_pids = alloc::vec::Vec::new();
            let current_pid_val = current_pid.as_u64();
            let tgid_val = tgid.as_u64();
            for (p, atom) in scheduler::TASK_TGIDS.iter().enumerate() {
                if p as u64 != current_pid_val
                    && atom.load(core::sync::atomic::Ordering::Acquire) == tgid_val
                {
                    other_pids.push(crate::process::pid::Pid::from_raw(p as u64));
                }
            }

            if !other_pids.is_empty() {
                let fds = x86_64::instructions::interrupts::without_interrupts(|| {
                    let mut collected = alloc::vec::Vec::new();
                    if let Some(ref mut sched) = *scheduler::SCHEDULER.lock() {
                        for pid in &other_pids {
                            collected.push(sched.exit_task(*pid, 0));
                        }
                    }
                    collected
                });
                drop(fds);

                // Wait for other cores to context-switch away from the terminated threads
                let mut waiting = true;
                while waiting {
                    waiting = false;
                    x86_64::instructions::interrupts::without_interrupts(|| {
                        if let Some(ref sched) = *scheduler::SCHEDULER.lock() {
                            for &pid in &other_pids {
                                for cpu_pid in &sched.current_cpus {
                                    if *cpu_pid == Some(pid) {
                                        waiting = true;
                                        break;
                                    }
                                }
                                if waiting {
                                    break;
                                }
                            }
                        }
                    });
                    if waiting {
                        core::hint::spin_loop();
                    }
                }
            }

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

            let mut sigs = task.sigactions.lock();
            for action in sigs.iter_mut() {
                if action.sa_handler != 1 {
                    // If not SIG_IGN
                    *action = crate::process::task::SigAction::default();
                }
            }
            drop(sigs);
            task.pending_signals = 0;
            let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
            unsafe {
                if apic_id < 32 {
                    crate::syscall::CPU_SCRATCHES[apic_id].signals_pending = 0;
                }
            }
            // Unshare fd_table if it was shared with threads across CLONE_FILES
            if Arc::strong_count(&task.fd_table) > 1 {
                let current_table = task.fd_table.lock();
                let new_entries = current_table.entries.clone();
                let new_cloexec = current_table.cloexec.clone();
                for slot in &new_entries {
                    if let Some(ref file_desc) = slot {
                        *file_desc.ref_count.lock() += 1;
                    }
                }
                drop(current_table);
                task.fd_table = Arc::new(spin::Mutex::new(crate::process::task::FdTable {
                    entries: new_entries,
                    cloexec: new_cloexec,
                }));
            }

            // Close O_CLOEXEC file descriptors
            let mut fd_table = task.fd_table.lock();
            for i in 0..fd_table.entries.len() {
                if i < fd_table.cloexec.len() && fd_table.cloexec[i] {
                    if let Some(desc) = fd_table.entries[i].take() {
                        let mut rc = desc.ref_count.lock();
                        if *rc > 0 {
                            *rc -= 1;
                        }
                    }
                    fd_table.cloexec[i] = false;
                }
            }
            drop(fd_table);

            task.name = path.clone();
            task.executable_path = canonical_exec_path.clone();
            task.cmdline = argv.clone();
            task.sigaltstack = None; // Reset alternate signal stack on execve

            // Switch to the new page table first so the CPU is no longer using the old one.
            unsafe {
                x86_64::registers::control::Cr3::write(
                    x86_64::structures::paging::PhysFrame::containing_address(
                        x86_64::PhysAddr::new(new_page_table),
                    ),
                    x86_64::registers::control::Cr3Flags::empty(),
                );
            }

            // Replace address space. This drops the old Arc<Mutex<AddressSpace>>.
            // If no other processes (like a parent process in CLONE_VM/vfork) are sharing it,
            // the old AddressSpace will be dropped and its page table root freed automatically.
            task.address_space = Arc::new(spin::Mutex::new(crate::process::task::AddressSpace {
                page_table_root: new_page_table,
                start_brk: initial_brk,
                brk: initial_brk,
                mmap_bump: 0x0000_5000_0000_0000u64,
                mmap_regions: exec_mmap_regions,
            }));

            task.context.fs_base = 0; // Clear TLS base for new process
            task.context.cr3 = new_page_table; // Set the new page table root in CPU context!
        }
    };

    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!(
            "[syscall] execve: loading OK, entry={:#x}, jumping to Ring 3...",
            entry
        );
    }

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
    crate::process::scheduler::exit_current_thread(status);
}

/// `exit_group(status)` — Terminate all threads in the thread group.
pub fn sys_exit_group(status: i32) -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => {
            scheduler::schedule();
            loop {
                x86_64::instructions::hlt();
            }
        }
    };

    let tgid = if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        task_arc.lock().tgid
    } else {
        // Task already exited or reaped on another core; schedule away and halt
        scheduler::schedule();
        loop {
            x86_64::instructions::hlt();
        }
    };

    // Get all other tasks sharing the same tgid without taking Task locks
    let mut other_pids = alloc::vec::Vec::new();
    let current_pid_val = current_pid.as_u64();
    let tgid_val = tgid.as_u64();
    for (p, atom) in scheduler::TASK_TGIDS.iter().enumerate() {
        if p as u64 != current_pid_val
            && atom.load(core::sync::atomic::Ordering::Acquire) == tgid_val
        {
            other_pids.push(crate::process::pid::Pid::from_raw(p as u64));
        }
    }

    // Clear fd_table entries OUTSIDE the scheduler lock
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        let fd_table = {
            let task = task_arc.lock();
            task.fd_table.clone()
        };
        if Arc::strong_count(&fd_table) == 1 {
            fd_table.lock().entries.clear();
        }
    }

    // Disable interrupts before scheduler manipulation
    x86_64::instructions::interrupts::disable();

    let mut all_fds = alloc::vec::Vec::new();
    if let Some(ref mut sched) = *scheduler::SCHEDULER.lock() {
        for pid in other_pids {
            // Drain futex registrations for this thread before marking it Zombie
            crate::syscall::process::futex::futex_drain_pid_locked(pid, sched);

            // Fire clear_child_tid for forcibly-killed threads
            if let Some(task_arc) = scheduler::get_task_arc(pid) {
                let task = task_arc.lock();
                let ctid = task.clear_child_tid;
                let tgid = task.tgid.as_u64();
                drop(task);
                if let Some(uaddr) = ctid {
                    crate::syscall::process::futex::clear_child_tid_wake_locked(tgid, uaddr, sched);
                }
            }

            all_fds.push(sched.exit_task(pid, status));
        }

        // Drain own futex registrations
        crate::syscall::process::futex::futex_drain_pid_locked(current_pid, sched);
        all_fds.push(sched.exit_task(current_pid, status));
    }
    drop(all_fds);

    scheduler::schedule();

    loop {
        x86_64::instructions::hlt();
    }
}

/// `sched_yield()` — Yield the processor.
pub fn sys_sched_yield() -> SyscallResult {
    crate::process::scheduler::yield_now();
    0
}

/// `wait4(pid, wstatus, options, rusage)` — Wait for a child process.
///
/// Cooperatively yields until a zombie child is found, then reaps it.
pub fn sys_wait4(pid: i32, wstatus: *mut i32, _options: i32, _rusage: *mut u8) -> SyscallResult {
    use crate::process::task::TaskState;

    if !wstatus.is_null()
        && validate_user_ptr_write(wstatus as *mut u8, core::mem::size_of::<i32>()).is_err()
    {
        return Errno::EFAULT.into();
    }

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    // kprintln!("[syscall] wait4(pid={})", pid);

    loop {
        // Disable interrupts and lock SCHEDULER
        x86_64::instructions::interrupts::disable();
        let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();

        // Scan for a zombie child
        let result = {
            let tasks = scheduler::TASKS.read();
            let mut found = None;
            let mut has_children = false;
            for slot in tasks.iter() {
                if let Some(task_arc) = slot {
                    if let Some(task) = task_arc.try_lock() {
                        let is_child = task.parent_pid == current_pid;
                        let matches_pid = pid == -1 || task.pid.as_u64() as i32 == pid;
                        // CLONE_THREAD tasks (threads, not process leaders) are never
                        // wait4-able from an external process.  They are reaped by
                        // the thread-group leader via pthread_join / futex, not wait4.
                        let is_process_leader = task.tgid == task.pid;
                        if is_child && matches_pid && is_process_leader {
                            has_children = true;
                            if task.state == TaskState::Zombie {
                                if crate::syscall::DEBUG_SYSCALLS {
                                    let (total_f, alloc_f, free_f) =
                                        crate::memory::physical::stats();
                                    crate::kprintln!(
                                        "[syscall] wait4: found zombie child PID {}, free_mem={}MB/{}MB (alloc_frames={})",
                                        task.pid,
                                        (free_f * 4096) / (1024 * 1024),
                                        (total_f * 4096) / (1024 * 1024),
                                        alloc_f
                                    );
                                }
                                found = Some((task.pid, task.exit_code.unwrap_or(0)));
                                break;
                            }
                        }
                    } else {
                        // Task is active on another core; assume runnable child
                        has_children = true;
                    }
                }
            }
            drop(tasks);

            if let Some((child_pid, exit_code)) = found {
                // Remove the zombie from the task list and drop it outside the TASKS lock
                let idx = child_pid.as_u64() as usize;
                let mut tasks_write = scheduler::TASKS.write();
                if let Some(slot) = tasks_write.get_mut(idx) {
                    slot.take();
                }
                drop(tasks_write);
                if idx < scheduler::TASK_TGIDS.len() {
                    scheduler::TASK_TGIDS[idx].store(0, core::sync::atomic::Ordering::Release);
                }

                Some(Ok((child_pid, exit_code)))
            } else if !has_children {
                Some(Err(Errno::ECHILD))
            } else {
                None
            }
        };

        match result {
            Some(Ok((child_pid, exit_code))) => {
                drop(sched_lock);
                x86_64::instructions::interrupts::enable();

                // Write exit status to user-space if wstatus is non-null with interrupts enabled
                if !wstatus.is_null() {
                    unsafe {
                        wstatus.write_volatile((exit_code & 0xFF) << 8);
                    }
                }
                return child_pid.as_u64() as SyscallResult;
            }
            Some(Err(err)) => {
                drop(sched_lock);
                x86_64::instructions::interrupts::enable();
                return err.into();
            }
            None => {
                // Sleep on our child_wait_queue until a child exits.
                // Since we hold the SCHEDULER lock, no child can exit or wake us up before we block!
                let task_arc = match scheduler::get_task_arc(current_pid) {
                    Some(t) => t,
                    None => {
                        drop(sched_lock);
                        x86_64::instructions::interrupts::enable();
                        return Errno::ESRCH.into();
                    }
                };
                let wait_queue = {
                    let mut task = task_arc.lock();
                    task.state = TaskState::Blocked;
                    task.child_wait_queue.clone()
                };
                wait_queue.register(current_pid);
                drop(sched_lock);

                // Note: schedule() will yield CPU and re-enable interrupts inside the new task's context switch
                scheduler::schedule();

                // When we wake up, interrupts are enabled. We must disable them again to clean up and re-loop
                x86_64::instructions::interrupts::disable();
                wait_queue.remove(current_pid);
                x86_64::instructions::interrupts::enable();
            }
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SigInfo {
    pub si_signo: i32,
    pub si_errno: i32,
    pub si_code: i32,
    pub si_pid: i32,
    pub si_uid: u32,
    pub si_status: i32,
    pub _pad: [u8; 104],
}

impl Default for SigInfo {
    fn default() -> Self {
        Self {
            si_signo: 0,
            si_errno: 0,
            si_code: 0,
            si_pid: 0,
            si_uid: 0,
            si_status: 0,
            _pad: [0u8; 104],
        }
    }
}

/// `waitid(which, id, infop, options, ru)` — Wait for process to change state.
pub fn sys_waitid(
    which: i32,
    id: i32,
    infop: *mut SigInfo,
    options: i32,
    _ru: *mut u8,
) -> SyscallResult {
    let pid = match which {
        0 => -1,  // P_ALL
        1 => id,  // P_PID
        2 => -id, // P_PGID
        _ => return Errno::EINVAL.into(),
    };

    let mut wstatus = 0i32;
    let ret = sys_wait4(
        pid,
        &mut wstatus as *mut i32,
        options,
        core::ptr::null_mut(),
    );
    if ret < 0 {
        return ret;
    }

    if !infop.is_null() {
        if validate_user_ptr_write(infop as *mut u8, core::mem::size_of::<SigInfo>()).is_err() {
            return Errno::EFAULT.into();
        }
        let info = SigInfo {
            si_signo: 17, // SIGCHLD
            si_errno: 0,
            si_code: 1, // CLD_EXITED
            si_pid: ret as i32,
            si_uid: 0,
            si_status: (wstatus >> 8) & 0xFF,
            _pad: [0u8; 104],
        };
        // SAFETY: Pointer validated with validate_user_ptr_write.
        unsafe {
            core::ptr::write(infop, info);
        }
    }
    0
}

/// `vfork()` — Create a child process and block parent.
pub fn sys_vfork(regs: *mut crate::syscall::SavedRegisters) -> SyscallResult {
    sys_fork(regs)
}

/// `brk(addr)` — Set the program break (end of data segment / heap top).
///
/// If `addr` is 0 or less than `start_brk`, returns the current break.
/// Otherwise extends or shrinks the heap up to `addr`.
pub fn sys_brk(addr: u64) -> SyscallResult {
    use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };

    let (current_brk, start_brk, page_table_root) = {
        let task = task_arc.lock();
        let addr_space = task.address_space.lock();
        (
            addr_space.brk,
            addr_space.start_brk,
            addr_space.page_table_root,
        )
    };

    if addr == 0 || addr < start_brk {
        return current_brk as SyscallResult;
    }

    if addr == current_brk {
        return current_brk as SyscallResult;
    }

    let aligned_new_brk = (addr + 4095) & !4095;
    let aligned_current_brk = (current_brk + 4095) & !4095;

    if aligned_new_brk > aligned_current_brk {
        // Check for overlap with existing mmap regions
        {
            let task = task_arc.lock();
            let addr_space = task.address_space.lock();
            for r in &addr_space.mmap_regions {
                let r_end = r.start.saturating_add(r.len as u64);
                if aligned_current_brk < r_end && aligned_new_brk > r.start {
                    return current_brk as SyscallResult;
                }
            }
        }

        let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(aligned_current_brk));
        let end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(aligned_new_brk - 1));

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
                let dest = (phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
                unsafe {
                    core::ptr::write_bytes(dest, 0, 4096);
                }
                mapped_count += 1;
            } else {
                if mapped_count > 0 {
                    crate::arch::x86_64::smp::shootdown_tlb();
                }
                return current_brk as SyscallResult;
            }
        }

        if mapped_count > 0 {
            crate::arch::x86_64::smp::shootdown_tlb();
        }
    } else if aligned_new_brk < aligned_current_brk {
        // Shrink heap
        let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(aligned_new_brk));
        let end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(aligned_current_brk - 1));

        let mut unmapped_count = 0;
        for page in Page::range_inclusive(start_page, end_page) {
            if let Ok(phys_addr) = unsafe {
                crate::memory::r#virtual::unmap_user_page_no_shootdown(page_table_root, page)
            } {
                crate::memory::physical::deallocate_frame(phys_addr);
                unmapped_count += 1;
            }
        }

        if unmapped_count > 0 {
            crate::arch::x86_64::smp::shootdown_tlb();
        }
    }

    {
        let task = task_arc.lock();
        task.address_space.lock().brk = addr;
    }

    addr as SyscallResult
}

/// `arch_prctl()` — Set/get thread base registers (FS_BASE / GS_BASE).
pub fn sys_arch_prctl(code: i32, addr: u64) -> SyscallResult {
    // kprintln!("[syscall] arch_prctl(code={:#x}, addr={:#x})", code, addr);
    match code {
        0x1001 => {
            // ARCH_SET_GS
            let mut msr = x86_64::registers::model_specific::Msr::new(0xC0000102);
            unsafe {
                msr.write(addr);
            }
            let current_pid = match scheduler::current_pid() {
                Some(p) => p,
                None => return Errno::ESRCH.into(),
            };
            if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
                task_arc.lock().context.kernel_gs_base = addr;
            }
            0
        }
        0x1002 => {
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
        }
        0x1003 => {
            // ARCH_GET_FS
            if validate_user_ptr_write(addr as *mut u8, 8).is_err() {
                return Errno::EFAULT.into();
            }
            let fs_base = x86_64::registers::model_specific::FsBase::read().as_u64();
            // SAFETY: The pointer was validated with validate_user_ptr_write and is safe to write to.
            unsafe {
                *(addr as *mut u64) = fs_base;
            }
            0
        }
        0x1004 => {
            // ARCH_GET_GS
            if validate_user_ptr_write(addr as *mut u8, 8).is_err() {
                return Errno::EFAULT.into();
            }
            let msr = x86_64::registers::model_specific::Msr::new(0xC0000102);
            let gs_base = unsafe { msr.read() };
            // SAFETY: The pointer was validated with validate_user_ptr_write and is safe to write to.
            unsafe {
                *(addr as *mut u64) = gs_base;
            }
            0
        }
        _ => Errno::EINVAL.into(),
    }
}

/// `set_tid_address()` — Set thread ID pointer.
pub fn sys_set_tid_address(tidptr: *mut i32) -> SyscallResult {
    if !tidptr.is_null() && !validate_user_ptr(tidptr as *const u8, core::mem::size_of::<i32>()) {
        return Errno::EFAULT.into();
    }
    let pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = scheduler::get_task_arc(pid) {
        task_arc.lock().clear_child_tid = if tidptr.is_null() {
            None
        } else {
            Some(tidptr as u64)
        };
    }
    pid.as_u64() as i64
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

    // kprintln!("[syscall] clone(flags={:#x}, child_stack={:#x}, parent_tid={:?}, child_tid={:?}, newtls={:#x})",
    //    flags, child_stack, parent_tidptr, child_tidptr, newtls);

    if child_stack != 0 && child_stack > 0x0000_7FFF_FFFF_FFFF {
        return Errno::EINVAL.into();
    }

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let (parent_cr3, mmap_regions, parent_brk, parent_mmap_bump) =
        match scheduler::get_task_arc(current_pid) {
            Some(task_arc) => {
                let task = task_arc.lock();
                let addr_space = task.address_space.lock();
                (
                    addr_space.page_table_root,
                    addr_space.mmap_regions.clone(),
                    addr_space.brk,
                    addr_space.mmap_bump,
                )
            }
            None => return Errno::ESRCH.into(),
        };

    let is_clone_vm = flags & 0x00000100 != 0;
    let child_page_table = if is_clone_vm {
        // CLONE_VM: share page tables
        parent_cr3
    } else {
        match crate::memory::r#virtual::clone_parent_page_table(parent_cr3, &mmap_regions) {
            Ok(pt) => pt,
            Err(_) => return Errno::ENOMEM.into(),
        }
    };

    let child_pid = pid::allocate();
    let mut child_task = Task::new(
        child_pid,
        alloc::format!("clone:{}", child_pid),
        if is_clone_vm { 0 } else { child_page_table },
    );

    if flags & 0x00200000 != 0 {
        child_task.clear_child_tid = Some(child_tidptr as u64);
    }

    {
        if let Some(parent_task_arc) = scheduler::get_task_arc(current_pid) {
            let parent_task = parent_task_arc.lock();

            // CLONE_FILES
            if flags & 0x00000400 != 0 {
                child_task.fd_table = parent_task.fd_table.clone();
            } else {
                let parent_fds = parent_task.fd_table.lock();
                let mut child_fds = child_task.fd_table.lock();
                child_fds.entries = parent_fds.entries.clone();
                child_fds.cloexec = parent_fds.cloexec.clone();
                for slot in &child_fds.entries {
                    if let Some(ref file_desc) = slot {
                        *file_desc.ref_count.lock() += 1;
                    }
                }
            }

            // CLONE_VM
            if flags & 0x00000100 != 0 {
                child_task.address_space = parent_task.address_space.clone();
            } else {
                let parent_start_brk = parent_task.address_space.lock().start_brk;
                let mut child_vm = child_task.address_space.lock();
                child_vm.start_brk = parent_start_brk;
                child_vm.brk = parent_brk;
                child_vm.mmap_bump = parent_mmap_bump;
                child_vm.mmap_regions = mmap_regions.clone();
            }

            // CLONE_SIGHAND
            if flags & 0x00000800 != 0 {
                child_task.sigactions = parent_task.sigactions.clone();
            } else {
                let parent_sigs = parent_task.sigactions.lock();
                child_task.sigactions = Arc::new(spin::Mutex::new(*parent_sigs));
            }

            // CLONE_THREAD
            if flags & 0x00010000 != 0 {
                child_task.tgid = parent_task.tgid;
            } else {
                child_task.tgid = child_pid;
            }

            child_task.sigaltstack = parent_task.sigaltstack;
            child_task.blocked_signals = parent_task.blocked_signals;
            child_task.cwd = parent_task.cwd.clone();
            child_task.uid = parent_task.uid;
            child_task.gid = parent_task.gid;
            child_task.euid = parent_task.euid;
            child_task.egid = parent_task.egid;
            child_task.suid = parent_task.suid;
            child_task.sgid = parent_task.sgid;
            child_task.groups = parent_task.groups.clone();
            child_task.sid = parent_task.sid;
            child_task.pgid = parent_task.pgid;
            child_task.rlimit_nofile_cur = parent_task.rlimit_nofile_cur;
            child_task.rlimit_nofile_max = parent_task.rlimit_nofile_max;
            child_task.cmdline = parent_task.cmdline.clone();
            child_task.umask = parent_task.umask;

            // ── Namespace propagation for clone() ─────────────────────────────

            // CLONE_NEWNS: child gets a private copy of the mount namespace.
            if flags & CLONE_NEWNS != 0 {
                let new_ns = parent_task.fs_ctx.read().mount_ns.read().fork();
                child_task.fs_ctx = Arc::new(spin::RwLock::new(
                    crate::fs::namespace::FsContext::new_initial(alloc::sync::Arc::new(
                        spin::RwLock::new(new_ns),
                    )),
                ));
                child_task.fs_ctx.write().root = parent_task.fs_ctx.read().root.clone();
            } else {
                // Shared mount namespace (Arc clone).
                child_task.fs_ctx = Arc::new(spin::RwLock::new(parent_task.fs_ctx.read().fork()));
            }

            // CLONE_NEWUTS: child gets its own UTS namespace copy.
            // Since uts_ns is Clone the child always starts with parent's values;
            // after CLONE_NEWUTS, sethostname() will not affect the parent.
            child_task.uts_ns = parent_task.uts_ns.clone();

            // CLONE_NEWPID: child becomes the init of a new PID namespace.
            // We assign a fresh pid_ns_id; the child is still visible to the host
            // with its real PID, but kill/wait4/procfs are restricted by pid_ns_id.
            if flags & CLONE_NEWPID != 0 {
                child_task.pid_ns_id = crate::fs::namespace::alloc_pid_ns_id();
                child_task.is_pid_ns_init = true;
                crate::kprintln!(
                    "[namespace] PID {} entered new PID namespace {}",
                    child_pid.as_u64(),
                    child_task.pid_ns_id
                );
            } else if let Some(new_pid_ns) = parent_task.child_pid_ns_id {
                child_task.pid_ns_id = new_pid_ns;
                child_task.is_pid_ns_init = true;
                crate::kprintln!(
                    "[namespace] Cloned child PID {} entered new PID namespace {}",
                    child_pid.as_u64(),
                    child_task.pid_ns_id
                );
            } else {
                child_task.pid_ns_id = parent_task.pid_ns_id;
                child_task.is_pid_ns_init = false;
            }
        } else {
            return Errno::ESRCH.into();
        }
    }
    child_task.pending_signals = 0;
    child_task.parent_pid = current_pid;

    let layout =
        alloc::alloc::Layout::from_size_align(crate::process::KERNEL_STACK_SIZE, 16).unwrap();
    let kstack_base = unsafe { alloc::alloc::alloc(layout) } as u64;
    if kstack_base == 0 {
        return Errno::ENOMEM.into();
    }
    child_task.kernel_stack_base = kstack_base;
    child_task.kernel_stack_size = crate::process::KERNEL_STACK_SIZE;

    let child_regs_ptr = (kstack_base + crate::process::KERNEL_STACK_SIZE as u64 - 128)
        as *mut crate::syscall::SavedRegisters;
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

    unsafe {
        let child_regs = &*child_regs_ptr;
        if crate::syscall::DEBUG_SYSCALLS {
            crate::kprintln!(
                "[syscall] clone debug: child_pid = {}, ctx.rip = {:#x}, ctx.rsp = {:#x}, ctx.cr3 = {:#x}, regs.rip = {:#x}, regs.rsp = {:#x}, regs.rax = {:#x}",
                child_pid,
                child_context.rip,
                child_context.rsp,
                child_context.cr3,
                child_regs.rip,
                child_regs.rsp,
                child_regs.rax
            );
        }
    }

    if flags & 0x00001000 != 0 && !parent_tidptr.is_null() {
        if validate_user_ptr_write(parent_tidptr as *mut u8, core::mem::size_of::<i32>()).is_err() {
            return Errno::EFAULT.into();
        }
        let pidfd = Arc::new(crate::fs::pidfd::PidFd::new(child_pid.as_u64()));
        let fd = match crate::process::fd::current_task_alloc_fd_with_flags(
            pidfd,
            crate::fs::file::OpenFlags(
                crate::fs::file::OpenFlags::O_RDWR | crate::fs::file::OpenFlags::O_CLOEXEC,
            ),
        ) {
            Some(fd) => fd as i32,
            None => return Errno::EMFILE.into(),
        };
        // SAFETY: Pointer was validated above
        unsafe {
            parent_tidptr.write_volatile(fd);
        }
    } else if flags & 0x00100000 != 0 && !parent_tidptr.is_null() {
        if validate_user_ptr_write(parent_tidptr as *mut u8, core::mem::size_of::<i32>()).is_err() {
            return Errno::EFAULT.into();
        }
        // SAFETY: Pointer was validated above
        unsafe {
            parent_tidptr.write_volatile(child_pid.as_u64() as i32);
        }
    }
    if flags & 0x01000000 != 0 && !child_tidptr.is_null() {
        if validate_user_ptr_write(child_tidptr as *mut u8, core::mem::size_of::<i32>()).is_err() {
            return Errno::EFAULT.into();
        }
        // SAFETY: Pointer was validated above
        unsafe {
            child_tidptr.write_volatile(child_pid.as_u64() as i32);
        }
    }

    scheduler::add_task(child_task);

    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!(
            "[syscall] clone() -> parent returns child PID {}",
            child_pid
        );
    }
    child_pid.as_u64() as SyscallResult
}

/// Linux `clone_args` structure for `clone3`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CloneArgs {
    pub flags: u64,
    pub pidfd: u64,
    pub child_tid: u64,
    pub parent_tid: u64,
    pub exit_signal: u64,
    pub stack: u64,
    pub stack_size: u64,
    pub tls: u64,
    pub set_tid: u64,
    pub set_tid_size: u64,
    pub cgroup: u64,
}

/// `clone3(cl_args, size)` — create a child process using extended arguments.
pub fn sys_clone3(
    args_ptr: *const CloneArgs,
    size: usize,
    regs: *mut crate::syscall::SavedRegisters,
) -> SyscallResult {
    if size < 64 || size > 4096 {
        return Errno::EINVAL.into();
    }
    if args_ptr.is_null() {
        return Errno::EFAULT.into();
    }
    if !crate::syscall::validation::validate_user_ptr(args_ptr as *const u8, size) {
        return Errno::EFAULT.into();
    }

    // SAFETY: Pointer validated above
    let args = unsafe { core::ptr::read_volatile(args_ptr) };

    if args.exit_signal & !0xff != 0 {
        return Errno::EINVAL.into();
    }

    let flags = args.flags | (args.exit_signal & 0xff);
    let child_stack = if args.stack != 0 {
        if args.stack_size != 0 {
            match args.stack.checked_add(args.stack_size) {
                Some(top) => top,
                None => return Errno::EINVAL.into(),
            }
        } else {
            args.stack
        }
    } else {
        0
    };

    let parent_tidptr = if (args.flags & 0x00001000) != 0 {
        args.pidfd as *mut i32
    } else {
        args.parent_tid as *mut i32
    };
    let child_tidptr = args.child_tid as *mut i32;
    let newtls = args.tls;

    sys_clone(
        flags,
        child_stack,
        parent_tidptr,
        child_tidptr,
        newtls,
        regs,
    )
}

// ── Namespace Flags ───────────────────────────────────────────────────────────

/// Linux `clone` flag: give the process a private copy of its mount namespace.
pub const CLONE_NEWNS: u64 = 0x0002_0000;
/// Linux `clone` flag: give the process a private UTS namespace.
pub const CLONE_NEWUTS: u64 = 0x0400_0000;
/// Linux `clone` flag: give the process a new PID namespace.
pub const CLONE_NEWPID: u64 = 0x2000_0000;

/// `unshare(flags)` — Disassociate parts of the process execution context.
///
/// Supported flags:
///
/// - **`CLONE_NEWNS`** (`0x00020000`): Create a private copy of the current
///   mount namespace.  Future `mount(2)` / `umount2(2)` calls will only affect
///   this task's namespace, not any peer that still shares the old one.
///
/// - **`CLONE_NEWUTS`** (`0x04000000`): Create a private UTS namespace so that
///   `sethostname(2)` / `setdomainname(2)` only affect this task.
///
/// - **`CLONE_NEWPID`** (`0x20000000`): Create a new PID namespace for subsequent
///   children created by this task via `fork(2)` or `clone(2)`.
///
/// Requires `euid == 0` for `CLONE_NEWNS` and `CLONE_NEWPID` (as on Linux).
/// Returns 0 on success, or a negative errno.
pub fn sys_unshare(flags: u64) -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(a) => a,
        None => return Errno::ESRCH.into(),
    };

    // ── CLONE_NEWNS: private mount namespace ──────────────────────────────────
    if (flags & CLONE_NEWNS) != 0 {
        // Require root for mount namespace creation (Linux behaviour).
        if task_arc.lock().euid != 0 {
            return Errno::EPERM.into();
        }

        // Fork the current MountNamespace into a private clone.
        let new_ns = {
            let task = task_arc.lock();
            let old_ns = task.fs_ctx.read().mount_ns.read().fork();
            alloc::sync::Arc::new(spin::RwLock::new(old_ns))
        };

        // Install the new private namespace on this task.
        {
            let task = task_arc.lock();
            task.fs_ctx.write().mount_ns = new_ns;
        }

        crate::kprintln!(
            "[namespace] PID {} unshared mount namespace",
            current_pid.as_u64()
        );
    }

    // ── CLONE_NEWUTS: private UTS namespace ───────────────────────────────────
    if (flags & CLONE_NEWUTS) != 0 {
        // UTS namespace cloning just duplicates the current hostname/domainname
        // into the task — since uts_ns is already per-task (Clone), the task
        // already owns its own copy.  We just log for visibility.
        //
        // A subsequent sethostname() call will modify task.uts_ns.hostname
        // without touching any sibling's UTS state (handled in sys_sethostname).
        crate::kprintln!(
            "[namespace] PID {} unshared UTS namespace (hostname=\"{}\")",
            current_pid.as_u64(),
            task_arc.lock().uts_ns.hostname
        );
    }

    // ── CLONE_NEWPID: new PID namespace for subsequent children ───────────────
    if (flags & CLONE_NEWPID) != 0 {
        if task_arc.lock().euid != 0 {
            return Errno::EPERM.into();
        }

        let new_pid_ns = crate::fs::namespace::alloc_pid_ns_id();
        task_arc.lock().child_pid_ns_id = Some(new_pid_ns);

        crate::kprintln!(
            "[namespace] PID {} unshared PID namespace (future children will be in PID ns {})",
            current_pid.as_u64(),
            new_pid_ns
        );
    }

    // Ignore unknown/unsupported flags and return success (Linux behaviour for
    // unrecognised but harmless flags is EINVAL; we are lenient here to avoid
    // breaking container runtimes that OR in extra flags).
    let supported = CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWPID;
    if (flags & !supported) != 0 {
        return Errno::EINVAL.into();
    }

    0
}
