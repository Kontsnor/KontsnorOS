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
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_high));
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_normal));
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_low));
    assert_eq!(sched.pick_next(), None);

    // Clean up mock tasks from global TASKS list
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut tasks = crate::process::scheduler::TASKS.write();
        if tasks.len() > 12 {
            tasks[10] = None;
            tasks[11] = None;
            tasks[12] = None;
        }
    });
}

#[test_case]
fn test_orphan_reparenting() {
    let mut sched = crate::process::scheduler::Scheduler::new();

    // Save the original bootstrap thread (PID 1) from TASKS
    let original_init = x86_64::instructions::interrupts::without_interrupts(|| {
        let tasks = crate::process::scheduler::TASKS.read();
        tasks.get(1).cloned().flatten()
    });

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

    // Restore original bootstrap thread and clear mock parent/child
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut tasks = crate::process::scheduler::TASKS.write();
        if tasks.len() > 1 {
            tasks[1] = original_init;
        }
        if tasks.len() > 21 {
            tasks[20] = None;
            tasks[21] = None;
        }
    });
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
    // 1. Create and open file on ext via VFS directly
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

    // Write magic value in parent mapping (this faults in the lazy mapping)
    let ptr = addr1 as *mut u64;
    unsafe {
        ptr.write_volatile(0xDEADBEEF12345678);
    }
    kprintln!("[test] Wrote magic value to virtual ptr {:#x}", addr1);

    // 4. Simulate fork by cloning page table
    let current_pid = crate::process::scheduler::current_pid().unwrap();
    let parent_task_arc = crate::process::scheduler::get_task_arc(current_pid).unwrap();
    let (parent_cr3, mmap_regions) = {
        let task = parent_task_arc.lock();
        let addr_space = task.address_space.lock();
        (addr_space.page_table_root, addr_space.mmap_regions.clone())
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
    // 1. Create and open file on ext via VFS directly
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
    // 1. Create and open file on ext via VFS directly
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

#[test_case]
fn test_pseudo_filesystems() {
    kprintln!("[test] Starting pseudo-filesystems (sysfs, cgroupfs, securityfs) test...");

    // Test sysfs online CPUs
    let online_inode = crate::fs::vfs::lookup("/sys/devices/system/cpu/online")
        .expect("Failed to lookup /sys/devices/system/cpu/online");
    let mut buf = [0u8; 128];
    let n = online_inode
        .read(0, &mut buf)
        .expect("Failed to read CPU online file");
    let online_str = core::str::from_utf8(&buf[..n]).expect("Invalid UTF-8");
    assert!(!online_str.is_empty());
    kprintln!("[test] sysfs CPU online: {}", online_str.trim());

    // Test loopback MAC address
    let lo_inode = crate::fs::vfs::lookup("/sys/class/net/lo/address")
        .expect("Failed to lookup /sys/class/net/lo/address");
    let n_lo = lo_inode
        .read(0, &mut buf)
        .expect("Failed to read lo address");
    let lo_str = core::str::from_utf8(&buf[..n_lo]).expect("Invalid UTF-8");
    assert_eq!(lo_str, "00:00:00:00:00:00\n");
    kprintln!("[test] sysfs lo address: {}", lo_str.trim());

    // Test eth0 MAC address
    let eth0_inode = crate::fs::vfs::lookup("/sys/class/net/eth0/address")
        .expect("Failed to lookup /sys/class/net/eth0/address");
    let n_eth0 = eth0_inode
        .read(0, &mut buf)
        .expect("Failed to read eth0 address");
    let eth0_str = core::str::from_utf8(&buf[..n_eth0]).expect("Invalid UTF-8");
    assert!(!eth0_str.is_empty());
    assert!(eth0_str.ends_with('\n'));
    kprintln!("[test] sysfs eth0 address: {}", eth0_str.trim());

    // Test cgroupfs controllers
    let controllers_inode = crate::fs::vfs::lookup("/sys/fs/cgroup/cgroup.controllers")
        .expect("Failed to lookup /sys/fs/cgroup/cgroup.controllers");
    let n_ctrl = controllers_inode
        .read(0, &mut buf)
        .expect("Failed to read cgroup.controllers");
    let ctrl_str = core::str::from_utf8(&buf[..n_ctrl]).expect("Invalid UTF-8");
    assert_eq!(ctrl_str, "cpu memory io pids\n");
    kprintln!("[test] cgroup controllers: {}", ctrl_str.trim());

    // Test cgroupfs procs
    let procs_inode = crate::fs::vfs::lookup("/sys/fs/cgroup/cgroup.procs")
        .expect("Failed to lookup /sys/fs/cgroup/cgroup.procs");
    let n_procs = procs_inode
        .read(0, &mut buf)
        .expect("Failed to read cgroup.procs");
    let procs_str = core::str::from_utf8(&buf[..n_procs]).expect("Invalid UTF-8");
    assert!(!procs_str.is_empty());
    kprintln!("[test] cgroup active procs:\n{}", procs_str.trim());

    // Test securityfs (apparmor revision / profiles)
    let apparmor_rev_inode = crate::fs::vfs::lookup("/sys/kernel/security/apparmor/revision")
        .expect("Failed to lookup /sys/kernel/security/apparmor/revision");
    let n_rev = apparmor_rev_inode
        .read(0, &mut buf)
        .expect("Failed to read apparmor/revision");
    let rev_str = core::str::from_utf8(&buf[..n_rev]).expect("Invalid UTF-8");
    assert_eq!(rev_str, "0\n");

    // Test selinux stubs
    let selinux_enforce = crate::fs::vfs::lookup("/sys/fs/selinux/enforce")
        .expect("Failed to lookup /sys/fs/selinux/enforce");
    let n_enf = selinux_enforce
        .read(0, &mut buf)
        .expect("Failed to read selinux/enforce");
    let enf_str = core::str::from_utf8(&buf[..n_enf]).expect("Invalid UTF-8");
    assert_eq!(enf_str, "0\n");

    kprintln!("[test] pseudo-filesystems test PASSED!");
}

#[test_case]
fn test_ext4_extent_mapping() {
    kprintln!("[test] Starting Ext4 extent mapping test...");

    // Create a mock ramdisk block device and mount a minimal Ext filesystem
    let device = crate::drivers::ramdisk::create_ext2_ramdisk();
    let fs = crate::fs::ext::ExtFileSystem::mount(device.clone()).expect("Failed to mount ext");

    // Get inode 12 (hello.txt regular file)
    let inode = fs.get_ext_inode(12).expect("Failed to get ext inode");

    // Test Case 1: Leaf node extent mapping (eh_depth = 0)
    let mut i_block = [0u32; 15];
    let mut bytes = [0u8; 60];

    let header = crate::fs::ext::types::Ext4ExtentHeader {
        eh_magic: 0xF30A,
        eh_entries: 2,
        eh_max: 4,
        eh_depth: 0,
        eh_generation: 0,
    };

    let ext1 = crate::fs::ext::types::Ext4Extent {
        ee_block: 10,
        ee_len: 10,
        ee_start_hi: 0,
        ee_start_lo: 1000,
    };

    let ext2 = crate::fs::ext::types::Ext4Extent {
        ee_block: 30,
        ee_len: 5,
        ee_start_hi: 0,
        ee_start_lo: 5000,
    };

    // Write structures to bytes buffer
    // SAFETY: We write to a stack-allocated byte buffer of size 60 which is sufficiently large and aligned.
    unsafe {
        core::ptr::write_unaligned(
            bytes.as_mut_ptr() as *mut crate::fs::ext::types::Ext4ExtentHeader,
            header,
        );
        core::ptr::write_unaligned(
            bytes[12..].as_mut_ptr() as *mut crate::fs::ext::types::Ext4Extent,
            ext1,
        );
        core::ptr::write_unaligned(
            bytes[24..].as_mut_ptr() as *mut crate::fs::ext::types::Ext4Extent,
            ext2,
        );
    }

    // Pack bytes into i_block array
    for i in 0..15 {
        i_block[i] = u32::from_le_bytes([
            bytes[i * 4],
            bytes[i * 4 + 1],
            bytes[i * 4 + 2],
            bytes[i * 4 + 3],
        ]);
    }

    // Assert physical mappings
    assert_eq!(inode.resolve_extent_block(&i_block, 10).unwrap(), 1000);
    assert_eq!(inode.resolve_extent_block(&i_block, 15).unwrap(), 1005);
    assert_eq!(inode.resolve_extent_block(&i_block, 19).unwrap(), 1009);
    assert_eq!(inode.resolve_extent_block(&i_block, 20).unwrap(), 0); // Not mapped
    assert_eq!(inode.resolve_extent_block(&i_block, 30).unwrap(), 5000);
    assert_eq!(inode.resolve_extent_block(&i_block, 32).unwrap(), 5002);
    assert_eq!(inode.resolve_extent_block(&i_block, 34).unwrap(), 5004);
    assert_eq!(inode.resolve_extent_block(&i_block, 35).unwrap(), 0); // Not mapped

    // Test Case 2: Index-based extent tree mapping (eh_depth = 1)
    let root_header = crate::fs::ext::types::Ext4ExtentHeader {
        eh_magic: 0xF30A,
        eh_entries: 1,
        eh_max: 4,
        eh_depth: 1,
        eh_generation: 0,
    };

    let idx = crate::fs::ext::types::Ext4ExtentIdx {
        ei_block: 0,
        ei_leaf_lo: 60,
        ei_leaf_hi: 0,
        ei_unused: 0,
    };

    let mut root_bytes = [0u8; 60];
    // SAFETY: We write to a stack-allocated byte buffer of size 60 which is sufficiently large and aligned.
    unsafe {
        core::ptr::write_unaligned(
            root_bytes.as_mut_ptr() as *mut crate::fs::ext::types::Ext4ExtentHeader,
            root_header,
        );
        core::ptr::write_unaligned(
            root_bytes[12..].as_mut_ptr() as *mut crate::fs::ext::types::Ext4ExtentIdx,
            idx,
        );
    }

    let mut i_block_idx = [0u32; 15];
    for i in 0..15 {
        i_block_idx[i] = u32::from_le_bytes([
            root_bytes[i * 4],
            root_bytes[i * 4 + 1],
            root_bytes[i * 4 + 2],
            root_bytes[i * 4 + 3],
        ]);
    }

    // Prepare child block (at block 60 on the ramdisk)
    let child_header = crate::fs::ext::types::Ext4ExtentHeader {
        eh_magic: 0xF30A,
        eh_entries: 1,
        eh_max: 4,
        eh_depth: 0,
        eh_generation: 0,
    };

    let leaf = crate::fs::ext::types::Ext4Extent {
        ee_block: 0,
        ee_len: 5,
        ee_start_hi: 0,
        ee_start_lo: 9000,
    };

    let mut child_bytes = [0u8; 1024];
    // SAFETY: We write to a stack-allocated byte buffer of size 1024 which is sufficiently large and aligned.
    unsafe {
        core::ptr::write_unaligned(
            child_bytes.as_mut_ptr() as *mut crate::fs::ext::types::Ext4ExtentHeader,
            child_header,
        );
        core::ptr::write_unaligned(
            child_bytes[12..].as_mut_ptr() as *mut crate::fs::ext::types::Ext4Extent,
            leaf,
        );
    }

    // Write child block data to block 60 (sectors 120 and 121)
    device
        .write_block(120, &child_bytes[0..512])
        .expect("Write block 120 failed");
    device
        .write_block(121, &child_bytes[512..1024])
        .expect("Write block 121 failed");

    // Assert physical mapping through the index tree structure
    let resolved = inode.resolve_extent_block(&i_block_idx, 2).unwrap();
    assert_eq!(resolved, 9002);

    kprintln!("[test] Ext4 extent mapping test PASSED!");
}

#[test_case]
fn test_jbd2_journal_mount_check() {
    kprintln!("[test] Starting JBD2 journal mount check test...");

    // 1. Create a mock JBD2 superblock with correct magic and clean unmount flag (s_start = 0)
    let clean_jsb = crate::fs::ext::types::JournalSuperblock {
        s_header: crate::fs::ext::types::JournalHeader {
            h_magic: 0xC03B3998u32.to_be(),
            h_blocktype: 4u32.to_be(),
            h_sequence: 1u32.to_be(),
        },
        s_blocksize: 1024u32.to_be(),
        s_maxlen: 1000u32.to_be(),
        s_first: 1u32.to_be(),
        s_sequence: 1u32.to_be(),
        s_start: 0u32.to_be(), // 0 means cleanly unmounted
        s_errno: 0,
        s_feature_compat: 0,
        s_feature_incompat: 0,
        s_feature_ro_compat: 0,
        s_uuid: [0; 16],
        s_nr_users: 0,
        s_dynsuper: 0,
        s_max_transaction: 0,
        s_max_user_data: 0,
    };

    // 2. Validate clean journal
    let magic = u32::from_be(clean_jsb.s_header.h_magic);
    assert_eq!(magic, 0xC03B3998);
    let j_start = u32::from_be(clean_jsb.s_start);
    assert_eq!(j_start, 0); // clean

    // 3. Create a mock JBD2 superblock with correct magic but dirty unmount flag (s_start = 123)
    let dirty_jsb = crate::fs::ext::types::JournalSuperblock {
        s_header: crate::fs::ext::types::JournalHeader {
            h_magic: 0xC03B3998u32.to_be(),
            h_blocktype: 4u32.to_be(),
            h_sequence: 1u32.to_be(),
        },
        s_blocksize: 1024u32.to_be(),
        s_maxlen: 1000u32.to_be(),
        s_first: 1u32.to_be(),
        s_sequence: 1u32.to_be(),
        s_start: 123u32.to_be(), // non-zero means dirty/active transactions
        s_errno: 0,
        s_feature_compat: 0,
        s_feature_incompat: 0,
        s_feature_ro_compat: 0,
        s_uuid: [0; 16],
        s_nr_users: 0,
        s_dynsuper: 0,
        s_max_transaction: 0,
        s_max_user_data: 0,
    };

    let j_start_dirty = u32::from_be(dirty_jsb.s_start);
    assert_eq!(j_start_dirty, 123); // dirty

    // 4. Create a superblock with invalid magic
    let invalid_jsb = crate::fs::ext::types::JournalSuperblock {
        s_header: crate::fs::ext::types::JournalHeader {
            h_magic: 0xDEADBEEFu32.to_be(),
            h_blocktype: 4u32.to_be(),
            h_sequence: 1u32.to_be(),
        },
        s_blocksize: 1024u32.to_be(),
        s_maxlen: 1000u32.to_be(),
        s_first: 1u32.to_be(),
        s_sequence: 1u32.to_be(),
        s_start: 0,
        s_errno: 0,
        s_feature_compat: 0,
        s_feature_incompat: 0,
        s_feature_ro_compat: 0,
        s_uuid: [0; 16],
        s_nr_users: 0,
        s_dynsuper: 0,
        s_max_transaction: 0,
        s_max_user_data: 0,
    };
    let magic_invalid = u32::from_be(invalid_jsb.s_header.h_magic);
    assert_ne!(magic_invalid, 0xC03B3998);

    kprintln!("[test] JBD2 journal mount check test PASSED!");
}

#[test_case]
fn test_ahci_controller_initialization() {
    kprintln!("[test] Starting AHCI Controller Initialization test...");

    // Allocate a mock register space on the heap (5000 bytes to fit 32 ports and control registers)
    let mut mock_registers = alloc::vec![0u8; 5000];
    let virt_base = mock_registers.as_mut_ptr() as u64;

    // Set Ports Implemented (PI) to 0x0000_0005 (ports 0 and 2 are active/implemented)
    let pi_offset = crate::drivers::block::ahci::HOST_PI as usize;
    unsafe {
        let pi_ptr = (virt_base + pi_offset as u64) as *mut u32;
        pi_ptr.write_volatile(0x0000_0005);
    }

    // Call init_controller_at
    let pi = unsafe { crate::drivers::block::ahci::test_helpers::init_controller_at(virt_base) };

    // Verify Ports Implemented
    assert_eq!(pi, 0x0000_0005);

    // Verify GHC has AE (AHCI Enable = bit 31) and IE (Interrupt Enable = bit 1) set
    let ghc_offset = crate::drivers::block::ahci::HOST_GHC as usize;
    let ghc = unsafe { ((virt_base + ghc_offset as u64) as *const u32).read_volatile() };
    assert_ne!(ghc & (1 << 31), 0);
    assert_ne!(ghc & (1 << 1), 0);

    kprintln!("[test] AHCI Controller Initialization test PASSED!");
}

#[test_case]
fn test_ahci_port_connection() {
    kprintln!("[test] Starting AHCI Port Connection test...");

    // Allocate mock register space
    let mut mock_registers = alloc::vec![0u8; 5000];
    let virt_base = mock_registers.as_mut_ptr() as u64;

    let port_idx = 2;
    let port_base = 0x100 + port_idx * 0x80;

    // Set SSTS of port 2 to 3 (device detected and PHY established)
    unsafe {
        let ssts_ptr = (virt_base
            + port_base as u64
            + crate::drivers::block::ahci::PORT_SSTS as u64) as *mut u32;
        ssts_ptr.write_volatile(3);
    }

    // Mock physical addresses for command list and FIS
    let cl_phys = 0x1000_2000;
    let fis_phys = 0x3000_4000;

    // Initialize port 2
    unsafe {
        crate::drivers::block::ahci::test_helpers::init_port_at(
            virt_base, port_idx, cl_phys, fis_phys,
        );
    }

    // Assert that the command list and FIS base addresses were written correctly
    let clb = unsafe {
        ((virt_base + port_base as u64 + crate::drivers::block::ahci::PORT_CLB as u64)
            as *const u32)
            .read_volatile()
    };
    let clbu = unsafe {
        ((virt_base + port_base as u64 + crate::drivers::block::ahci::PORT_CLBU as u64)
            as *const u32)
            .read_volatile()
    };
    let fb = unsafe {
        ((virt_base + port_base as u64 + crate::drivers::block::ahci::PORT_FB as u64) as *const u32)
            .read_volatile()
    };
    let fbu = unsafe {
        ((virt_base + port_base as u64 + crate::drivers::block::ahci::PORT_FBU as u64)
            as *const u32)
            .read_volatile()
    };
    let cl_phys_read = clb as u64 | ((clbu as u64) << 32);
    let fis_phys_read = fb as u64 | ((fbu as u64) << 32);

    assert_eq!(cl_phys_read, cl_phys);
    assert_eq!(fis_phys_read, fis_phys);

    // Assert that port 2 CMD register has FRE (0x10) and ST (0x01) bits set
    let cmd = unsafe {
        ((virt_base + port_base as u64 + crate::drivers::block::ahci::PORT_CMD as u64)
            as *const u32)
            .read_volatile()
    };
    assert_ne!(cmd & 0x0010, 0); // FRE set
    assert_ne!(cmd & 0x0001, 0); // ST set

    kprintln!("[test] AHCI Port Connection test PASSED!");
}

#[test_case]
fn test_nvme_controller_initialization() {
    kprintln!("[test] Starting NVMe Controller Initialization test...");

    // Allocate a mock register space on the heap (8192 bytes for MMIO registers)
    let mut mock_registers = alloc::vec![0u8; 8192];
    let virt_base = mock_registers.as_mut_ptr() as u64;

    unsafe {
        // Test VS register (0x08)
        crate::drivers::block::nvme::test_helpers::write_reg32(
            virt_base,
            crate::drivers::block::nvme::VS,
            0x00010300,
        ); // VS = 1.3.0
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg32(
                virt_base,
                crate::drivers::block::nvme::VS
            ),
            0x00010300
        );

        // Test CAP register (0x00) - 8 bytes
        crate::drivers::block::nvme::test_helpers::write_reg64(
            virt_base,
            crate::drivers::block::nvme::CAP,
            0x0014000300020001,
        );
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg64(
                virt_base,
                crate::drivers::block::nvme::CAP
            ),
            0x0014000300020001
        );

        // Test CC register (0x14)
        crate::drivers::block::nvme::test_helpers::write_reg32(
            virt_base,
            crate::drivers::block::nvme::CC,
            0x00460001,
        ); // CC.EN = 1, IOSQES=6, IOCQES=4
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg32(
                virt_base,
                crate::drivers::block::nvme::CC
            ),
            0x00460001
        );

        // Test AQA register (0x24)
        crate::drivers::block::nvme::test_helpers::write_reg32(
            virt_base,
            crate::drivers::block::nvme::AQA,
            (63 << 16) | 63,
        );
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg32(
                virt_base,
                crate::drivers::block::nvme::AQA
            ),
            (63 << 16) | 63
        );

        // Test ASQ and ACQ registers (0x28, 0x30) - 8 bytes
        crate::drivers::block::nvme::test_helpers::write_reg64(
            virt_base,
            crate::drivers::block::nvme::ASQ,
            0x10002000,
        );
        crate::drivers::block::nvme::test_helpers::write_reg64(
            virt_base,
            crate::drivers::block::nvme::ACQ,
            0x30004000,
        );
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg64(
                virt_base,
                crate::drivers::block::nvme::ASQ
            ),
            0x10002000
        );
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg64(
                virt_base,
                crate::drivers::block::nvme::ACQ
            ),
            0x30004000
        );
    }

    kprintln!("[test] NVMe Controller Initialization test PASSED!");
}

#[test_case]
fn test_nvme_identify_parsing() {
    kprintln!("[test] Starting NVMe Identify Parsing test...");

    // Allocate simulated identify namespace buffer (4096 bytes)
    let mut identify_buf = alloc::vec![0u8; 4096];

    // NSZE (Namespace Size) at offset 0 (8 bytes) = 0x0000_0000_1234_5678 (305,419,896 sectors)
    let expected_nsze: u64 = 0x12345678;
    identify_buf[0..8].copy_from_slice(&expected_nsze.to_ne_bytes());

    // FLBAS (Formatted LBA Size) at offset 27 (1 byte) = 0
    // Index 0 in LBA format table will be active
    identify_buf[27] = 0;

    // LBA Format table starts at offset 128
    // LBA Format 0 at bytes 128..132:
    // bits 16..23 is LBADS (LBA Data Size). If LBADS = 9 (2^9 = 512 bytes)
    let lbads: u8 = 9;
    let lbads_word = (lbads as u32) << 16;
    identify_buf[128..132].copy_from_slice(&lbads_word.to_ne_bytes());

    // Parse just like the driver would
    let nsze = u64::from_ne_bytes([
        identify_buf[0],
        identify_buf[1],
        identify_buf[2],
        identify_buf[3],
        identify_buf[4],
        identify_buf[5],
        identify_buf[6],
        identify_buf[7],
    ]);
    let flbas = identify_buf[27];
    let lbaf_idx = (flbas & 0x0F) as usize;

    let lbaf_offset = 128 + lbaf_idx * 4;
    let lbaf_entry = u32::from_ne_bytes([
        identify_buf[lbaf_offset],
        identify_buf[lbaf_offset + 1],
        identify_buf[lbaf_offset + 2],
        identify_buf[lbaf_offset + 3],
    ]);
    let parsed_lbads = ((lbaf_entry >> 16) & 0xFF) as u8;
    let block_size = if parsed_lbads >= 9 && parsed_lbads <= 16 {
        1u64 << parsed_lbads
    } else {
        512
    };

    assert_eq!(nsze, expected_nsze);
    assert_eq!(block_size, 512);

    // Test a different LBA size: LBADS = 12 (2^12 = 4096 bytes)
    let lbads_12: u8 = 12;
    let lbads_word_12 = (lbads_12 as u32) << 16;
    identify_buf[128..132].copy_from_slice(&lbads_word_12.to_ne_bytes());

    let lbaf_entry_12 = u32::from_ne_bytes([
        identify_buf[128],
        identify_buf[129],
        identify_buf[130],
        identify_buf[131],
    ]);
    let parsed_lbads_12 = ((lbaf_entry_12 >> 16) & 0xFF) as u8;
    let block_size_12 = if parsed_lbads_12 >= 9 && parsed_lbads_12 <= 16 {
        1u64 << parsed_lbads_12
    } else {
        512
    };
    assert_eq!(block_size_12, 4096);

    kprintln!("[test] NVMe Identify Parsing test PASSED!");
}

static FUTEX_ADDR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn futex_helper_thread() {
    kprintln!("[test_futex] Helper thread started, waiting for futex address...");
    let addr = loop {
        let a = FUTEX_ADDR.load(core::sync::atomic::Ordering::SeqCst);
        if a != 0 {
            break a;
        }
        crate::process::scheduler::yield_now();
    };

    // Yield a few times to let the main test thread call FUTEX_WAIT and block
    for _ in 0..10 {
        crate::process::scheduler::yield_now();
    }

    kprintln!("[test_futex] Helper thread waking futex at {:#x}", addr);
    let woken = crate::syscall::process::futex::sys_futex(
        addr as *mut i32,
        1, // FUTEX_WAKE
        1, // Wake 1 task
        0,
        core::ptr::null_mut(),
        0,
    );
    kprintln!("[test_futex] Helper thread woke {} task(s)", woken);
}

#[test_case]
fn test_futex_wait_wake() {
    kprintln!("[test] Starting Futex Wait/Wake test...");
    FUTEX_ADDR.store(0, core::sync::atomic::Ordering::SeqCst);

    // 1. Allocate mapped address for futex variable
    let addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0);
    assert!(addr > 0);
    let uaddr = addr as *mut i32;

    // Set value to 42
    unsafe {
        uaddr.write_volatile(42);
    }

    // 2. Spawn helper thread
    let _helper_pid = crate::process::spawn_kernel_thread(
        alloc::string::String::from("futex_helper"),
        futex_helper_thread,
    );

    // 3. Store address so the helper thread can find it
    FUTEX_ADDR.store(addr as u64, core::sync::atomic::Ordering::SeqCst);

    // 4. Call FUTEX_WAIT. This should block the current thread until woken by the helper.
    kprintln!("[test] Main thread calling FUTEX_WAIT on {:#x}...", addr);
    let res = crate::syscall::process::futex::sys_futex(
        uaddr,
        0,  // FUTEX_WAIT
        42, // Expected value
        0,
        core::ptr::null_mut(),
        0,
    );
    assert_eq!(res, 0);
    kprintln!("[test] Main thread woke up successfully from FUTEX_WAIT!");

    kprintln!("[test] Futex Wait/Wake test PASSED!");
}

#[test_case]
fn test_thread_clone_vm() {
    kprintln!("[test] Starting Thread Shared VM test...");
    let mut sched = crate::process::scheduler::Scheduler::new();

    let pid1 = crate::process::pid::Pid::from_raw(40);
    let mut task1 =
        crate::process::task::Task::new(pid1, alloc::string::String::from("thread1"), 0);
    task1.state = crate::process::task::TaskState::Ready;

    let pid2 = crate::process::pid::Pid::from_raw(41);
    let mut task2 =
        crate::process::task::Task::new(pid2, alloc::string::String::from("thread2"), 0);
    task2.state = crate::process::task::TaskState::Ready;

    // Share address space
    task2.address_space = task1.address_space.clone();

    sched.add_task(task1);
    sched.add_task(task2);

    // Modify brk in thread1
    {
        let t1_arc = crate::process::scheduler::get_task_arc(pid1).unwrap();
        let mut t1 = t1_arc.lock();
        t1.address_space.lock().brk = 0x1000;
    }

    // Verify read in thread2
    {
        let t2_arc = crate::process::scheduler::get_task_arc(pid2).unwrap();
        let t2 = t2_arc.lock();
        assert_eq!(t2.address_space.lock().brk, 0x1000);
    }

    kprintln!("[test] Thread Shared VM test PASSED!");

    // Clean up mock tasks from global TASKS list
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut tasks = crate::process::scheduler::TASKS.write();
        if tasks.len() > 41 {
            tasks[40] = None;
            tasks[41] = None;
        }
    });
}

static STRESS_FUTEX_ADDR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static STRESS_THREADS_ACTIVE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
static STRESS_THREAD_ID: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

fn stress_helper_thread() {
    let id = STRESS_THREAD_ID.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    let addr = loop {
        let a = STRESS_FUTEX_ADDR.load(core::sync::atomic::Ordering::SeqCst);
        if a != 0 {
            break a;
        }
        crate::process::scheduler::yield_now();
    };

    let uaddr = addr as *mut i32;

    for step in 0..100 {
        // Yield sometimes to cause scheduling pressure
        if step % 5 == 0 {
            crate::process::scheduler::yield_now();
        }

        // Print to standard output (competing for serial output lock)
        kprintln!(
            "[stress] Core {} - Thread {} step {}",
            crate::arch::x86_64::smp::current_lapic_id(),
            id,
            step
        );

        if id % 2 == 0 {
            // Even threads call FUTEX_WAIT if value is still 0 (which it might be)
            unsafe {
                let current_val = uaddr.read_volatile();
                if current_val == 0 {
                    let _ = crate::syscall::process::futex::sys_futex(
                        uaddr,
                        0, // FUTEX_WAIT
                        0, // Expected value
                        0,
                        core::ptr::null_mut(),
                        0,
                    );
                } else {
                    // Reset value and wake other threads
                    uaddr.write_volatile(0);
                    let _ = crate::syscall::process::futex::sys_futex(
                        uaddr,
                        1, // FUTEX_WAKE
                        1, // Wake 1
                        0,
                        core::ptr::null_mut(),
                        0,
                    );
                }
            }
        } else {
            // Odd threads set value to 1 and wake others
            unsafe {
                uaddr.write_volatile(1);
                let _ = crate::syscall::process::futex::sys_futex(
                    uaddr,
                    1, // FUTEX_WAKE
                    1, // Wake 1
                    0,
                    core::ptr::null_mut(),
                    0,
                );
            }
        }
    }

    kprintln!("[stress] Thread {} finished.", id);
    STRESS_THREADS_ACTIVE.fetch_sub(1, core::sync::atomic::Ordering::SeqCst);
}

#[test_case]
fn test_multicore_deadlock_stress() {
    kprintln!("[test] Starting Multi-Core Deadlock Stress Test...");
    STRESS_FUTEX_ADDR.store(0, core::sync::atomic::Ordering::SeqCst);
    STRESS_THREAD_ID.store(0, core::sync::atomic::Ordering::SeqCst);

    let thread_count = 8;
    STRESS_THREADS_ACTIVE.store(thread_count, core::sync::atomic::Ordering::SeqCst);

    // 1. Allocate shared futex page
    let addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0);
    assert!(addr > 0);
    let uaddr = addr as *mut i32;
    unsafe {
        uaddr.write_volatile(0);
    }

    // 2. Spawn helper threads
    for _ in 0..thread_count {
        crate::process::spawn_kernel_thread(
            alloc::string::String::from("stress_thread"),
            stress_helper_thread,
        );
    }

    // 3. Enable threads to proceed
    STRESS_FUTEX_ADDR.store(addr as u64, core::sync::atomic::Ordering::SeqCst);

    // 4. Wait for all threads to finish while yielding and printing
    let mut main_step = 0;
    while STRESS_THREADS_ACTIVE.load(core::sync::atomic::Ordering::SeqCst) > 0 {
        main_step += 1;
        if main_step % 20 == 0 {
            kprintln!(
                "[stress] Main thread monitoring (remaining: {})...",
                STRESS_THREADS_ACTIVE.load(core::sync::atomic::Ordering::SeqCst)
            );
        }
        // Force the futex wake sometimes from the main thread
        unsafe {
            uaddr.write_volatile(1);
            let _ = crate::syscall::process::futex::sys_futex(
                uaddr,
                1, // FUTEX_WAKE
                8, // Wake all
                0,
                core::ptr::null_mut(),
                0,
            );
        }
        crate::process::scheduler::yield_now();
    }

    // Clean up
    crate::syscall::memory::sys_munmap(addr as u64, 4096);
    kprintln!("[test] Multi-Core Deadlock Stress Test PASSED!");
}

#[test_case]
fn test_sys_mremap() {
    kprintln!("[test] Starting sys_mremap verification test...");

    // 1. Create a private anonymous mapping of 1 page (4 KiB)
    let addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64; // PROT_READ|WRITE, MAP_PRIVATE|ANON
    assert!(addr > 0);

    // Write some data to the page
    let ptr = addr as *mut u64;
    unsafe {
        ptr.write_volatile(0x123456789ABCDEF0);
    }

    // 2. Grow the mapping in-place (from 4 KiB to 8 KiB)
    let grew_addr = crate::syscall::memory::sys_mremap(addr, 4096, 8192, 0, 0) as u64;
    assert_eq!(grew_addr, addr); // Should grow in-place as nothing is next to it

    // Verify original data is preserved
    let val = unsafe { ptr.read_volatile() };
    assert_eq!(val, 0x123456789ABCDEF0);

    // Verify new page is accessible (write to it)
    let ptr2 = (addr + 4096) as *mut u64;
    unsafe {
        ptr2.write_volatile(0xDEADC0DECAFEBABE);
    }
    let val2 = unsafe { ptr2.read_volatile() };
    assert_eq!(val2, 0xDEADC0DECAFEBABE);

    // 3. Move the mapping using MREMAP_MAYMOVE (force move by allocating at a different address)
    // First, let's allocate a dummy block right after our grew block to block in-place growth,
    // then mremap with new size 16 KiB.
    let dummy = crate::syscall::memory::sys_mmap(addr + 8192, 4096, 3, 0x22, -1, 0) as u64;
    assert_eq!(dummy, addr + 8192);

    let moved_addr = crate::syscall::memory::sys_mremap(addr, 8192, 16384, 1, 0) as u64; // MREMAP_MAYMOVE = 1
    assert!(moved_addr > 0);
    assert_ne!(moved_addr, addr); // Must have moved because of dummy mapping

    // Verify original data is preserved at the new address
    let moved_ptr = moved_addr as *mut u64;
    let val_moved = unsafe { moved_ptr.read_volatile() };
    assert_eq!(val_moved, 0x123456789ABCDEF0);

    let moved_ptr2 = (moved_addr + 4096) as *mut u64;
    let val_moved2 = unsafe { moved_ptr2.read_volatile() };
    assert_eq!(val_moved2, 0xDEADC0DECAFEBABE);

    // 4. Shrink the mapping (from 16 KiB to 4 KiB)
    let shrunk_addr = crate::syscall::memory::sys_mremap(moved_addr, 16384, 4096, 0, 0) as u64;
    assert_eq!(shrunk_addr, moved_addr);

    // Original data should still be there
    let val_shrunk = unsafe { moved_ptr.read_volatile() };
    assert_eq!(val_shrunk, 0x123456789ABCDEF0);

    // Clean up
    crate::syscall::memory::sys_munmap(shrunk_addr, 4096);
    crate::syscall::memory::sys_munmap(dummy, 4096);

    kprintln!("[test] sys_mremap verification test PASSED!");
}

static mut BITSET_FUTEX_ADDR: u64 = 0;
static BITSET_WOKE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn futex_bitset_helper_thread() {
    let uaddr = unsafe { BITSET_FUTEX_ADDR as *mut i32 };
    kprintln!("[test] Helper thread waiting on futex with bitset 0x1...");
    let res = crate::syscall::process::sys_futex(
        uaddr,
        9, // FUTEX_WAIT_BITSET
        0, // Expected val
        0,
        core::ptr::null_mut(),
        1, // Bitset = 0x1
    );
    assert_eq!(res, 0);
    kprintln!("[test] Helper thread woke up!");
    BITSET_WOKE.store(true, core::sync::atomic::Ordering::SeqCst);
}

#[test_case]
fn test_futex_bitset_and_cleartid() {
    kprintln!("[test] Starting futex bitset and CLONE_CHILD_CLEARTID verification test...");

    // 1. FUTEX_WAIT_BITSET and FUTEX_WAKE_BITSET test
    BITSET_WOKE.store(false, core::sync::atomic::Ordering::SeqCst);
    let addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    assert!(addr > 0);
    let uaddr = addr as *mut i32;
    unsafe {
        uaddr.write_volatile(0);
        BITSET_FUTEX_ADDR = addr;
    }

    crate::process::spawn_kernel_thread(
        alloc::string::String::from("futex_bitset_helper"),
        futex_bitset_helper_thread,
    );

    // Let the helper thread start and wait
    for _ in 0..10 {
        crate::process::scheduler::yield_now();
    }

    // Try to wake with a non-matching bitset 0x2 (should wake 0 threads)
    let woken = crate::syscall::process::sys_futex(
        uaddr,
        10, // FUTEX_WAKE_BITSET
        1,  // Wake 1
        0,
        core::ptr::null_mut(),
        2, // Bitset = 0x2
    );
    assert_eq!(woken, 0);
    assert_eq!(
        BITSET_WOKE.load(core::sync::atomic::Ordering::SeqCst),
        false
    );

    // Wake with matching bitset 0x1 (should wake 1 thread)
    let woken2 = crate::syscall::process::sys_futex(
        uaddr,
        10, // FUTEX_WAKE_BITSET
        1,  // Wake 1
        0,
        core::ptr::null_mut(),
        1, // Bitset = 0x1
    );
    assert_eq!(woken2, 1);

    // Yield to let the helper thread finish
    for _ in 0..10 {
        crate::process::scheduler::yield_now();
    }
    assert_eq!(BITSET_WOKE.load(core::sync::atomic::Ordering::SeqCst), true);

    // 2. CLONE_CHILD_CLEARTID test
    // Create a mock task and exit it, checking that clear_child_tid clears user memory and wakes
    let pid_child = crate::process::pid::allocate();
    let mut task_child =
        crate::process::task::Task::new(pid_child, alloc::string::String::from("mock_child"), 0);

    // Set up clear_child_tid pointing to our uaddr
    unsafe {
        uaddr.write_volatile(999);
    }
    task_child.clear_child_tid = Some(addr);
    task_child.state = crate::process::task::TaskState::Ready;

    // We need to wait on this address
    // Let's spawn a helper thread that waits on uaddr (expected val 0)
    // Wait, first the helper thread waits using FUTEX_WAIT on uaddr (which is 0 once cleared)
    // Let's set uaddr to 999. The helper thread will wait with expected val 999.
    // When the child task exits, it writes 0 to uaddr and wakes futex.
    struct JoinWaiter {
        woke: core::sync::atomic::AtomicBool,
    }
    static JOIN_WOKE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

    fn join_waiter_thread() {
        let uaddr = unsafe { BITSET_FUTEX_ADDR as *mut i32 };
        kprintln!("[test] Join waiter thread waiting on TID clear...");
        let res = crate::syscall::process::sys_futex(
            uaddr,
            0,   // FUTEX_WAIT
            999, // Expected val before exit
            0,
            core::ptr::null_mut(),
            0,
        );
        // Wait, if it returns EAGAIN (because it was already cleared before wait), or returns 0 (normal wake), it's fine.
        kprintln!("[test] Join waiter thread woke! res={}", res);
        JOIN_WOKE.store(true, core::sync::atomic::Ordering::SeqCst);
    }

    JOIN_WOKE.store(false, core::sync::atomic::Ordering::SeqCst);
    crate::process::spawn_kernel_thread(
        alloc::string::String::from("join_waiter"),
        join_waiter_thread,
    );

    // Let helper start
    for _ in 0..10 {
        crate::process::scheduler::yield_now();
    }

    // Now exit the mock child task via the scheduler
    let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
    let sched = sched_lock.as_mut().unwrap();

    // Add child task to scheduler so it exists in TASKS
    sched.add_task(task_child);

    // Exit it
    let fds = sched.exit_task(pid_child, 0);
    drop(sched_lock);
    drop(fds);

    // Yield to let join_waiter run
    for _ in 0..20 {
        crate::process::scheduler::yield_now();
    }

    // Verify uaddr was cleared to 0
    let val_after = unsafe { uaddr.read_volatile() };
    assert_eq!(val_after, 0);

    // Verify join waiter was woken
    assert_eq!(JOIN_WOKE.load(core::sync::atomic::Ordering::SeqCst), true);

    // Clean up
    crate::syscall::memory::sys_munmap(addr, 4096);

    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut tasks = crate::process::scheduler::TASKS.write();
        let idx = pid_child.as_u64() as usize;
        if idx < tasks.len() {
            tasks[idx] = None;
        }
    });

    kprintln!("[test] futex bitset and CLONE_CHILD_CLEARTID verification test PASSED!");
}

#[test_case]
fn test_phase2_features() {
    kprintln!("[test] Starting Phase 2 features verification...");

    // --- 1. /dev/urandom & /dev/random ---
    kprintln!("[test] 1. Looking up /dev/urandom");
    let urandom = crate::fs::vfs::lookup("/dev/urandom").expect("/dev/urandom missing");
    let mut rand_buf1 = [0u8; 16];
    let mut rand_buf2 = [0u8; 16];
    kprintln!("[test] 1. Reading /dev/urandom 1");
    let r1 = urandom
        .read(0, &mut rand_buf1)
        .expect("read /dev/urandom failed");
    kprintln!("[test] 1. Reading /dev/urandom 2");
    let r2 = urandom
        .read(0, &mut rand_buf2)
        .expect("read /dev/urandom failed");
    assert_eq!(r1, 16);
    assert_eq!(r2, 16);
    // Highly unlikely that two random buffers of 16 bytes are identical or all zero
    assert_ne!(rand_buf1, rand_buf2);
    assert_ne!(rand_buf1, [0u8; 16]);

    kprintln!("[test] 1. Looking up /dev/random");
    let random = crate::fs::vfs::lookup("/dev/random").expect("/dev/random missing");
    kprintln!("[test] 1. Reading /dev/random");
    let r3 = random
        .read(0, &mut rand_buf1)
        .expect("read /dev/random failed");
    assert_eq!(r3, 16);

    // --- 2. /proc/self/fd ---
    kprintln!("[test] 2. Looking up /proc/self/fd");
    let fd_dir = crate::fs::vfs::lookup("/proc/self/fd").expect("/proc/self/fd missing");
    kprintln!("[test] 2. Reading directory /proc/self/fd");
    let entries = fd_dir.readdir();
    // Must contain stdin (0), stdout (1), stderr (2)
    let mut has_stdin = false;
    let mut has_stdout = false;
    let mut has_stderr = false;
    for entry in &entries {
        if entry.name == "0" {
            has_stdin = true;
        }
        if entry.name == "1" {
            has_stdout = true;
        }
        if entry.name == "2" {
            has_stderr = true;
        }
    }
    assert!(has_stdin);
    assert!(has_stdout);
    assert!(has_stderr);

    // Read link value
    kprintln!("[test] 2. Looking up /proc/self/fd/0");
    let fd0_link =
        crate::fs::vfs::lookup_follow("/proc/self/fd/0", false).expect("/proc/self/fd/0 missing");
    kprintln!("[test] 2. Reading /proc/self/fd/0");
    let mut link_buf = [0u8; 64];
    let link_len = fd0_link
        .read(0, &mut link_buf)
        .expect("readlink /proc/self/fd/0 failed");
    let link_str = core::str::from_utf8(&link_buf[..link_len]).unwrap();
    assert_eq!(link_str, "/dev/stdin");

    // --- 3. Clocks ---
    kprintln!("[test] 3. Starting Clock tests");
    let mut ts_mono1 = [0u8; 16];
    let mut ts_mono2 = [0u8; 16];
    let ret1 = crate::syscall::process::sys_clock_gettime(1, ts_mono1.as_mut_ptr()); // CLOCK_MONOTONIC
    assert_eq!(ret1, 0);
    // Yield a bit
    for _ in 0..10 {
        crate::process::scheduler::yield_now();
    }
    let ret2 = crate::syscall::process::sys_clock_gettime(1, ts_mono2.as_mut_ptr());
    assert_eq!(ret2, 0);

    // SAFETY: ts_mono1 and ts_mono2 are properly written 16 bytes.
    let sec1 = unsafe { *(ts_mono1.as_ptr() as *const i64) };
    let nsec1 = unsafe { *(ts_mono1.as_ptr().add(8) as *const i64) };
    let sec2 = unsafe { *(ts_mono2.as_ptr() as *const i64) };
    let nsec2 = unsafe { *(ts_mono2.as_ptr().add(8) as *const i64) };
    let t1 = sec1 * 1_000_000_000 + nsec1;
    let t2 = sec2 * 1_000_000_000 + nsec2;
    assert!(t2 >= t1);

    // --- 4. File Advisory Locking (flock & fcntl) ---
    let tmp_dir = crate::fs::vfs::lookup("/tmp").unwrap();
    let _ = tmp_dir.unlink("lock_test.txt");
    let file = tmp_dir
        .create("lock_test.txt", crate::fs::inode::FileType::Regular)
        .unwrap();

    // Write some bytes so fcntl range locking works on offset
    let dummy_data = [0u8; 100];
    file.write(0, &dummy_data).unwrap();

    let fd1 = crate::process::fd::current_task_alloc_fd(file.clone()).unwrap();
    let fd2 = crate::process::fd::current_task_alloc_fd(file.clone()).unwrap();

    // Try flock LOCK_EX on fd1
    let r_lock1 = crate::syscall::fs::sys_flock(fd1, 2); // LOCK_EX
    assert_eq!(r_lock1, 0);

    // Try flock LOCK_EX | LOCK_NB on fd2 -> should fail with EAGAIN (-11)
    let r_lock2 = crate::syscall::fs::sys_flock(fd2, 2 | 4); // LOCK_EX | LOCK_NB
    assert_eq!(r_lock2, -11); // -EAGAIN

    // Try flock LOCK_UN on fd1
    let r_unlock = crate::syscall::fs::sys_flock(fd1, 8); // LOCK_UN
    assert_eq!(r_unlock, 0);

    // Try flock LOCK_EX on fd2 now -> should succeed
    let r_lock3 = crate::syscall::fs::sys_flock(fd2, 2); // LOCK_EX
    assert_eq!(r_lock3, 0);

    // Cleanup fd2 lock
    crate::syscall::fs::sys_flock(fd2, 8);

    // Test fcntl range locking
    use crate::syscall::fs::io::Flock;
    let mut fl1 = Flock {
        l_type: 1,   // F_WRLCK
        l_whence: 0, // SEEK_SET
        l_start: 10,
        l_len: 20,
        l_pid: 0,
    };
    // Acquire wrlock on range [10, 30) using fd1 (owner Flock because cmd is F_OFD_SETLK)
    let r_fc1 = crate::syscall::fs::sys_fcntl(fd1, 37, &mut fl1 as *mut Flock as u64); // F_OFD_SETLK
    assert_eq!(r_fc1, 0);

    // Try acquire wrlock on overlapping range [20, 40) using fd2 -> should fail with EAGAIN (-11)
    let mut fl2 = Flock {
        l_type: 1, // F_WRLCK
        l_whence: 0,
        l_start: 20,
        l_len: 20,
        l_pid: 0,
    };
    let r_fc2 = crate::syscall::fs::sys_fcntl(fd2, 37, &mut fl2 as *mut Flock as u64); // F_OFD_SETLK
    assert_eq!(r_fc2, -11); // -EAGAIN

    // Unlock on [10, 30)
    let mut fl_un = Flock {
        l_type: 2, // F_UNLCK
        l_whence: 0,
        l_start: 10,
        l_len: 20,
        l_pid: 0,
    };
    let r_fc_un = crate::syscall::fs::sys_fcntl(fd1, 37, &mut fl_un as *mut Flock as u64);
    assert_eq!(r_fc_un, 0);

    // Try acquire on fd2 again -> should succeed
    let r_fc3 = crate::syscall::fs::sys_fcntl(fd2, 37, &mut fl2 as *mut Flock as u64);
    assert_eq!(r_fc3, 0);

    // Close fds
    crate::process::fd::current_task_close_fd(fd1);
    crate::process::fd::current_task_close_fd(fd2);
    let _ = tmp_dir.unlink("lock_test.txt");

    kprintln!("[test] Phase 2 features verification test PASSED!");
}
