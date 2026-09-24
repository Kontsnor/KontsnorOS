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

#[path = "tests/mod.rs"]
pub mod tests;

/// Trait implemented by test functions to enable declarative naming and execution.
pub trait Testable {
    fn run(&self);
    fn name(&self) -> &'static str;
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        self();
    }
    fn name(&self) -> &'static str {
        core::any::type_name::<T>()
    }
}

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

/// Custom test runner executing a slice of test cases with RDTSC execution timing.
pub fn test_runner(tests: &[&dyn Testable]) {
    kprintln!("Running {} tests", tests.len());
    let tsc_khz = 2_000_000u64; // Nominally 2.0 GHz for timing calculations

    for test in tests {
        let name = test.name();
        let short_name = name.rsplit("::").next().unwrap_or(name);

        let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        test.run();
        let end_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let elapsed_cycles = end_tsc.saturating_sub(start_tsc);
        let elapsed_ms_x100 = (elapsed_cycles * 100) / (tsc_khz * 1000);
        let whole_ms = elapsed_ms_x100 / 100;
        let frac_ms = elapsed_ms_x100 % 100;

        kprintln!("[ok] {} ({}.{:02} ms)", short_name, whole_ms, frac_ms);
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

    // pick_next should retrieve them in priority order: High (100), Normal (101), Low (102)
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_high));
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_normal));
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_low));
    assert_eq!(sched.pick_next(), None);

    sched.remove_mock_task(pid_high);
    sched.remove_mock_task(pid_normal);
    sched.remove_mock_task(pid_low);
}

#[test_case]
fn test_orphan_reparenting() {
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

    // Read magic value from child's mapped physical address
    let phys_offset = crate::memory::r#virtual::phys_mem_offset();
    let child_ptr = (phys_child + phys_offset) as *const u64;
    let read_val = unsafe { child_ptr.read_volatile() };
    assert_eq!(read_val, 0xDEADBEEF12345678);

    // Clean up
    crate::syscall::memory::sys_munmap(addr1 as u64, 4096);
    crate::process::fd::current_task_close_fd(fd);
    let _ = crate::memory::r#virtual::free_user_page_table(child_cr3);
    let _ = disk_dir.unlink("shared_test.txt");
}

#[test_case]
fn test_page_cache_isolation() {
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
}

#[test_case]
fn test_dirty_page_flush() {
    // 1. Create and open file on ext via VFS directly
    let disk_dir = crate::fs::vfs::lookup("/disk").expect("Failed to lookup /disk");
    let _ = disk_dir.unlink("flush_test.txt");
    let inode = disk_dir
        .create("flush_test.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create flush_test.txt");

    // 2. Write 4096 bytes directly using InodeOps::write to populate/extend it
    let data = [0u8; 4096];
    let written = inode
        .write(0, &data)
        .expect("Failed to write to flush_test.txt");
    assert_eq!(written, 4096);

    // Allocate file descriptor manually
    let fd = crate::process::fd::current_task_alloc_fd(inode.clone())
        .expect("Failed to allocate file descriptor");

    // Sync the initial zero-fill to disk so disk backing has zeros before dirty mmap write
    crate::syscall::fs::sys_fsync(fd);

    // Map MAP_SHARED
    let addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x01, fd, 0);
    assert!(addr > 0);

    // Write magic value to the shared mapping
    let ptr = addr as *mut u64;
    unsafe {
        ptr.write_volatile(0x8877665544332211);
    }

    // Verify that the disk still has 0 before fsync (since it's only in page cache / memory)
    let mut read_buf = [0u8; 8];
    let res = inode.read_direct(0, &mut read_buf);
    assert!(res.is_ok());
    let val_before = u64::from_ne_bytes(read_buf);
    assert_eq!(val_before, 0);

    // Call fsync to commit changes
    let fsync_res = crate::syscall::fs::sys_fsync(fd);
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
}

#[test_case]
fn test_auxiliary_vectors() {
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
}

#[test_case]
fn test_userspace_wrfsbase() {
    let test_val = 0x0000_1234_5678_9ABCu64;

    // Save current FS_BASE
    let orig_fs = x86_64::registers::model_specific::FsBase::read().as_u64();

    // Write new FS_BASE using wrfsbase instruction
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
    unsafe {
        core::arch::asm!(
            "wrfsbase {}",
            in(reg) orig_fs,
        );
    }
}

#[test_case]
fn test_eventfd() {
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
}

#[test_case]
fn test_timerfd() {
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
}

#[test_case]
fn test_signalfd() {
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
}

#[test_case]
fn test_pseudo_filesystems() {
    // Test sysfs online CPUs
    let online_inode = crate::fs::vfs::lookup("/sys/devices/system/cpu/online")
        .expect("Failed to lookup /sys/devices/system/cpu/online");
    let mut buf = [0u8; 128];
    let n = online_inode
        .read(0, &mut buf)
        .expect("Failed to read CPU online file");
    let online_str = core::str::from_utf8(&buf[..n]).expect("Invalid UTF-8");
    assert!(!online_str.is_empty());

    // Test loopback MAC address
    let lo_inode = crate::fs::vfs::lookup("/sys/class/net/lo/address")
        .expect("Failed to lookup /sys/class/net/lo/address");
    let n_lo = lo_inode
        .read(0, &mut buf)
        .expect("Failed to read lo address");
    let lo_str = core::str::from_utf8(&buf[..n_lo]).expect("Invalid UTF-8");
    assert_eq!(lo_str, "00:00:00:00:00:00\n");

    // Test eth0 MAC address
    let eth0_inode = crate::fs::vfs::lookup("/sys/class/net/eth0/address")
        .expect("Failed to lookup /sys/class/net/eth0/address");
    let n_eth0 = eth0_inode
        .read(0, &mut buf)
        .expect("Failed to read eth0 address");
    let eth0_str = core::str::from_utf8(&buf[..n_eth0]).expect("Invalid UTF-8");
    assert!(!eth0_str.is_empty());
    assert!(eth0_str.ends_with('\n'));

    // Test cgroupfs controllers
    let controllers_inode = crate::fs::vfs::lookup("/sys/fs/cgroup/cgroup.controllers")
        .expect("Failed to lookup /sys/fs/cgroup/cgroup.controllers");
    let n_ctrl = controllers_inode
        .read(0, &mut buf)
        .expect("Failed to read cgroup.controllers");
    let ctrl_str = core::str::from_utf8(&buf[..n_ctrl]).expect("Invalid UTF-8");
    assert_eq!(ctrl_str, "cpu memory io pids\n");

    // Test cgroupfs procs
    let procs_inode = crate::fs::vfs::lookup("/sys/fs/cgroup/cgroup.procs")
        .expect("Failed to lookup /sys/fs/cgroup/cgroup.procs");
    let n_procs = procs_inode
        .read(0, &mut buf)
        .expect("Failed to read cgroup.procs");
    let procs_str = core::str::from_utf8(&buf[..n_procs]).expect("Invalid UTF-8");
    assert!(!procs_str.is_empty());

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
}

#[test_case]
fn test_ext4_extent_mapping() {
    let device = crate::drivers::ramdisk::create_ext2_ramdisk();
    let fs = crate::fs::ext::ExtFileSystem::mount(device.clone()).expect("Failed to mount ext");

    let inode = fs.get_ext_inode(12).expect("Failed to get ext inode");

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

    for i in 0..15 {
        i_block[i] = u32::from_le_bytes([
            bytes[i * 4],
            bytes[i * 4 + 1],
            bytes[i * 4 + 2],
            bytes[i * 4 + 3],
        ]);
    }

    assert_eq!(inode.resolve_extent_block(&i_block, 10).unwrap(), 1000);
    assert_eq!(inode.resolve_extent_block(&i_block, 15).unwrap(), 1005);
    assert_eq!(inode.resolve_extent_block(&i_block, 19).unwrap(), 1009);
    assert_eq!(inode.resolve_extent_block(&i_block, 20).unwrap(), 0);
    assert_eq!(inode.resolve_extent_block(&i_block, 30).unwrap(), 5000);
    assert_eq!(inode.resolve_extent_block(&i_block, 32).unwrap(), 5002);
    assert_eq!(inode.resolve_extent_block(&i_block, 34).unwrap(), 5004);
    assert_eq!(inode.resolve_extent_block(&i_block, 35).unwrap(), 0);

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

    device
        .write_block(120, &child_bytes[0..512])
        .expect("Write block 120 failed");
    device
        .write_block(121, &child_bytes[512..1024])
        .expect("Write block 121 failed");

    let resolved = inode.resolve_extent_block(&i_block_idx, 2).unwrap();
    assert_eq!(resolved, 9002);
}

#[test_case]
fn test_devfs_special_nodes() {
    let dev_null = crate::fs::vfs::lookup("/dev/null").expect("/dev/null missing");
    let inode_null = dev_null.inode();
    assert_eq!(inode_null.file_type, crate::fs::inode::FileType::CharDevice);
    assert_eq!(inode_null.rdev, (1 << 8) | 3);
    assert_eq!(inode_null.permissions.mode, 0o666);

    let mut buf = [0xAAu8; 16];
    let read_null = dev_null.read(0, &mut buf).expect("read /dev/null failed");
    assert_eq!(read_null, 0);

    let write_null = dev_null
        .write(0, b"test_data")
        .expect("write /dev/null failed");
    assert_eq!(write_null, 9);

    let poll_null = dev_null.poll(crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT);
    assert_eq!(
        poll_null,
        crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT
    );

    let dev_zero = crate::fs::vfs::lookup("/dev/zero").expect("/dev/zero missing");
    let inode_zero = dev_zero.inode();
    assert_eq!(inode_zero.file_type, crate::fs::inode::FileType::CharDevice);
    assert_eq!(inode_zero.rdev, (1 << 8) | 5);
    assert_eq!(inode_zero.permissions.mode, 0o666);

    let read_zero = dev_zero.read(0, &mut buf).expect("read /dev/zero failed");
    assert_eq!(read_zero, 16);
    assert_eq!(buf, [0u8; 16]);

    let write_zero = dev_zero
        .write(0, b"test_data")
        .expect("write /dev/zero failed");
    assert_eq!(write_zero, 9);

    let poll_zero = dev_zero.poll(crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT);
    assert_eq!(
        poll_zero,
        crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT
    );

    let dev_full = crate::fs::vfs::lookup("/dev/full").expect("/dev/full missing");
    let inode_full = dev_full.inode();
    assert_eq!(inode_full.file_type, crate::fs::inode::FileType::CharDevice);
    assert_eq!(inode_full.rdev, (1 << 8) | 7);
    assert_eq!(inode_full.permissions.mode, 0o666);

    let read_full = dev_full.read(0, &mut buf).expect("read /dev/full failed");
    assert_eq!(read_full, 16);
    assert_eq!(buf, [0u8; 16]);

    let write_full_res = dev_full.write(0, b"test_data");
    assert_eq!(write_full_res, Err(-28));

    let poll_full = dev_full.poll(crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT);
    assert_eq!(
        poll_full,
        crate::fs::inode::POLLIN | crate::fs::inode::POLLOUT
    );

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
}

#[test_case]
fn test_lseek_espipe_on_pipe() {
    let mut pipefds = [0i32; 2];
    let res = crate::syscall::fs::sys_pipe(pipefds.as_mut_ptr());
    assert_eq!(res, 0);

    let read_fd = pipefds[0];
    let write_fd = pipefds[1];

    let lseek_read_res = crate::syscall::fs::sys_lseek(read_fd, 0, 0);
    assert_eq!(lseek_read_res, crate::syscall::Errno::ESPIPE as i64);

    let lseek_write_res = crate::syscall::fs::sys_lseek(write_fd, 0, 0);
    assert_eq!(lseek_write_res, crate::syscall::Errno::ESPIPE as i64);

    crate::syscall::fs::sys_close(read_fd);
    crate::syscall::fs::sys_close(write_fd);
}
