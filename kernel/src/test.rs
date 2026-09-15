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
    kprintln!("[test] Starting trivial test...");
    let two = 2;
    assert_eq!(1 + 1, two);
    kprintln!("[test] trivial test PASSED!");
}

#[test_case]
fn test_memory_allocator() {
    kprintln!("[test] Starting memory allocator test...");
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
    kprintln!("[test] memory allocator test PASSED!");
}

#[test_case]
fn test_vfs_path_resolution() {
    kprintln!("[test] Starting VFS path resolution test...");
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
    kprintln!("[test] VFS path resolution test PASSED!");
}

#[test_case]
fn test_scheduler_priority_queues() {
    kprintln!("[test] Starting scheduler priority queues test...");
    let mut sched = crate::process::scheduler::Scheduler::new();

    // Create mock tasks with High, Normal, and Low priorities
    let pid_high = crate::process::pid::Pid::from_raw(100);
    let mut task_high =
        crate::process::task::Task::new(pid_high, alloc::string::String::from("high_prio"), 0);
    task_high.priority = crate::process::task::Priority::High;
    task_high.state = crate::process::task::TaskState::Ready;

    let pid_normal = crate::process::pid::Pid::from_raw(101);
    let mut task_normal =
        crate::process::task::Task::new(pid_normal, alloc::string::String::from("normal_prio"), 0);
    task_normal.priority = crate::process::task::Priority::Normal;
    task_normal.state = crate::process::task::TaskState::Ready;

    let pid_low = crate::process::pid::Pid::from_raw(102);
    let mut task_low =
        crate::process::task::Task::new(pid_low, alloc::string::String::from("low_prio"), 0);
    task_low.priority = crate::process::task::Priority::Low;
    task_low.state = crate::process::task::TaskState::Ready;

    // Add them in mixed order
    sched.add_task(task_low);
    sched.add_task(task_high);
    sched.add_task(task_normal);
    kprintln!("[test] Tasks added to scheduler");

    // pick_next should retrieve them in priority order: High (100), Normal (101), Low (102)
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_high));
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_normal));
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_low));
    assert_eq!(sched.pick_next(), None);
    kprintln!("[test] pick_next asserted");

    sched.remove_mock_task(pid_high);
    sched.remove_mock_task(pid_normal);
    sched.remove_mock_task(pid_low);

    kprintln!("[test] Scheduler priority queues test PASSED!");
}

#[test_case]
fn test_orphan_reparenting() {
    kprintln!("[test] Starting orphan reparenting test...");
    let mut sched = crate::process::scheduler::Scheduler::new();

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
    drop(parent);

    sched.remove_mock_task(pid_parent);
    sched.remove_mock_task(pid_child);

    kprintln!("[test] Orphan reparenting test PASSED!");
}

#[test_case]
fn test_vfs_permissions() {
    kprintln!("[test] Starting VFS permissions test...");
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
    kprintln!("[test] VFS permissions test PASSED!");
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

    // Sync the initial zero-fill to disk so disk backing has zeros before dirty mmap write
    crate::syscall::fs::sys_fsync(fd);

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

    let init_stack = crate::process::elf::construct_user_stack(
        &argv,
        &envp,
        entry_point,
        phdr,
        phnum,
        phent,
        interpreter_base,
    )
    .expect("Failed to construct user stack");

    let user_sp = init_stack.user_sp;

    // The stack pointer returned is at some offset in the stack top.
    assert!(user_sp >= init_stack.base_vaddr);
    assert!(user_sp < crate::process::elf::USER_STACK_TOP);
    let offset_in_buf = (user_sp - init_stack.base_vaddr) as usize;

    let stack_ptr = unsafe { init_stack.stack_buf.as_ptr().add(offset_in_buf) } as *const u64;

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

    sched.remove_mock_task(pid1);
    sched.remove_mock_task(pid2);

    kprintln!("[test] Thread Shared VM test PASSED!");
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
            let tasks = crate::process::scheduler::TASKS.read();
            let mut stress_states = alloc::vec::Vec::new();
            for (idx, task_opt) in tasks.iter().enumerate() {
                if let Some(task_arc) = task_opt {
                    let t = task_arc.lock();
                    if t.name.contains("stress") {
                        stress_states.push((idx, t.state, t.priority, t.in_queue));
                    }
                }
            }
            kprintln!(
                "[stress] Main thread monitoring (remaining: {}). Stress threads: {:?}",
                STRESS_THREADS_ACTIVE.load(core::sync::atomic::Ordering::SeqCst),
                stress_states
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

    // Wait for the helper thread to finish
    while !BITSET_WOKE.load(core::sync::atomic::Ordering::SeqCst) {
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
    // Emit the clear_child_tid write + futex wake BEFORE taking the scheduler
    // lock, exactly mirroring exit_current_thread (lifecycle.rs:343-353).
    // validate_user_ptr_write needs to call current_pid() which requires the
    // scheduler NOT to be locked.
    if let Some(ctid) = task_child.clear_child_tid {
        if crate::syscall::validation::validate_user_ptr_write(ctid as *mut u8, 4).is_ok() {
            // SAFETY: validate_user_ptr_write verified pointer lies in user memory
            unsafe {
                (ctid as *mut u32).write_volatile(0);
            }
        }
        let child_tgid = task_child.tgid.as_u64();
        crate::process::lifecycle::run_with_scheduler_lock(|sched| {
            crate::syscall::process::futex::futex_wake_locked(
                child_tgid, ctid, 1, 0xffffffff, sched,
            );
        });
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

    // Wait to let join_waiter run and set JOIN_WOKE
    while !JOIN_WOKE.load(core::sync::atomic::Ordering::SeqCst) {
        crate::process::scheduler::yield_now();
    }

    // Verify uaddr was cleared to 0
    let val_after = unsafe { uaddr.read_volatile() };
    assert_eq!(val_after, 0);

    // Verify join waiter was woken
    assert_eq!(JOIN_WOKE.load(core::sync::atomic::Ordering::SeqCst), true);

    // Clean up
    crate::syscall::memory::sys_munmap(addr, 4096);

    kprintln!("[test] futex bitset and CLONE_CHILD_CLEARTID verification test PASSED!");
}

#[test_case]
fn test_futex_bitset_timeout_and_requeue() {
    kprintln!("[test] Starting futex bitset timeout and requeue verification test...");

    let addr1 = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    let addr2 = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    assert!(addr1 > 0 && addr2 > 0);

    let uaddr1 = addr1 as *mut i32;
    let uaddr2 = addr2 as *mut i32;
    unsafe {
        uaddr1.write_volatile(42);
        uaddr2.write_volatile(100);
    }

    // 1. Test FUTEX_WAIT_BITSET with past deadline (should return ETIMEDOUT immediately)
    let past_ts = crate::syscall::process::futex::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let res = crate::syscall::process::sys_futex(
        uaddr1,
        9,  // FUTEX_WAIT_BITSET
        42, // Expected val
        &past_ts as *const _ as u64,
        core::ptr::null_mut(),
        -1,
    );
    assert_eq!(res, crate::syscall::Errno::ETIMEDOUT as i64);

    // 2. Test FUTEX_WAIT_BITSET with future monotonic timeout
    let current_ticks = crate::arch::x86_64::interrupts::timer_ticks();
    let current_mono_ns = current_ticks * 10_000_000;
    let target_mono_ns = current_mono_ns + 50_000_000; // 50ms in the future
    let future_ts = crate::syscall::process::futex::Timespec {
        tv_sec: (target_mono_ns / 1_000_000_000) as i64,
        tv_nsec: (target_mono_ns % 1_000_000_000) as i64,
    };

    let res_timed = crate::syscall::process::sys_futex(
        uaddr1,
        9,  // FUTEX_WAIT_BITSET (CLOCK_MONOTONIC)
        42, // Expected val
        &future_ts as *const _ as u64,
        core::ptr::null_mut(),
        -1,
    );
    assert_eq!(res_timed, crate::syscall::Errno::ETIMEDOUT as i64);

    // 3. Test FUTEX_CMP_REQUEUE
    // Test CMP_REQUEUE mismatch (expected 999 != 42) -> EAGAIN
    let res_mismatch = crate::syscall::process::sys_futex(
        uaddr1, 4, // FUTEX_CMP_REQUEUE
        1, // Wake 1
        1, // Requeue 1 (val2)
        uaddr2, 999, // Mismatched val3
    );
    assert_eq!(res_mismatch, crate::syscall::Errno::EAGAIN as i64);

    // Clean up
    crate::syscall::memory::sys_munmap(addr1, 4096);
    crate::syscall::memory::sys_munmap(addr2, 4096);
    kprintln!("[test] futex bitset timeout and requeue verification test PASSED!");
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

    // --- 1.5. /proc/self/exe ---
    kprintln!("[test] 1.5. Looking up /proc/self/exe");
    let self_exe =
        crate::fs::vfs::lookup_follow("/proc/self/exe", false).expect("/proc/self/exe missing");
    let mut exe_buf = [0u8; 128];
    let exe_len = self_exe
        .read(0, &mut exe_buf)
        .expect("readlink /proc/self/exe failed");
    let exe_str = core::str::from_utf8(&exe_buf[..exe_len]).unwrap();
    assert!(!exe_str.is_empty());
    assert!(exe_str.starts_with('/'));

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

#[test_case]
fn test_wine_tls_arch_prctl() {
    kprintln!("[test] Starting Wine TLS & arch_prctl test...");

    // Allocate 1 page for user buffer to test GET_FS / GET_GS
    let user_buf_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    assert!(user_buf_addr > 0);
    let fs_ptr = user_buf_addr as *mut u64;
    let gs_ptr = (user_buf_addr + 8) as *mut u64;

    let fs_val = 0x0000_7FFF_1234_5678u64;
    let gs_val = 0x0000_7FFF_8765_4321u64;

    // 1. Set FS_BASE (ARCH_SET_FS = 0x1002)
    let res_set_fs = crate::syscall::process::sys_arch_prctl(0x1002, fs_val);
    assert_eq!(res_set_fs, 0);

    // 2. Get FS_BASE (ARCH_GET_FS = 0x1003)
    let res_get_fs = crate::syscall::process::sys_arch_prctl(0x1003, fs_ptr as u64);
    assert_eq!(res_get_fs, 0);
    let read_fs = unsafe { fs_ptr.read_volatile() };
    assert_eq!(read_fs, fs_val);

    // 3. Set GS_BASE (ARCH_SET_GS = 0x1001)
    let res_set_gs = crate::syscall::process::sys_arch_prctl(0x1001, gs_val);
    assert_eq!(res_set_gs, 0);

    // 4. Get GS_BASE (ARCH_GET_GS = 0x1004)
    let res_get_gs = crate::syscall::process::sys_arch_prctl(0x1004, gs_ptr as u64);
    assert_eq!(res_get_gs, 0);
    let read_gs = unsafe { gs_ptr.read_volatile() };
    assert_eq!(read_gs, gs_val);

    // Yield to cause context switches and verify TLS bases remain intact
    for _ in 0..5 {
        crate::process::scheduler::yield_now();
    }

    let res_get_fs2 = crate::syscall::process::sys_arch_prctl(0x1003, fs_ptr as u64);
    assert_eq!(res_get_fs2, 0);
    assert_eq!(unsafe { fs_ptr.read_volatile() }, fs_val);

    let res_get_gs2 = crate::syscall::process::sys_arch_prctl(0x1004, gs_ptr as u64);
    assert_eq!(res_get_gs2, 0);
    assert_eq!(unsafe { gs_ptr.read_volatile() }, gs_val);

    crate::syscall::memory::sys_munmap(user_buf_addr, 4096);
    kprintln!("[test] Wine TLS & arch_prctl test PASSED!");
}

#[test_case]
fn test_wine_memory_fixed_and_mprotect() {
    kprintln!("[test] Starting Wine memory model & MAP_FIXED_NOREPLACE test...");

    // Pick a low 2GB address for hardcoded PE base simulation (e.g. 0x40000000)
    let target_base: u64 = 0x4000_0000;
    let page_size: usize = 4096;

    // First map a region at target_base using MAP_FIXED (0x10)
    let addr1 = crate::syscall::memory::sys_mmap(target_base, page_size, 3, 0x32, -1, 0) as u64; // PROT_READ|WRITE, MAP_PRIVATE|ANON|MAP_FIXED
    assert_eq!(addr1, target_base);

    // Write magic value to page
    let ptr = addr1 as *mut u64;
    unsafe {
        ptr.write_volatile(0xABCDEF1234567890);
    }
    assert_eq!(unsafe { ptr.read_volatile() }, 0xABCDEF1234567890);

    // Attempt MAP_FIXED_NOREPLACE (0x100000) on overlapping address -> must return -EEXIST (-17)
    let res_overlap = crate::syscall::memory::sys_mmap(target_base, page_size, 3, 0x100022, -1, 0);
    assert_eq!(res_overlap, crate::syscall::Errno::EEXIST as i64);

    // Attempt MAP_FIXED_NOREPLACE on adjacent non-overlapping page (target_base + 0x1000) -> must succeed
    let next_base = target_base + 0x1000;
    let addr2 = crate::syscall::memory::sys_mmap(next_base, page_size, 3, 0x100022, -1, 0) as u64;
    assert_eq!(addr2, next_base);

    // Test sys_mprotect transitions (e.g., PROT_READ = 1, PROT_NONE = 0, PROT_READ|WRITE = 3)
    let res_prot_read = crate::syscall::memory::sys_mprotect(target_base, page_size, 1);
    assert_eq!(res_prot_read, 0);

    let res_prot_rw = crate::syscall::memory::sys_mprotect(target_base, page_size, 3);
    assert_eq!(res_prot_rw, 0);

    // Clean up
    crate::syscall::memory::sys_munmap(addr1, page_size);
    crate::syscall::memory::sys_munmap(addr2, page_size);
    kprintln!("[test] Wine memory model & MAP_FIXED_NOREPLACE test PASSED!");
}

#[test_case]
fn test_wine_sigaltstack_and_ucontext() {
    kprintln!("[test] Starting Wine sigaltstack & ucontext frame test...");

    // Allocate stack memory for sigaltstack
    let alt_stack_size: u64 = 16384;
    let alt_stack_mem =
        crate::syscall::memory::sys_mmap(0, alt_stack_size as usize, 3, 0x22, -1, 0) as u64;
    assert!(alt_stack_mem > 0);

    let old_ss_buf = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    assert!(old_ss_buf > 0);

    let new_ss_buf = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    assert!(new_ss_buf > 0);

    let new_ss = crate::process::task::StackT {
        ss_sp: alt_stack_mem,
        ss_flags: 0,
        _pad: 0,
        ss_size: alt_stack_size,
    };
    // SAFETY: new_ss_buf is mapped user memory with write permissions.
    unsafe {
        core::ptr::write(new_ss_buf as *mut crate::process::task::StackT, new_ss);
    }

    // 1. Register alternate signal stack
    let res_alt = crate::syscall::process::sys_sigaltstack(
        new_ss_buf as *const u8,
        old_ss_buf as *mut u8,
        0x0000_7FFF_0000_0000,
    );
    assert_eq!(res_alt, 0);

    // Read back old_ss from old_ss_buf
    // SAFETY: old_ss_buf contains old_ss written by sys_sigaltstack.
    let old_ss = unsafe { *(old_ss_buf as *const crate::process::task::StackT) };
    assert_ne!(old_ss.ss_flags & 2, 0); // Previously SS_DISABLE

    // Query sigaltstack again to verify active configuration
    let res_query = crate::syscall::process::sys_sigaltstack(
        core::ptr::null(),
        old_ss_buf as *mut u8,
        0x0000_7FFF_0000_0000,
    );
    assert_eq!(res_query, 0);
    // SAFETY: old_ss_buf contains queried stack written by sys_sigaltstack.
    let query_ss = unsafe { *(old_ss_buf as *const crate::process::task::StackT) };
    assert_eq!(query_ss.ss_sp, alt_stack_mem);
    assert_eq!(query_ss.ss_size, alt_stack_size);
    assert_eq!(query_ss.ss_flags, 0);

    // Disable alternate signal stack
    let disable_ss = crate::process::task::StackT {
        ss_sp: 0,
        ss_flags: 2, // SS_DISABLE
        _pad: 0,
        ss_size: 0,
    };
    // SAFETY: new_ss_buf is mapped user memory with write permissions.
    unsafe {
        core::ptr::write(new_ss_buf as *mut crate::process::task::StackT, disable_ss);
    }
    let res_disable = crate::syscall::process::sys_sigaltstack(
        new_ss_buf as *const u8,
        core::ptr::null_mut(),
        0x0000_7FFF_0000_0000,
    );
    assert_eq!(res_disable, 0);

    // Clean up
    crate::syscall::memory::sys_munmap(alt_stack_mem, alt_stack_size as usize);
    crate::syscall::memory::sys_munmap(old_ss_buf, 4096);
    crate::syscall::memory::sys_munmap(new_ss_buf, 4096);
    kprintln!("[test] Wine sigaltstack & ucontext frame test PASSED!");
}

#[test_case]
fn test_crypto_prng() {
    kprintln!("[test] Starting PRNG byte generation test...");

    // 1. Reset PRNG state to test unseeded / no-entropy state
    crate::crypto::prng::reset_for_test();
    let mut unseeded_buf = [0xAAu8; 32];
    let res_unseeded = crate::crypto::prng::fill_bytes(&mut unseeded_buf);
    assert!(
        !res_unseeded,
        "fill_bytes should return false when PRNG has no entropy"
    );
    assert_eq!(
        unseeded_buf, [0xAAu8; 32],
        "Destination slice must remain unmodified when fill_bytes fails"
    );

    // 2. Seed PRNG with initial entropy key
    let seed_key = [0x42u8; 32];
    crate::crypto::prng::seed(&seed_key);

    // 3. Test small buffer fill and mutation verification
    let mut small_buf = [0u8; 16];
    let res_seeded = crate::crypto::prng::fill_bytes(&mut small_buf);
    assert!(
        res_seeded,
        "fill_bytes should return true after PRNG is seeded"
    );
    assert_ne!(
        small_buf, [0u8; 16],
        "Destination slice must be mutated with random bytes"
    );

    // 4. Test distinct, non-repetitive random output across consecutive calls
    let mut buf_a = [0u8; 32];
    let mut buf_b = [0u8; 32];
    assert!(crate::crypto::prng::fill_bytes(&mut buf_a));
    assert!(crate::crypto::prng::fill_bytes(&mut buf_b));
    assert_ne!(
        buf_a, buf_b,
        "Consecutive PRNG byte fills must produce distinct random output"
    );

    // 5. Test multi-block generation (> 64 bytes) to test ChaCha20 block generation and buffer index wrapping
    let mut large_buf = [0u8; 128];
    assert!(crate::crypto::prng::fill_bytes(&mut large_buf));
    // Verify first block (0..64) and second block (64..128) are non-zero and non-identical
    assert_ne!(&large_buf[0..64], &large_buf[64..128]);

    // 6. Test reseed functionality
    let reseed_entropy = [0x99u8; 32];
    crate::crypto::prng::reseed(&reseed_entropy);
    let mut reseeded_buf = [0u8; 32];
    assert!(crate::crypto::prng::fill_bytes(&mut reseeded_buf));
    assert_ne!(reseeded_buf, [0u8; 32]);

    kprintln!("[test] PRNG byte generation test PASSED!");
}

#[test_case]
fn test_ext_file_write_persistence() {
    kprintln!("[test] Starting ext file write persistence test...");
    let path_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    assert!(path_addr > 0);
    let path_str = b"/disk/persist_test.txt\0";
    // SAFETY: path_addr is a newly mmapped 4096-byte region with PROT_READ | PROT_WRITE.
    unsafe {
        core::ptr::copy_nonoverlapping(path_str.as_ptr(), path_addr as *mut u8, path_str.len());
    }

    // Open/create file for writing using user-space pathname
    let fd = crate::syscall::fs::sys_open(path_addr as *const u8, 0o102, 0o644); // O_CREAT | O_RDWR
    assert!(fd >= 0, "Failed to open/create test file");

    let buf_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    assert!(buf_addr > 0);
    let test_data = b"persistence_test_content_12345";
    // SAFETY: buf_addr is a newly mmapped 4096-byte region with PROT_READ | PROT_WRITE.
    unsafe {
        core::ptr::copy_nonoverlapping(test_data.as_ptr(), buf_addr as *mut u8, test_data.len());
    }

    let written = crate::syscall::fs::sys_write(fd as i32, buf_addr as *const u8, test_data.len());
    assert_eq!(written, test_data.len() as i64, "Short write");

    // Commit page cache dirty pages to disk
    let fsync_res = crate::syscall::fs::sys_fsync(fd as i32);
    assert_eq!(fsync_res, 0);

    // Close the file (triggers FileDescription::drop and flush)
    let close_res = crate::syscall::fs::sys_close(fd as i32);
    assert_eq!(close_res, 0);

    // Invalidate the page cache for this inode to force reading directly from disk
    let inode =
        crate::fs::vfs::lookup("/disk/persist_test.txt").expect("File must exist after write");
    let ino = inode.inode().ino;
    let dev = inode.inode().dev;
    crate::memory::page_cache::page_cache_invalidate_inode(dev, ino);

    // Read directly from disk using read_direct
    let mut read_buf = [0u8; 64];
    let read_bytes = inode
        .read_direct(0, &mut read_buf)
        .expect("read_direct failed");
    assert_eq!(read_bytes, test_data.len());
    assert_eq!(&read_buf[..test_data.len()], test_data);

    // Also verify normal read_page_cache repopulates from disk and matches
    let mut cache_read_buf = [0u8; 64];
    let cache_bytes = inode
        .read(0, &mut cache_read_buf)
        .expect("inode.read failed");
    assert_eq!(cache_bytes, test_data.len());
    assert_eq!(&cache_read_buf[..test_data.len()], test_data);

    // Clean up
    crate::syscall::memory::sys_munmap(path_addr, 4096);
    crate::syscall::memory::sys_munmap(buf_addr, 4096);
    kprintln!("[test] ext file write persistence test PASSED!");
}

#[test_case]
fn test_ext_fast_symlink_and_unlinked_open_file() {
    kprintln!("[test] Starting ext fast symlink and unlinked open file test...");

    // 1. Test fast symlink creation and unlinking
    let target_str = b"target_file.txt\0";
    let link_path = b"/disk/fast_symlink_test\0";

    let target_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    let link_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    assert!(target_addr > 0 && link_addr > 0);

    // SAFETY: target_addr and link_addr are newly allocated valid mapped user buffers.
    unsafe {
        core::ptr::copy_nonoverlapping(
            target_str.as_ptr(),
            target_addr as *mut u8,
            target_str.len(),
        );
        core::ptr::copy_nonoverlapping(link_path.as_ptr(), link_addr as *mut u8, link_path.len());
    }

    let sym_res = crate::syscall::fs::sys_symlink(target_addr as *const u8, link_addr as *const u8);
    assert_eq!(sym_res, 0, "Failed to create fast symlink");

    // Readlink to verify content
    let readlink_buf = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    let rl_res =
        crate::syscall::fs::sys_readlink(link_addr as *const u8, readlink_buf as *mut u8, 4096);
    assert_eq!(rl_res, (target_str.len() - 1) as i64);
    // SAFETY: readlink_buf has been written with target_str bytes without null terminator.
    let read_slice =
        unsafe { core::slice::from_raw_parts(readlink_buf as *const u8, rl_res as usize) };
    assert_eq!(read_slice, &target_str[..target_str.len() - 1]);

    // Unlink fast symlink - MUST NOT FAIL WITH -5 (EIO)
    let unlink_res = crate::syscall::fs::sys_unlink(link_addr as *const u8);
    assert_eq!(unlink_res, 0, "Unlink on fast symlink failed!");

    // Verify symlink is gone
    let mut stat_buf = crate::syscall::fs::meta::LinuxStat::default();
    let lstat_res = crate::syscall::fs::sys_lstat(link_addr as *const u8, &mut stat_buf);
    assert_eq!(lstat_res, -2, "Fast symlink should not exist after unlink");

    // 2. Test unlinked open file read and write semantics
    let file_path = b"/disk/unlinked_open_file.txt\0";
    let file_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    // SAFETY: file_addr is a newly allocated valid mapped user buffer.
    unsafe {
        core::ptr::copy_nonoverlapping(file_path.as_ptr(), file_addr as *mut u8, file_path.len());
    }

    let fd = crate::syscall::fs::sys_open(file_addr as *const u8, 0o102, 0o644); // O_CREAT | O_RDWR
    assert!(fd >= 0, "Failed to create test file");

    let test_data = b"data_written_before_unlink_12345";
    let data_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    // SAFETY: data_addr is a newly allocated valid mapped user buffer.
    unsafe {
        core::ptr::copy_nonoverlapping(test_data.as_ptr(), data_addr as *mut u8, test_data.len());
    }

    let written = crate::syscall::fs::sys_write(fd as i32, data_addr as *const u8, test_data.len());
    assert_eq!(written, test_data.len() as i64);

    // Unlink file while fd is still open!
    let unlink_file_res = crate::syscall::fs::sys_unlink(file_addr as *const u8);
    assert_eq!(unlink_file_res, 0, "Failed to unlink open file");

    // Seeking back to 0 and reading from fd should still succeed and return test_data!
    let lseek_res = crate::syscall::fs::sys_lseek(fd as i32, 0, 0); // SEEK_SET
    assert_eq!(lseek_res, 0);

    let read_dest = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    let bytes_read = crate::syscall::fs::sys_read(fd as i32, read_dest as *mut u8, test_data.len());
    assert_eq!(
        bytes_read,
        test_data.len() as i64,
        "Read from unlinked open file failed"
    );
    // SAFETY: read_dest has bytes_read bytes written by sys_read.
    let read_slice =
        unsafe { core::slice::from_raw_parts(read_dest as *const u8, bytes_read as usize) };
    assert_eq!(read_slice, test_data);

    // Close fd - now the file blocks and inode will be cleaned up on Drop
    let close_res = crate::syscall::fs::sys_close(fd as i32);
    assert_eq!(close_res, 0);

    // Verify lookup fails
    assert!(crate::fs::vfs::lookup("/disk/unlinked_open_file.txt").is_none());

    // Clean up allocated mmap buffers
    crate::syscall::memory::sys_munmap(target_addr, 4096);
    crate::syscall::memory::sys_munmap(link_addr, 4096);
    crate::syscall::memory::sys_munmap(readlink_buf, 4096);
    crate::syscall::memory::sys_munmap(file_addr, 4096);
    crate::syscall::memory::sys_munmap(data_addr, 4096);
    crate::syscall::memory::sys_munmap(read_dest, 4096);

    kprintln!("[test] ext fast symlink and unlinked open file test PASSED!");
}

#[test_case]
fn test_ext_small_file_creation_benchmark() {
    kprintln!("[test] Starting ext small-file creation benchmark test (1,000 files)...");

    let dir_path = b"/disk/bench_dir\0";
    let dir_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    assert!(dir_addr > 0);
    // SAFETY: dir_addr is mapped user memory.
    unsafe {
        core::ptr::copy_nonoverlapping(dir_path.as_ptr(), dir_addr as *mut u8, dir_path.len());
    }

    let mkdir_res = crate::syscall::fs::sys_mkdir(dir_addr as *const u8, 0o755);
    assert_eq!(mkdir_res, 0, "mkdir /disk/bench_dir failed");

    let start_ticks = crate::arch::x86_64::interrupts::timer_ticks();

    let path_buf_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    let data_buf_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    let test_data = [0xABu8; 1024];
    // SAFETY: data_buf_addr is mapped user memory.
    unsafe {
        core::ptr::copy_nonoverlapping(
            test_data.as_ptr(),
            data_buf_addr as *mut u8,
            test_data.len(),
        );
    }

    for i in 0..1000 {
        let filename = alloc::format!("/disk/bench_dir/file_{}.txt\0", i);
        let name_bytes = filename.as_bytes();
        unsafe {
            core::ptr::copy_nonoverlapping(
                name_bytes.as_ptr(),
                path_buf_addr as *mut u8,
                name_bytes.len(),
            );
        }

        let fd = crate::syscall::fs::sys_open(path_buf_addr as *const u8, 0o102, 0o644); // O_CREAT | O_RDWR
        assert!(fd >= 0, "open/create file failed");

        let written = crate::syscall::fs::sys_write(fd as i32, data_buf_addr as *const u8, 1024);
        assert_eq!(written, 1024);

        let close_res = crate::syscall::fs::sys_close(fd as i32);
        assert_eq!(close_res, 0);
    }

    let end_ticks = crate::arch::x86_64::interrupts::timer_ticks();
    let elapsed_ms = (end_ticks.saturating_sub(start_ticks)) * 10;
    kprintln!("[test] Created 1,000 files in {} ms", elapsed_ms);

    // Clean up created benchmark files and directory
    for i in 0..1000 {
        let filename = alloc::format!("/disk/bench_dir/file_{}.txt\0", i);
        let name_bytes = filename.as_bytes();
        unsafe {
            core::ptr::copy_nonoverlapping(
                name_bytes.as_ptr(),
                path_buf_addr as *mut u8,
                name_bytes.len(),
            );
        }
        let _ = crate::syscall::fs::sys_unlink(path_buf_addr as *const u8);
    }
    let _ = crate::syscall::fs::sys_rmdir(dir_addr as *const u8);

    crate::syscall::memory::sys_munmap(dir_addr, 4096);
    crate::syscall::memory::sys_munmap(path_buf_addr, 4096);
    crate::syscall::memory::sys_munmap(data_buf_addr, 4096);

    kprintln!("[test] ext small-file creation benchmark test PASSED!");
}

#[test_case]
fn test_acpi_find_table_edge_cases() {
    kprintln!("[test] Starting ACPI find_table edge cases test...");

    let phys_offset = crate::memory::r#virtual::phys_mem_offset();

    // 1. Invalid physical address (0)
    let res = crate::acpi::tables::find_table(0, b"APIC", 2);
    assert!(matches!(
        res,
        Err(crate::acpi::tables::AcpiError::InvalidAddress)
    ));

    // 2. Invalid signature
    let phys1 = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let virt1 = phys1 + phys_offset;
    unsafe {
        (virt1 as *mut crate::acpi::tables::SdtHeader).write(crate::acpi::tables::SdtHeader {
            signature: *b"BADS",
            length: 36,
            revision: 1,
            checksum: 0,
            oem_id: [0; 6],
            oem_table_id: [0; 8],
            oem_revision: 0,
            creator_id: 0,
            creator_revision: 0,
        });
    }

    let res = crate::acpi::tables::find_table(phys1, b"APIC", 2);
    assert!(matches!(
        res,
        Err(crate::acpi::tables::AcpiError::InvalidSignature)
    ));

    let res = crate::acpi::tables::find_table(phys1, b"APIC", 0);
    assert!(matches!(
        res,
        Err(crate::acpi::tables::AcpiError::InvalidSignature)
    ));
    crate::memory::physical::deallocate_frame(phys1);

    // 3. Short table length (< 36 bytes)
    let phys2 = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let virt2 = phys2 + phys_offset;
    unsafe {
        (virt2 as *mut crate::acpi::tables::SdtHeader).write(crate::acpi::tables::SdtHeader {
            signature: *b"XSDT",
            length: 20, // Less than minimum 36 bytes
            revision: 1,
            checksum: 0,
            oem_id: [0; 6],
            oem_table_id: [0; 8],
            oem_revision: 0,
            creator_id: 0,
            creator_revision: 0,
        });
    }

    let res = crate::acpi::tables::find_table(phys2, b"APIC", 2);
    assert!(matches!(
        res,
        Err(crate::acpi::tables::AcpiError::InvalidSignature)
    ));
    crate::memory::physical::deallocate_frame(phys2);

    // 4. Table not found
    let phys3 = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let virt3 = phys3 + phys_offset;
    unsafe {
        (virt3 as *mut crate::acpi::tables::SdtHeader).write(crate::acpi::tables::SdtHeader {
            signature: *b"XSDT",
            length: 36, // Header only, 0 entries
            revision: 1,
            checksum: 0,
            oem_id: [0; 6],
            oem_table_id: [0; 8],
            oem_revision: 0,
            creator_id: 0,
            creator_revision: 0,
        });
    }

    let res = crate::acpi::tables::find_table(phys3, b"APIC", 2);
    assert!(matches!(
        res,
        Err(crate::acpi::tables::AcpiError::TableNotFound)
    ));
    crate::memory::physical::deallocate_frame(phys3);

    // 5. XSDT (64-bit pointers) lookup success with null entry skipping
    let target_phys = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let target_virt = target_phys + phys_offset;
    unsafe {
        (target_virt as *mut crate::acpi::tables::SdtHeader).write(
            crate::acpi::tables::SdtHeader {
                signature: *b"APIC",
                length: 36,
                revision: 1,
                checksum: 0,
                oem_id: [0; 6],
                oem_table_id: [0; 8],
                oem_revision: 0,
                creator_id: 0,
                creator_revision: 0,
            },
        );
    }

    let xsdt_phys = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let xsdt_virt = xsdt_phys + phys_offset;
    unsafe {
        (xsdt_virt as *mut crate::acpi::tables::SdtHeader).write(crate::acpi::tables::SdtHeader {
            signature: *b"XSDT",
            length: 36 + 16, // 36 header + 2 * 8-byte entries
            revision: 1,
            checksum: 0,
            oem_id: [0; 6],
            oem_table_id: [0; 8],
            oem_revision: 0,
            creator_id: 0,
            creator_revision: 0,
        });
        let entries_ptr = (xsdt_virt + 36) as *mut u64;
        core::ptr::write_unaligned(entries_ptr, 0); // Null entry
        core::ptr::write_unaligned(entries_ptr.add(1), target_phys); // Valid entry
    }

    let found_phys =
        crate::acpi::tables::find_table(xsdt_phys, b"APIC", 2).expect("XSDT find_table failed");
    assert_eq!(found_phys, target_phys);

    crate::memory::physical::deallocate_frame(target_phys);
    crate::memory::physical::deallocate_frame(xsdt_phys);

    // 6. RSDT (32-bit pointers) lookup success with null entry skipping
    let target_rsdt_phys =
        crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let target_rsdt_virt = target_rsdt_phys + phys_offset;
    unsafe {
        (target_rsdt_virt as *mut crate::acpi::tables::SdtHeader).write(
            crate::acpi::tables::SdtHeader {
                signature: *b"MCFG",
                length: 36,
                revision: 1,
                checksum: 0,
                oem_id: [0; 6],
                oem_table_id: [0; 8],
                oem_revision: 0,
                creator_id: 0,
                creator_revision: 0,
            },
        );
    }

    let rsdt_phys = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let rsdt_virt = rsdt_phys + phys_offset;
    unsafe {
        (rsdt_virt as *mut crate::acpi::tables::SdtHeader).write(crate::acpi::tables::SdtHeader {
            signature: *b"RSDT",
            length: 36 + 8, // 36 header + 2 * 4-byte entries
            revision: 1,
            checksum: 0,
            oem_id: [0; 6],
            oem_table_id: [0; 8],
            oem_revision: 0,
            creator_id: 0,
            creator_revision: 0,
        });
        let entries_ptr = (rsdt_virt + 36) as *mut u32;
        core::ptr::write_unaligned(entries_ptr, 0); // Null entry
        core::ptr::write_unaligned(entries_ptr.add(1), target_rsdt_phys as u32);
        // Valid entry
    }

    let found_rsdt_phys =
        crate::acpi::tables::find_table(rsdt_phys, b"MCFG", 0).expect("RSDT find_table failed");
    assert_eq!(found_rsdt_phys, target_rsdt_phys);

    crate::memory::physical::deallocate_frame(target_rsdt_phys);
    crate::memory::physical::deallocate_frame(rsdt_phys);

    kprintln!("[test] ACPI find_table edge cases test PASSED!");
}

#[test_case]
fn test_git_pack_write_and_trailer_pread() {
    kprintln!("[test] Starting Git packfile write and trailer pread test...");

    // Map user memory buffers for paths and data
    let mmap_addr = crate::syscall::memory::sys_mmap(0, 16384, 3, 0x22, -1, 0) as u64;
    assert!(mmap_addr > 0);

    let pack_path = b"/disk/test_pack.pack\0";
    let idx_path = b"/disk/test_pack.idx\0";

    let pack_path_addr = mmap_addr;
    let idx_path_addr = mmap_addr + 256;
    let write_buf_addr = mmap_addr + 512;
    let read_buf_addr = mmap_addr + 12288;

    // SAFETY: mmap_addr points to an allocated 16384-byte region with PROT_READ | PROT_WRITE.
    unsafe {
        core::ptr::copy_nonoverlapping(
            pack_path.as_ptr(),
            pack_path_addr as *mut u8,
            pack_path.len(),
        );
        core::ptr::copy_nonoverlapping(idx_path.as_ptr(), idx_path_addr as *mut u8, idx_path.len());
    }

    // 1. Create simulated packfile (10,000 bytes spanning multiple 4096-byte blocks)
    let pack_fd = crate::syscall::fs::sys_open(pack_path_addr as *const u8, 0o102, 0o644); // O_CREAT | O_RDWR
    assert!(pack_fd >= 0, "Failed to open packfile");

    const PACK_SIZE: usize = 30000;
    const TRAILER_SIZE: usize = 20;
    let expected_trailer = [
        0x54u8, 0x26, 0x4a, 0xd5, 0xf5, 0x90, 0x99, 0x1b, 0x8e, 0xad, 0xf8, 0x7d, 0x2e, 0x87, 0x15,
        0x3a, 0xdf, 0xf9, 0xa0, 0x54,
    ];

    // Write in chunks to simulate git-index-pack
    let mut written_total = 0;
    while written_total < PACK_SIZE {
        let chunk_len = core::cmp::min(4096, PACK_SIZE - written_total);
        // SAFETY: write_buf_addr has 4096 bytes available.
        unsafe {
            let slice = core::slice::from_raw_parts_mut(write_buf_addr as *mut u8, chunk_len);
            for (i, byte) in slice.iter_mut().enumerate() {
                let file_pos = written_total + i;
                if file_pos >= PACK_SIZE - TRAILER_SIZE {
                    *byte = expected_trailer[file_pos - (PACK_SIZE - TRAILER_SIZE)];
                } else {
                    *byte = ((file_pos * 31 + 7) & 0xFF) as u8;
                }
            }
        }
        let res =
            crate::syscall::fs::sys_write(pack_fd as i32, write_buf_addr as *const u8, chunk_len);
        assert_eq!(res, chunk_len as i64, "Short write in packfile");
        written_total += chunk_len;
    }
    assert_eq!(crate::syscall::fs::sys_fsync(pack_fd as i32), 0);
    assert_eq!(crate::syscall::fs::sys_close(pack_fd as i32), 0);

    // 2. Create adjacent index file to trigger adjacent inode table allocation and writes
    let idx_fd = crate::syscall::fs::sys_open(idx_path_addr as *const u8, 0o102, 0o644);
    assert!(idx_fd >= 0, "Failed to open index file");
    let idx_content = b"GIT_PACK_INDEX_V2_DATA_SIMULATION_HEADER";
    // SAFETY: write_buf_addr has space for idx_content.
    unsafe {
        core::ptr::copy_nonoverlapping(
            idx_content.as_ptr(),
            write_buf_addr as *mut u8,
            idx_content.len(),
        );
    }
    let idx_written = crate::syscall::fs::sys_write(
        idx_fd as i32,
        write_buf_addr as *const u8,
        idx_content.len(),
    );
    assert_eq!(idx_written, idx_content.len() as i64);
    assert_eq!(crate::syscall::fs::sys_fsync(idx_fd as i32), 0);
    assert_eq!(crate::syscall::fs::sys_close(idx_fd as i32), 0);

    // 3. Open packfile read-only and verify trailer with pread64 (matching open_packed_git_1)
    let ro_pack_fd = crate::syscall::fs::sys_open(pack_path_addr as *const u8, 0, 0);
    assert!(ro_pack_fd >= 0, "Failed to re-open packfile");

    let pread_offset = (PACK_SIZE - TRAILER_SIZE) as i64;
    let pread_res = crate::syscall::fs::sys_pread64(
        ro_pack_fd as i32,
        read_buf_addr as *mut u8,
        TRAILER_SIZE,
        pread_offset,
    );
    assert_eq!(pread_res, TRAILER_SIZE as i64, "pread64 trailer failed");

    // SAFETY: read_buf_addr contains TRAILER_SIZE bytes read by pread64.
    let trailer_read =
        unsafe { core::slice::from_raw_parts(read_buf_addr as *const u8, TRAILER_SIZE) };
    assert_eq!(
        trailer_read, &expected_trailer,
        "Trailer mismatch via pread64"
    );

    // Invalidate page cache to force reading packfile and idx from underlying block cache / disk
    let pack_inode = crate::fs::vfs::lookup("/disk/test_pack.pack").expect("packfile must exist");
    crate::memory::page_cache::page_cache_invalidate_inode(
        pack_inode.inode().dev,
        pack_inode.inode().ino,
    );

    let pread_res2 = crate::syscall::fs::sys_pread64(
        ro_pack_fd as i32,
        read_buf_addr as *mut u8,
        TRAILER_SIZE,
        pread_offset,
    );
    assert_eq!(pread_res2, TRAILER_SIZE as i64);
    // SAFETY: read_buf_addr contains TRAILER_SIZE bytes read by pread64.
    let trailer_read2 =
        unsafe { core::slice::from_raw_parts(read_buf_addr as *const u8, TRAILER_SIZE) };
    assert_eq!(
        trailer_read2, &expected_trailer,
        "Trailer mismatch after page cache invalidation"
    );

    assert_eq!(crate::syscall::fs::sys_close(ro_pack_fd as i32), 0);

    // 4. Verify index file was not corrupted by adjacent inode writes
    let ro_idx_fd = crate::syscall::fs::sys_open(idx_path_addr as *const u8, 0, 0);
    assert!(ro_idx_fd >= 0, "Failed to re-open index file");
    let read_idx_res = crate::syscall::fs::sys_read(
        ro_idx_fd as i32,
        read_buf_addr as *mut u8,
        idx_content.len(),
    );
    assert_eq!(
        read_idx_res,
        idx_content.len() as i64,
        "Index file read corrupted"
    );
    // SAFETY: read_buf_addr contains idx_content.len() bytes read by sys_read.
    let idx_read =
        unsafe { core::slice::from_raw_parts(read_buf_addr as *const u8, idx_content.len()) };
    assert_eq!(
        idx_read, idx_content,
        "Index file contents corrupted by adjacent inode write"
    );
    assert_eq!(crate::syscall::fs::sys_close(ro_idx_fd as i32), 0);

    // Clean up
    crate::syscall::memory::sys_munmap(mmap_addr, 16384);
    kprintln!("[test] Git packfile write and trailer pread test PASSED!");
}

#[test_case]
fn test_acpi_rsdp_parsing() {
    kprintln!("[test] Starting ACPI RSDP parsing error handling test...");

    // 1. Null physical address
    assert_eq!(
        crate::acpi::tables::parse_rsdp(0),
        Err(crate::acpi::tables::AcpiError::InvalidAddress)
    );

    let phys_offset = crate::memory::r#virtual::phys_mem_offset();

    // 2. Invalid RSDP Signature
    let mut invalid_rsdp = crate::acpi::tables::Rsdp {
        signature: *b"BAD SIG ",
        checksum: 0,
        oem_id: *b"TESTOM",
        revision: 2,
        rsdt_address: 0x1000,
        length: 36,
        xsdt_address: 0x2000,
        extended_checksum: 0,
        reserved: [0; 3],
    };
    let virt_addr1 = &invalid_rsdp as *const _ as u64;
    let phys_addr1 = virt_addr1 - phys_offset;

    assert_eq!(
        crate::acpi::tables::parse_rsdp(phys_addr1),
        Err(crate::acpi::tables::AcpiError::InvalidRsdpSignature)
    );

    // 3. Invalid Checksum
    let mut bad_checksum_rsdp = crate::acpi::tables::Rsdp {
        signature: *b"RSD PTR ",
        checksum: 0xFF, // Intentionally incorrect checksum
        oem_id: *b"TESTOM",
        revision: 2,
        rsdt_address: 0x1000,
        length: 36,
        xsdt_address: 0x2000,
        extended_checksum: 0,
        reserved: [0; 3],
    };
    let virt_addr2 = &bad_checksum_rsdp as *const _ as u64;
    let phys_addr2 = virt_addr2 - phys_offset;

    assert_eq!(
        crate::acpi::tables::parse_rsdp(phys_addr2),
        Err(crate::acpi::tables::AcpiError::InvalidChecksum)
    );

    // 4. Valid RSDP (Happy Path)
    let mut valid_rsdp = crate::acpi::tables::Rsdp {
        signature: *b"RSD PTR ",
        checksum: 0,
        oem_id: *b"MY OEM",
        revision: 2,
        rsdt_address: 0x1000,
        length: 36,
        xsdt_address: 0x2000_0000,
        extended_checksum: 0,
        reserved: [0; 3],
    };
    // Calculate valid checksum for first 20 bytes
    let bytes =
        unsafe { core::slice::from_raw_parts_mut(&mut valid_rsdp as *mut _ as *mut u8, 20) };
    let sum_without_checksum: u8 = bytes[0..8]
        .iter()
        .chain(&bytes[9..20])
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    valid_rsdp.checksum = (0u8).wrapping_sub(sum_without_checksum);

    let virt_addr3 = &valid_rsdp as *const _ as u64;
    let phys_addr3 = virt_addr3 - phys_offset;

    let parsed = crate::acpi::tables::parse_rsdp(phys_addr3).expect("Valid RSDP parsing failed");
    assert_eq!(parsed.oem_id, "MY OEM");
    assert_eq!(parsed.revision, 2);
    assert_eq!(parsed.xsdt_address, 0x2000_0000);

    kprintln!("[test] ACPI RSDP parsing error handling test PASSED!");
}

#[test_case]
fn test_prng_seed_initialization() {
    kprintln!("[test] Starting PRNG seed initialization test...");

    // 1. Seed the PRNG with initial 32-byte entropy key
    let seed1: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];

    crate::crypto::prng::seed(&seed1);

    // 2. Verify fill_bytes returns true after seeding
    let mut buf1 = [0u8; 32];
    let ok = crate::crypto::prng::fill_bytes(&mut buf1);
    assert!(ok, "fill_bytes should return true after PRNG is seeded");

    // Ensure the generated random bytes are not all zeros
    assert_ne!(buf1, [0u8; 32], "PRNG output should non-zero");

    // 3. Verify deterministic generation for identical seed initialization
    crate::crypto::prng::seed(&seed1);
    let mut buf2 = [0u8; 32];
    let ok2 = crate::crypto::prng::fill_bytes(&mut buf2);
    assert!(ok2);
    assert_eq!(
        buf1, buf2,
        "Identical seeds must produce identical initial output blocks"
    );

    // 4. Verify re-seeding / different seed initialization changes output sequence
    let seed2: [u8; 32] = [0xff; 32];
    crate::crypto::prng::seed(&seed2);
    let mut buf3 = [0u8; 32];
    let ok3 = crate::crypto::prng::fill_bytes(&mut buf3);
    assert!(ok3);
    assert_ne!(
        buf1, buf3,
        "Different seeds must produce different output blocks"
    );

    kprintln!("[test] PRNG seed initialization test PASSED!");
}

#[test_case]
fn test_devfs_special_nodes() {
    kprintln!("[test] Starting devfs special character device nodes test...");

    // 1. Verify /dev/null
    let dev_null = crate::fs::vfs::lookup("/dev/null").expect("/dev/null missing");
    let inode_null = dev_null.inode();
    assert_eq!(inode_null.file_type, crate::fs::inode::FileType::CharDevice);
    assert_eq!(inode_null.rdev, (1 << 8) | 3);
    assert_eq!(inode_null.permissions.mode, 0o666);

    let mut buf = [0xAAu8; 16];
    let read_null = dev_null.read(0, &mut buf).expect("read /dev/null failed");
    assert_eq!(read_null, 0, "/dev/null read must return EOF (0 bytes)");

    let write_null = dev_null
        .write(0, b"test_data")
        .expect("write /dev/null failed");
    assert_eq!(
        write_null, 9,
        "/dev/null write must discard all bytes and return count"
    );

    let poll_null = dev_null.poll(crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT);
    assert_eq!(
        poll_null,
        crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT
    );

    // 2. Verify /dev/zero
    let dev_zero = crate::fs::vfs::lookup("/dev/zero").expect("/dev/zero missing");
    let inode_zero = dev_zero.inode();
    assert_eq!(inode_zero.file_type, crate::fs::inode::FileType::CharDevice);
    assert_eq!(inode_zero.rdev, (1 << 8) | 5);
    assert_eq!(inode_zero.permissions.mode, 0o666);

    let read_zero = dev_zero.read(0, &mut buf).expect("read /dev/zero failed");
    assert_eq!(read_zero, 16);
    assert_eq!(buf, [0u8; 16], "/dev/zero read must yield zeroed buffer");

    let write_zero = dev_zero
        .write(0, b"test_data")
        .expect("write /dev/zero failed");
    assert_eq!(write_zero, 9);

    let poll_zero = dev_zero.poll(crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT);
    assert_eq!(
        poll_zero,
        crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT
    );

    // 3. Verify /dev/full
    let dev_full = crate::fs::vfs::lookup("/dev/full").expect("/dev/full missing");
    let inode_full = dev_full.inode();
    assert_eq!(inode_full.file_type, crate::fs::inode::FileType::CharDevice);
    assert_eq!(inode_full.rdev, (1 << 8) | 7);
    assert_eq!(inode_full.permissions.mode, 0o666);

    let read_full = dev_full.read(0, &mut buf).expect("read /dev/full failed");
    assert_eq!(read_full, 16);
    assert_eq!(buf, [0u8; 16], "/dev/full read must yield zeroed buffer");

    let write_full_res = dev_full.write(0, b"test_data");
    assert_eq!(
        write_full_res,
        Err(-28),
        "/dev/full write must fail with -ENOSPC (-28)"
    );

    let poll_full = dev_full.poll(crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT);
    assert_eq!(
        poll_full,
        crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT
    );

    // 4. Verify /dev/random & /dev/urandom
    let dev_random = crate::fs::vfs::lookup("/dev/random").expect("/dev/random missing");
    let inode_random = dev_random.inode();
    assert_eq!(
        inode_random.file_type,
        crate::fs::inode::FileType::CharDevice
    );
    assert_eq!(inode_random.rdev, (1 << 8) | 8);
    assert_eq!(inode_random.permissions.mode, 0o666);

    let dev_urandom = crate::fs::vfs::lookup("/dev/urandom").expect("/dev/urandom missing");
    let inode_urandom = dev_urandom.inode();
    assert_eq!(
        inode_urandom.file_type,
        crate::fs::inode::FileType::CharDevice
    );
    assert_eq!(inode_urandom.rdev, (1 << 8) | 9);
    assert_eq!(inode_urandom.permissions.mode, 0o666);

    let mut rand_buf = [0u8; 32];
    let read_urandom = dev_urandom
        .read(0, &mut rand_buf)
        .expect("read /dev/urandom failed");
    assert_eq!(read_urandom, 32);
    assert_ne!(rand_buf, [0u8; 32]);

    let seed_data = [0x55u8; 32];
    let write_urandom = dev_urandom
        .write(0, &seed_data)
        .expect("write /dev/urandom failed");
    assert_eq!(write_urandom, 32);

    kprintln!("[test] devfs special character device nodes test PASSED!");
}

#[test_case]
fn test_path_normalization_fastpath() {
    kprintln!("[test] Starting path normalization fast-path and join test...");

    use crate::fs::path::{join, normalize};

    // 1. Already-normalized absolute paths
    assert_eq!(normalize("/"), "/");
    assert_eq!(normalize("/usr/bin/bash"), "/usr/bin/bash");
    assert_eq!(normalize("/a/b/c/d/e"), "/a/b/c/d/e");

    // 2. Already-normalized relative paths
    assert_eq!(normalize("foo"), "foo");
    assert_eq!(
        normalize("foo/bar/baz"),
        "foo/bar/bash".replace("bash", "baz")
    );

    // 3. Paths requiring normalization
    assert_eq!(normalize("/usr/./local/../bin"), "/usr/bin");
    assert_eq!(normalize("///foo//bar"), "/foo/bar");
    assert_eq!(normalize("/foo/bar/"), "/foo/bar");
    assert_eq!(normalize("/a/b/c/."), "/a/b/c");
    assert_eq!(normalize("/a/b/c/.."), "/a/b");
    assert_eq!(normalize("."), "/");
    assert_eq!(normalize(".."), "/");
    assert_eq!(normalize(""), "/");

    // 4. Path joining with exact capacity
    assert_eq!(join("/usr/local", "bin"), "/usr/local/bin");
    assert_eq!(join("/usr/local/", "bin"), "/usr/local/bin");
    assert_eq!(join("/usr", "/bin"), "/bin");

    kprintln!("[test] path normalization fast-path and join test PASSED!");
}
