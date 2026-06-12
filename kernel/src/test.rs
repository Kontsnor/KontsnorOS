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
    assert_eq!(1 + 1, 2);
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
    let test_dir = tmp_dir.mkdir("test_dir").expect("Failed to create /tmp/test_dir");
    
    // Create a file under /tmp/test_dir
    let test_file = test_dir.create("test.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create /tmp/test_dir/test.txt");
    
    // Write data to the file
    let test_data = b"Hello, VFS!";
    let written = test_file.write(0, test_data).expect("Failed to write to test.txt");
    assert_eq!(written, test_data.len());

    // Invalidate the cache to ensure we test true lookup resolution
    crate::fs::vfs::invalidate_dentry("/tmp/test_dir/test.txt");

    // Read back data
    let looked_up = crate::fs::vfs::lookup("/tmp/test_dir/test.txt")
        .expect("Failed to lookup /tmp/test_dir/test.txt after cache invalidation");
    let mut read_buf = [0u8; 32];
    let read_len = looked_up.read(0, &mut read_buf).expect("Failed to read from test.txt");
    assert_eq!(read_len, test_data.len());
    assert_eq!(&read_buf[..read_len], test_data);
}

#[test_case]
fn test_scheduler_priority_queues() {
    let mut sched = crate::process::scheduler::Scheduler::new();

    // Create mock tasks with High, Normal, and Low priorities
    let pid_high = crate::process::pid::Pid::from_raw(10);
    let mut task_high = crate::process::task::Task::new(pid_high, alloc::string::String::from("high_prio"), 0);
    task_high.priority = crate::process::task::Priority::High;
    task_high.state = crate::process::task::TaskState::Ready;

    let pid_normal = crate::process::pid::Pid::from_raw(11);
    let mut task_normal = crate::process::task::Task::new(pid_normal, alloc::string::String::from("normal_prio"), 0);
    task_normal.priority = crate::process::task::Priority::Normal;
    task_normal.state = crate::process::task::TaskState::Ready;

    let pid_low = crate::process::pid::Pid::from_raw(12);
    let mut task_low = crate::process::task::Task::new(pid_low, alloc::string::String::from("low_prio"), 0);
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
