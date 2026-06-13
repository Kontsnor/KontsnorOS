//! Custom in-kernel test suite execution and test cases.

use crate::kprintln;
use core::panic::PanicInfo;

/// QEMU exit status codes mapping to the isa-debug-exit device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// Shuts down QEMU with the specified status code using the isa-debug-exit device.
pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    use x86_64::instructions::port::Port;
    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
    loop {
        x86_64::instructions::hlt();
    }
}

/// Custom test runner executing a slice of test cases.
pub fn test_runner(tests: &[&dyn Fn()]) {
    kprintln!("Running {} tests", tests.len());
    for (i, test) in tests.iter().enumerate() {
        kprintln!("Running test {}/{}", i + 1, tests.len());
        test();
        kprintln!("[ok]");
    }
    kprintln!("All tests passed!");
    exit_qemu(QemuExitCode::Success);
}

/// Test mode panic handler.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    x86_64::instructions::interrupts::disable();
    kprintln!();
    kprintln!("!!! TEST PANIC !!!");
    kprintln!("==================");
    if let Some(location) = info.location() {
        kprintln!(
            "  Location: {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
    }
    if let Some(message) = info.message().as_str() {
        kprintln!("  Message: {}", message);
    } else {
        kprintln!("  Message: {}", info.message());
    }
    kprintln!("==================");
    kprintln!("Test failed.");
    kprintln!();
    exit_qemu(QemuExitCode::Failed);
}

// ── Test Cases ─────────────────────────────────────────────────────────────

#[test_case]
fn test_trivial() {
    let two = 2;
    assert_eq!(1 + 1, two);
}

#[test_case]
fn test_memory_allocator() {
    let (initial_used, _, _) = crate::memory::heap::stats();
    {
        let mut vec = alloc::vec::Vec::new();
        for i in 0..1000 {
            vec.push(i);
        }
        let (used_during, _, _) = crate::memory::heap::stats();
        assert!(used_during > initial_used);
    }
    let (final_used, _, _) = crate::memory::heap::stats();
    assert_eq!(initial_used, final_used);
}

#[test_case]
fn test_vfs_path_resolution() {
    // Lookup non-existent path
    let non_existent = crate::fs::vfs::lookup("/tmp/nonexistent");
    assert!(non_existent.is_none());

    // Create a subdirectory under /tmp (which is tmpfs)
    let tmp_dir = crate::fs::vfs::lookup("/tmp").expect("Failed to lookup /tmp");
    let test_dir = tmp_dir
        .mkdir("test_dir")
        .expect("Failed to create /tmp/test_dir");

    // Create a file under /tmp/test_dir
    let test_file = test_dir
        .create("test.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create /tmp/test_dir/test.txt");

    // Write data to the file
    let test_data = b"Hello, VFS!";
    let written = test_file
        .write(0, test_data)
        .expect("Failed to write to test.txt");
    assert_eq!(written, test_data.len());

    // Invalidate the cache to ensure we test true lookup resolution
    crate::fs::vfs::invalidate_dentry("/tmp/test_dir/test.txt");

    // Read back data
    let looked_up = crate::fs::vfs::lookup("/tmp/test_dir/test.txt")
        .expect("Failed to lookup /tmp/test_dir/test.txt after cache invalidation");
    let mut read_buf = [0u8; 32];
    let read_len = looked_up
        .read(0, &mut read_buf)
        .expect("Failed to read from test.txt");
    assert_eq!(read_len, test_data.len());
    assert_eq!(&read_buf[..read_len], test_data);
}

#[test_case]
fn test_scheduler_priority_queues() {
    let mut sched = crate::process::scheduler::Scheduler::new();

    // Create mock tasks with High, Normal, and Low priorities
    let pid_high = crate::process::pid::Pid::from_raw(10);
    let mut task_high =
        crate::process::task::Task::new(pid_high, alloc::string::String::from("high_prio"), 0);
    task_high.priority = crate::process::task::Priority::High;
    task_high.state = crate::process::task::TaskState::Ready;

    let pid_normal = crate::process::pid::Pid::from_raw(11);
    let mut task_normal =
        crate::process::task::Task::new(pid_normal, alloc::string::String::from("normal_prio"), 0);
    task_normal.priority = crate::process::task::Priority::Normal;
    task_normal.state = crate::process::task::TaskState::Ready;

    let pid_low = crate::process::pid::Pid::from_raw(12);
    let mut task_low =
        crate::process::task::Task::new(pid_low, alloc::string::String::from("low_prio"), 0);
    task_low.priority = crate::process::task::Priority::Low;
    task_low.state = crate::process::task::TaskState::Ready;

    // Add them in mixed order
    sched.add_task(task_low);
    sched.add_task(task_high);
    sched.add_task(task_normal);

    // pick_next should retrieve them in priority order: High (10), Normal (11), Low (12)
    assert_eq!(sched.pick_next(), Some(pid_high));
    assert_eq!(sched.pick_next(), Some(pid_normal));
    assert_eq!(sched.pick_next(), Some(pid_low));
    assert_eq!(sched.pick_next(), None);
}

#[test_case]
fn test_orphan_reparenting() {
    let mut sched = crate::process::scheduler::Scheduler::new();

    // Create a mock init task (PID 1) so it exists in TASKS
    let pid_init = crate::process::pid::Pid::from_raw(1);
    let mut task_init =
        crate::process::task::Task::new(pid_init, alloc::string::String::from("init"), 0);
    task_init.state = crate::process::task::TaskState::Blocked;
    sched.add_task(task_init);

    // Create parent task (PID 20)
    let pid_parent = crate::process::pid::Pid::from_raw(20);
    let mut task_parent =
        crate::process::task::Task::new(pid_parent, alloc::string::String::from("parent"), 0);
    task_parent.state = crate::process::task::TaskState::Ready;
    sched.add_task(task_parent);

    // Create child task (PID 21) whose parent is the parent task
    let pid_child = crate::process::pid::Pid::from_raw(21);
    let mut task_child =
        crate::process::task::Task::new(pid_child, alloc::string::String::from("child"), 0);
    task_child.parent_pid = pid_parent;
    task_child.state = crate::process::task::TaskState::Ready;
    sched.add_task(task_child);

    // Exit the parent task
    sched.exit_task(pid_parent, 0);

    // Verify child has been re-parented to PID 1 (INIT)
    let child_arc = crate::process::scheduler::get_task_arc(pid_child).expect("Child task missing");
    let child = child_arc.lock();
    assert_eq!(child.parent_pid, crate::process::pid::Pid::INIT);

    // Verify parent has transitioned to Zombie
    let parent_arc =
        crate::process::scheduler::get_task_arc(pid_parent).expect("Parent task missing");
    let parent = parent_arc.lock();
    assert_eq!(parent.state, crate::process::task::TaskState::Zombie);
}

#[test_case]
fn test_vfs_permissions() {
    let pid = crate::process::scheduler::current_pid().expect("No current task");
    let task_arc = crate::process::scheduler::get_task_arc(pid).expect("No task arc");

    // Save original task credentials
    let (orig_uid, orig_gid, orig_euid, orig_egid) = {
        let t = task_arc.lock();
        (t.uid, t.gid, t.euid, t.egid)
    };

    // Reset to root (0)
    {
        let mut t = task_arc.lock();
        t.uid = 0;
        t.gid = 0;
        t.euid = 0;
        t.egid = 0;
    }

    // 1. Check getuid / getgid / geteuid / getegid system calls
    assert_eq!(crate::syscall::process::sys_getuid(), 0);
    assert_eq!(crate::syscall::process::sys_getgid(), 0);
    assert_eq!(crate::syscall::process::sys_geteuid(), 0);
    assert_eq!(crate::syscall::process::sys_getegid(), 0);

    // 2. setuid / setgid as root
    assert_eq!(crate::syscall::process::sys_setuid(1000), 0);
    assert_eq!(crate::syscall::process::sys_getuid(), 1000);
    assert_eq!(crate::syscall::process::sys_geteuid(), 1000);

    assert_eq!(crate::syscall::process::sys_setgid(2000), 0);
    assert_eq!(crate::syscall::process::sys_getgid(), 2000);
    assert_eq!(crate::syscall::process::sys_getegid(), 2000);

    // 3. Restricting unauthorized credential changes (non-root setting UID to arbitrary values)
    assert_eq!(
        crate::syscall::process::sys_setuid(1001),
        crate::syscall::Errno::EPERM as i64
    );
    assert_eq!(crate::syscall::process::sys_setuid(1000), 0);

    assert_eq!(
        crate::syscall::process::sys_setgid(2001),
        crate::syscall::Errno::EPERM as i64
    );
    assert_eq!(crate::syscall::process::sys_setgid(2000), 0);

    // 4. Access denial (EACCES) when attempting to open a file with incorrect permissions
    {
        let mut t = task_arc.lock();
        t.uid = 0;
        t.gid = 0;
        t.euid = 0;
        t.egid = 0;
    }

    let tmp_dir = crate::fs::vfs::lookup("/tmp").expect("Failed to lookup /tmp");
    let test_dir = tmp_dir
        .mkdir("perm_test_dir")
        .expect("Failed to create /tmp/perm_test_dir");

    let test_file = test_dir
        .create("test_file.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create test file");

    test_file
        .set_owner(1000, 2000)
        .expect("Failed to set owner");
    test_file
        .set_permissions(0o600)
        .expect("Failed to set permissions");

    // Make caller a different user (UID 3000, GID 3000)
    {
        let mut t = task_arc.lock();
        t.uid = 3000;
        t.gid = 3000;
        t.euid = 3000;
        t.egid = 3000;
    }

    // Try to check permission for test_file.txt
    let looked_up =
        crate::fs::vfs::lookup("/tmp/perm_test_dir/test_file.txt").expect("Lookup failed");
    assert_eq!(
        crate::fs::inode::check_permission(looked_up.inode(), crate::fs::inode::MAY_READ),
        Err(crate::syscall::Errno::EACCES)
    );

    // If we are owner (UID 1000), it should succeed
    {
        let mut t = task_arc.lock();
        t.euid = 1000;
    }
    assert_eq!(
        crate::fs::inode::check_permission(looked_up.inode(), crate::fs::inode::MAY_READ),
        Ok(())
    );

    // 5. Access denial when attempting to lookup a path containing a directory without execute permissions
    {
        let mut t = task_arc.lock();
        t.uid = 0;
        t.gid = 0;
        t.euid = 0;
        t.egid = 0;
    }
    test_dir
        .set_permissions(0o600)
        .expect("Failed to set dir permissions");

    // Make caller a different user (UID 3000, GID 3000)
    {
        let mut t = task_arc.lock();
        t.uid = 3000;
        t.gid = 3000;
        t.euid = 3000;
        t.egid = 3000;
    }

    // Try to lookup path (should return None because intermediate directory has no execute permission for other)
    assert!(crate::fs::vfs::lookup("/tmp/perm_test_dir/test_file.txt").is_none());

    // 6. Privilege elevation in execve when launching a set-UID file (using calculate_exec_creds)
    let (elevated_euid, elevated_egid) = crate::syscall::process::calculate_exec_creds(
        0o4755, // set-UID set
        0,      // owner is root
        0, 1000, 1000,
    );
    assert_eq!(elevated_euid, 0);
    assert_eq!(elevated_egid, 1000);

    let (elevated_euid_gid, elevated_egid_gid) = crate::syscall::process::calculate_exec_creds(
        0o2755, // set-GID set
        0, 0, // group is root
        1000, 1000,
    );
    assert_eq!(elevated_euid_gid, 1000);
    assert_eq!(elevated_egid_gid, 0);

    // Clean up
    {
        let mut t = task_arc.lock();
        t.uid = 0;
        t.gid = 0;
        t.euid = 0;
        t.egid = 0;
    }
    test_dir
        .set_permissions(0o777)
        .expect("Restore permissions");
    let _ = test_dir.unlink("test_file.txt");
    let _ = tmp_dir.rmdir("perm_test_dir");
    {
        let mut t = task_arc.lock();
        t.uid = orig_uid;
        t.gid = orig_gid;
        t.euid = orig_euid;
        t.egid = orig_egid;
    }
}

#[test_case]
fn test_shared_mapping_communication() {
    kprintln!("[test] Starting shared mapping communication test...");
    // 1. Create and open file on ext2 via VFS directly
    let disk_dir = crate::fs::vfs::lookup("/disk").expect("Failed to lookup /disk");
    let _ = disk_dir.unlink("shared_test.txt");
    let inode = disk_dir
        .create("shared_test.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create shared_test.txt");

    // 2. Write 4096 bytes directly using InodeOps::write to populate/extend it
    let data = [0u8; 4096];
    let written = inode
        .write(0, &data)
        .expect("Failed to write to shared_test.txt");
    assert_eq!(written, 4096);

    // Allocate file descriptor manually
    let fd = crate::process::fd::current_task_alloc_fd(inode.clone())
        .expect("Failed to allocate file descriptor");

    // 3. mmap it with MAP_SHARED
    let addr1 = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x01, fd, 0); // PROT_READ|WRITE, MAP_SHARED
    assert!(addr1 > 0);

    // 4. Simulate fork by cloning page table
    let current_pid = crate::process::scheduler::current_pid().unwrap();
    let parent_task_arc = crate::process::scheduler::get_task_arc(current_pid).unwrap();
    let (parent_cr3, mmap_regions) = {
        let task = parent_task_arc.lock();
        (task.page_table_root, task.mmap_regions.clone())
    };
    let child_cr3 = crate::memory::r#virtual::clone_parent_page_table(parent_cr3, &mmap_regions)
        .expect("Failed to clone page table");

    // Verify both point to same physical address
    let vaddr = x86_64::VirtAddr::new(addr1 as u64);
    let pte_parent = unsafe { crate::memory::page_cache::get_page_table_entry(parent_cr3, vaddr) }
        .expect("Parent PTE missing");
    let pte_child = unsafe { crate::memory::page_cache::get_page_table_entry(child_cr3, vaddr) }
        .expect("Child PTE missing");

    let phys_parent = pte_parent.addr().as_u64();
    let phys_child = pte_child.addr().as_u64();
    assert_eq!(phys_parent, phys_child);

    // Write magic value in parent mapping
    let ptr = addr1 as *mut u64;
    unsafe {
        ptr.write_volatile(0xDEADBEEF12345678);
    }
    kprintln!("[test] Wrote magic value to virtual ptr {:#x}", addr1);

    // Read magic value from virtual mapping directly
    let direct_val = unsafe { ptr.read_volatile() };
    kprintln!(
        "[test] Read magic value from virtual mapping: {:#x}",
        direct_val
    );

    // Read magic value from child's mapped physical address
    let phys_offset = crate::memory::r#virtual::phys_mem_offset();
    kprintln!(
        "[test] phys_parent={:#x}, phys_child={:#x}, phys_offset={:#x}",
        phys_parent,
        phys_child,
        phys_offset
    );
    let child_ptr = (phys_child + phys_offset) as *const u64;
    let read_val = unsafe { child_ptr.read_volatile() };
    kprintln!(
        "[test] Read magic value from child_ptr={:#x}: {:#x}",
        child_ptr as u64,
        read_val
    );
    assert_eq!(read_val, 0xDEADBEEF12345678);

    // Clean up
    crate::syscall::memory::sys_munmap(addr1 as u64, 4096);
    crate::process::fd::current_task_close_fd(fd);
    let _ = crate::memory::r#virtual::free_user_page_table(child_cr3);
    let _ = disk_dir.unlink("shared_test.txt");
    kprintln!("[test] Shared mapping communication test PASSED!");
}

#[test_case]
fn test_page_cache_isolation() {
    kprintln!("[test] Starting page cache isolation test...");
    // 1. Create and open file on ext2 via VFS directly
    let disk_dir = crate::fs::vfs::lookup("/disk").expect("Failed to lookup /disk");
    let _ = disk_dir.unlink("private_test.txt");
    let inode = disk_dir
        .create("private_test.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create private_test.txt");

    // 2. Write 4096 bytes directly using InodeOps::write to populate/extend it
    let data = [0u8; 4096];
    let written = inode
        .write(0, &data)
        .expect("Failed to write to private_test.txt");
    assert_eq!(written, 4096);

    // Allocate file descriptor manually
    let fd = crate::process::fd::current_task_alloc_fd(inode.clone())
        .expect("Failed to allocate file descriptor");

    // Map MAP_PRIVATE
    let addr_priv = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x02, fd, 0);
    assert!(addr_priv > 0);

    // Map MAP_SHARED (to monitor the underlying file/cache state)
    let addr_shared = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x01, fd, 0);
    assert!(addr_shared > 0);

    // Write to private mapping (will trigger COW page fault)
    let priv_ptr = addr_priv as *mut u64;
    unsafe {
        priv_ptr.write_volatile(0x1122334455667788);
    }

    // Verify private mapping has the new value
    let priv_val = unsafe { priv_ptr.read_volatile() };
    assert_eq!(priv_val, 0x1122334455667788);

    // Verify shared mapping still has 0 (isolation)
    let shared_ptr = addr_shared as *const u64;
    let shared_val = unsafe { shared_ptr.read_volatile() };
    assert_eq!(shared_val, 0);

    // Verify underlying file still has 0
    let mut read_buf = [0u8; 8];
    let read_res = inode
        .read(0, &mut read_buf)
        .expect("Failed to read from private_test.txt");
    assert_eq!(read_res, 8);
    let file_val = u64::from_ne_bytes(read_buf);
    assert_eq!(file_val, 0);

    // Clean up
    crate::syscall::memory::sys_munmap(addr_priv as u64, 4096);
    crate::syscall::memory::sys_munmap(addr_shared as u64, 4096);
    crate::process::fd::current_task_close_fd(fd);
    let _ = disk_dir.unlink("private_test.txt");
    kprintln!("[test] Page cache isolation test PASSED!");
}

#[test_case]
fn test_dirty_page_flush() {
    kprintln!("[test] Starting dirty page flush test...");
    // 1. Create and open file on ext2 via VFS directly
    let disk_dir = crate::fs::vfs::lookup("/disk").expect("Failed to lookup /disk");
    kprintln!("[test] Looked up /disk");
    let _ = disk_dir.unlink("flush_test.txt");
    kprintln!("[test] Unlinked if existed");
    let inode = disk_dir
        .create("flush_test.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create flush_test.txt");
    kprintln!("[test] Created file");

    // 2. Write 4096 bytes directly using InodeOps::write to populate/extend it
    let data = [0u8; 4096];
    let written = inode
        .write(0, &data)
        .expect("Failed to write to flush_test.txt");
    kprintln!("[test] Wrote 4096 bytes");
    assert_eq!(written, 4096);

    // Allocate file descriptor manually
    let fd = crate::process::fd::current_task_alloc_fd(inode.clone())
        .expect("Failed to allocate file descriptor");
    kprintln!("[test] Allocated fd: {}", fd);

    // Map MAP_SHARED
    let addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x01, fd, 0);
    kprintln!("[test] Called sys_mmap: {:#x}", addr);
    assert!(addr > 0);

    // Write magic value to the shared mapping
    let ptr = addr as *mut u64;
    unsafe {
        ptr.write_volatile(0x8877665544332211);
    }
    kprintln!("[test] Wrote magic value to mapping");

    // Verify that the disk still has 0 before fsync (since it's only in page cache / memory)
    let mut read_buf = [0u8; 8];
    let res = inode.read_direct(0, &mut read_buf);
    kprintln!("[test] Read direct from disk");
    assert!(res.is_ok());
    let val_before = u64::from_ne_bytes(read_buf);
    assert_eq!(val_before, 0);

    // Call fsync to commit changes
    kprintln!("[test] Calling sys_fsync");
    let fsync_res = crate::syscall::fs::sys_fsync(fd);
    kprintln!("[test] sys_fsync returned: {}", fsync_res);
    assert_eq!(fsync_res, 0);

    // Verify that the disk now has the magic value after fsync
    let res = inode.read_direct(0, &mut read_buf);
    assert!(res.is_ok());
    let val_after = u64::from_ne_bytes(read_buf);
    assert_eq!(val_after, 0x8877665544332211);

    // Clean up
    crate::syscall::memory::sys_munmap(addr as u64, 4096);
    crate::process::fd::current_task_close_fd(fd);
    let _ = disk_dir.unlink("flush_test.txt");
    kprintln!("[test] Dirty page flush test PASSED!");
}

#[test_case]
fn test_auxiliary_vectors() {
    kprintln!("[test] Starting auxiliary vector verification test...");
    let phys = crate::memory::physical::allocate_frame().expect("Failed to allocate frame");

    let argv = [alloc::string::String::from("test_arg")];
    let envp = [alloc::string::String::from("TEST_ENV=1")];
    let entry_point = 0x10002000;
    let phdr = 0x30004000;
    let phnum = 4;
    let phent = 56;
    let interpreter_base = 0x0000_7FFF_F7F0_0000;

    let user_sp = crate::process::elf::construct_user_stack(
        &argv,
        &envp,
        phys,
        entry_point,
        phdr,
        phnum,
        phent,
        interpreter_base,
    )
    .expect("Failed to construct user stack");

    // Read the stack from the allocated page.
    let page_virt = (phys + crate::memory::r#virtual::phys_mem_offset()) as *const u8;

    // The stack pointer returned is at some offset in the stack top.
    // Let's translate user_sp to the offset in our page.
    // stack top is USER_STACK_TOP = 0x0000_7FFF_FFFF_0000.
    // page is USER_STACK_TOP - 4096.
    let page_base_vaddr = crate::process::elf::USER_STACK_TOP - 4096;
    assert!(user_sp >= page_base_vaddr);
    assert!(user_sp < crate::process::elf::USER_STACK_TOP);
    let offset_in_page = (user_sp - page_base_vaddr) as usize;

    // Now let's parse the stack starting at `page_virt + offset_in_page`.
    // SAFETY: The stack_ptr is a valid pointer within the page boundary of the allocated frame.
    let stack_ptr = unsafe { page_virt.add(offset_in_page) } as *const u64;

    let argc = unsafe { stack_ptr.read() };
    assert_eq!(argc, 1);

    let arg0_ptr = unsafe { stack_ptr.add(1).read() };
    assert!(arg0_ptr > 0);

    let argv_null = unsafe { stack_ptr.add(2).read() };
    assert_eq!(argv_null, 0);

    let env0_ptr = unsafe { stack_ptr.add(3).read() };
    assert!(env0_ptr > 0);

    let envp_null = unsafe { stack_ptr.add(4).read() };
    assert_eq!(envp_null, 0);

    // The auxiliary vectors start at index 5.
    let mut aux_idx = 5;
    let mut found_phdr = false;
    let mut found_base = false;
    let mut found_entry = false;
    let mut found_phent = false;
    let mut found_phnum = false;
    let mut found_pagesz = false;
    let mut found_random = false;

    loop {
        let type_ = unsafe { stack_ptr.add(aux_idx).read() };
        let val = unsafe { stack_ptr.add(aux_idx + 1).read() };
        if type_ == 0 {
            break;
        }
        match type_ {
            3 => {
                // AT_PHDR
                assert_eq!(val, phdr);
                found_phdr = true;
            }
            4 => {
                // AT_PHENT
                assert_eq!(val, phent);
                found_phent = true;
            }
            5 => {
                // AT_PHNUM
                assert_eq!(val, phnum);
                found_phnum = true;
            }
            6 => {
                // AT_PAGESZ
                assert_eq!(val, 4096);
                found_pagesz = true;
            }
            7 => {
                // AT_BASE
                assert_eq!(val, interpreter_base);
                found_base = true;
            }
            9 => {
                // AT_ENTRY
                assert_eq!(val, entry_point);
                found_entry = true;
            }
            25 => {
                // AT_RANDOM
                assert!(val > 0);
                found_random = true;
            }
            _ => {}
        }
        aux_idx += 2;
    }

    assert!(found_phdr, "AT_PHDR not found or incorrect");
    assert!(found_phent, "AT_PHENT not found or incorrect");
    assert!(found_phnum, "AT_PHNUM not found or incorrect");
    assert!(found_pagesz, "AT_PAGESZ not found or incorrect");
    assert!(found_base, "AT_BASE not found or incorrect");
    assert!(found_entry, "AT_ENTRY not found or incorrect");
    assert!(found_random, "AT_RANDOM not found or incorrect");

    crate::memory::physical::deallocate_frame(phys);
    kprintln!("[test] Auxiliary vector verification test PASSED!");
}

#[test_case]
fn test_userspace_wrfsbase() {
    kprintln!("[test] Starting WRFSBASE verification test...");

    let test_val = 0x0000_1234_5678_9ABCu64;

    // Save current FS_BASE
    let orig_fs = x86_64::registers::model_specific::FsBase::read().as_u64();

    // Write new FS_BASE using wrfsbase instruction
    // SAFETY: Enabling FSGSBASE CR4 bit during early initialization guarantees
    // the wrfsbase instruction is supported and safe to execute.
    unsafe {
        core::arch::asm!(
            "wrfsbase {}",
            in(reg) test_val,
        );
    }

    // Read and verify
    let new_fs = x86_64::registers::model_specific::FsBase::read().as_u64();
    assert_eq!(new_fs, test_val);

    // Restore original FS_BASE
    // SAFETY: Restoring the original FS_BASE is required to maintain the kernel's thread state.
    unsafe {
        core::arch::asm!(
            "wrfsbase {}",
            in(reg) orig_fs,
        );
    }

    kprintln!("[test] WRFSBASE verification test PASSED!");
}

#[test_case]
fn test_eventfd() {
    kprintln!("[test] Starting eventfd test...");
    let fd = crate::fs::eventfd::sys_eventfd2(10, 0);
    assert!(fd >= 0);
    let fd = fd as i32;

    let inode = crate::process::fd::current_task_read_fd(fd).unwrap();
    let events = inode.poll(crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT);
    assert_eq!(events & crate::fs::inode::POLLIN, crate::fs::inode::POLLIN);
    assert_eq!(
        events & crate::fs::inode::POLLOUT,
        crate::fs::inode::POLLOUT
    );

    let mut buf = [0u8; 8];
    let n = inode.read(0, &mut buf).unwrap();
    assert_eq!(n, 8);
    let val = u64::from_ne_bytes(buf);
    assert_eq!(val, 10);

    let events2 = inode.poll(crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT);
    assert_eq!(events2 & crate::fs::inode::POLLIN, 0);
    assert_eq!(
        events2 & crate::fs::inode::POLLOUT,
        crate::fs::inode::POLLOUT
    );

    let write_buf = 5u64.to_ne_bytes();
    let n_write = inode.write(0, &write_buf).unwrap();
    assert_eq!(n_write, 8);

    let events3 = inode.poll(crate::fs::inode::POLLIN);
    assert_eq!(events3 & crate::fs::inode::POLLIN, crate::fs::inode::POLLIN);

    let mut buf2 = [0u8; 8];
    let n2 = inode.read(0, &mut buf2).unwrap();
    assert_eq!(n2, 8);
    let val2 = u64::from_ne_bytes(buf2);
    assert_eq!(val2, 5);

    crate::process::fd::current_task_close_fd(fd);
    kprintln!("[test] eventfd test PASSED!");
}

#[test_case]
fn test_timerfd() {
    kprintln!("[test] Starting timerfd test...");
    let epfd = crate::fs::epoll::sys_epoll_create1(0);
    assert!(epfd >= 0);
    let epfd = epfd as i32;

    let tfd = crate::fs::timerfd::sys_timerfd_create(0, 0);
    assert!(tfd >= 0);
    let tfd = tfd as i32;

    let mut ev = crate::fs::epoll::EpollEvent {
        events: crate::fs::inode::POLLIN,
        data: 999,
    };
    let res = crate::fs::epoll::sys_epoll_ctl(epfd, 1, tfd, &mut ev);
    assert_eq!(res, 0);

    let new_value = crate::fs::timerfd::Itimerspec {
        it_interval: crate::fs::timerfd::Timespec::default(),
        it_value: crate::fs::timerfd::Timespec {
            tv_sec: 0,
            tv_nsec: 10_000_000,
        },
    };
    let res = crate::fs::timerfd::sys_timerfd_settime(tfd, 0, &new_value, core::ptr::null_mut());
    assert_eq!(res, 0);

    let mut ready_evs = [crate::fs::epoll::EpollEvent::default(); 1];
    let n = crate::fs::epoll::sys_epoll_wait(epfd, ready_evs.as_mut_ptr(), 1, 100);
    assert_eq!(n, 1);
    let ev_data = ready_evs[0].data;
    let ev_events = ready_evs[0].events;
    assert_eq!(ev_data, 999);
    assert_eq!(
        ev_events & crate::fs::inode::POLLIN,
        crate::fs::inode::POLLIN
    );

    let mut buf = [0u8; 8];
    let inode = crate::process::fd::current_task_read_fd(tfd).unwrap();
    let n_read = inode.read(0, &mut buf).unwrap();
    assert_eq!(n_read, 8);
    let count = u64::from_ne_bytes(buf);
    assert_eq!(count, 1);

    crate::process::fd::current_task_close_fd(tfd);
    crate::process::fd::current_task_close_fd(epfd);
    kprintln!("[test] timerfd test PASSED!");
}

#[test_case]
fn test_signalfd() {
    kprintln!("[test] Starting signalfd test...");
    let mask = 1u64 << (10 - 1);
    let sfd = crate::fs::signalfd::sys_signalfd4(-1, &mask, 8, 0);
    assert!(sfd >= 0);
    let sfd = sfd as i32;

    let pid = crate::process::scheduler::current_pid().unwrap();
    crate::syscall::signal::deliver_signal(pid, 10);

    let inode = crate::process::fd::current_task_read_fd(sfd).unwrap();
    let events = inode.poll(crate::fs::inode::POLLIN);
    assert_eq!(events & crate::fs::inode::POLLIN, crate::fs::inode::POLLIN);

    let mut siginfo = crate::fs::signalfd::SignalFdSiginfo::default();
    let ptr = &mut siginfo as *mut crate::fs::signalfd::SignalFdSiginfo as *mut u8;
    let slice = unsafe {
        core::slice::from_raw_parts_mut(
            ptr,
            core::mem::size_of::<crate::fs::signalfd::SignalFdSiginfo>(),
        )
    };
    let n = inode.read(0, slice).unwrap();
    assert_eq!(
        n,
        core::mem::size_of::<crate::fs::signalfd::SignalFdSiginfo>()
    );
    assert_eq!(siginfo.ssi_signo, 10);

    let task_arc = crate::process::scheduler::get_task_arc(pid).unwrap();
    task_arc.lock().pending_signals &= !mask;

    crate::process::fd::current_task_close_fd(sfd);
    kprintln!("[test] signalfd test PASSED!");
}
