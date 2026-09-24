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

//! Filesystem and VFS unit & regression tests.

use crate::kprintln;

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

#[test_case]
fn test_vfs_lookup_dcache_benchmark() {
    let tmp_dir = crate::fs::vfs::lookup("/tmp").expect("Failed to lookup /tmp");
    let test_dir = tmp_dir
        .mkdir("bench_dir")
        .expect("Failed to create /tmp/bench_dir");

    let test_file = test_dir
        .create("file.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create /tmp/bench_dir/file.txt");
    let _ = test_file.write(0, b"data");

    // Populate dcache on first lookup
    let _ = crate::fs::vfs::lookup("/tmp/bench_dir/file.txt").expect("Lookup failed");

    // Measure cached path lookups
    let iterations = 1_000;
    let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    for _ in 0..iterations {
        let node = crate::fs::vfs::lookup("/tmp/bench_dir/file.txt");
        core::hint::black_box(node);
    }
    let end_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let elapsed_tsc = end_tsc - start_tsc;
    let cycles_per_lookup = elapsed_tsc / iterations;

    crate::kprintln!(
        "[bench] VFS cached path lookup: {} total cycles for {} iterations (avg {} cycles/lookup)",
        elapsed_tsc,
        iterations,
        cycles_per_lookup
    );

    let _ = test_dir.unlink("file.txt");
    let _ = tmp_dir.rmdir("bench_dir");
}

#[test_case]
fn test_ext_readdir_streaming_benchmark() {
    kprintln!("[test] Starting Ext4 zero-allocation streaming readdir benchmark test...");

    let dir_path = b"/disk/stream_bench_dir\0";
    let dir_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    assert!(dir_addr > 0);
    unsafe {
        core::ptr::copy_nonoverlapping(dir_path.as_ptr(), dir_addr as *mut u8, dir_path.len());
    }

    let mkdir_res = crate::syscall::fs::sys_mkdir(dir_addr as *const u8, 0o755);
    assert_eq!(mkdir_res, 0, "mkdir /disk/stream_bench_dir failed");

    let path_buf_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    let entry_count = 100;

    // 1. Create 100 files in the directory
    for i in 0..entry_count {
        let filename = alloc::format!("/disk/stream_bench_dir/entry_{:03}.txt\0", i);
        let name_bytes = filename.as_bytes();
        unsafe {
            core::ptr::copy_nonoverlapping(
                name_bytes.as_ptr(),
                path_buf_addr as *mut u8,
                name_bytes.len(),
            );
        }

        let fd = crate::syscall::fs::sys_open(path_buf_addr as *const u8, 0o102, 0o644); // O_CREAT | O_RDWR
        assert!(fd >= 0, "create file failed");
        let _ = crate::syscall::fs::sys_close(fd as i32);
    }

    let dir_inode = crate::fs::vfs::lookup("/disk/stream_bench_dir")
        .expect("Failed to lookup stream_bench_dir");

    // 2. Measure iterate_dir_entries performance
    let start_ticks_stream = crate::arch::x86_64::interrupts::timer_ticks();
    let mut stream_count = 0;

    for _ in 0..100 {
        stream_count = 0;
        let _ = dir_inode.iterate_dir_entries(0, &mut |_next_off, _ino, _type, name| {
            if name != "." && name != ".." {
                stream_count += 1;
            }
            true
        });
    }

    let end_ticks_stream = crate::arch::x86_64::interrupts::timer_ticks();
    assert_eq!(stream_count, entry_count);

    // 3. Test sys_getdents64 integration
    let dir_fd = crate::syscall::fs::sys_open(dir_addr as *const u8, 0o20000, 0); // O_DIRECTORY
    assert!(dir_fd >= 0, "Failed to open directory for getdents64");

    let dents_buf_addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0) as u64;
    let mut total_dents_read = 0;

    loop {
        let nread =
            crate::syscall::fs::sys_getdents64(dir_fd as i32, dents_buf_addr as *mut u8, 4096);
        if nread <= 0 {
            break;
        }

        let mut offset = 0;
        while offset < nread as usize {
            let reclen = unsafe {
                let ptr = (dents_buf_addr as *const u8).add(offset + 16) as *const u16;
                ptr.read_unaligned() as usize
            };
            if reclen == 0 {
                break;
            }
            total_dents_read += 1;
            offset += reclen;
        }
    }

    assert_eq!(total_dents_read, entry_count + 2); // Includes . and ..
    let _ = crate::syscall::fs::sys_close(dir_fd as i32);

    let stream_ms = (end_ticks_stream.saturating_sub(start_ticks_stream)) * 10;
    kprintln!(
        "[test] Ext4 zero-allocation streaming readdir benchmark (100 iterations on {} entries): {} ms",
        entry_count,
        stream_ms
    );

    // Clean up created files and directory
    for i in 0..entry_count {
        let filename = alloc::format!("/disk/stream_bench_dir/entry_{:03}.txt\0", i);
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
    crate::syscall::memory::sys_munmap(dents_buf_addr, 4096);

    kprintln!("[test] Ext4 zero-allocation streaming readdir benchmark test PASSED!");
}
