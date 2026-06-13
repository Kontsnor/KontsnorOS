//! # KontsnorOS Kernel
//!
//! A Unix-compatible hybrid kernel written entirely in Rust.
//!
//! ## Architecture
//!
//! KontsnorOS is a hybrid kernel that combines the performance of a monolithic
//! kernel with the modularity and safety of a microkernel. Core subsystems
//! (scheduler, VMM, IPC) run in ring 0, while drivers are loadable modules
//! with well-defined interfaces.
//!
//! ## License
//!
//! Licensed under either of:
//! - Apache License, Version 2.0 (LICENSE-APACHE or <http://www.apache.org/licenses/LICENSE-2.0>)
//! - MIT license (LICENSE-MIT or <http://opensource.org/licenses/MIT>)
//!
//! at your option.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![cfg_attr(feature = "test", feature(custom_test_frameworks))]
#![cfg_attr(feature = "test", reexport_test_harness_main = "test_main")]
#![cfg_attr(feature = "test", test_runner(crate::test::test_runner))]
#![deny(unsafe_op_in_unsafe_fn)]
#![allow(warnings)]

extern crate alloc;

// Declare arch first so that kprint!/kprintln! macros (defined with
// #[macro_export]) are available to all subsequent modules.
mod acpi;
#[macro_use]
mod arch;
mod crypto;
mod drivers;
mod fs;
mod ipc;
mod memory;
mod net;
mod panic;
mod process;
mod sync;
mod syscall;
#[cfg(feature = "test")]
pub mod test;
mod util;

use bootloader_api::config::Mapping;
use bootloader_api::{entry_point, BootInfo, BootloaderConfig};

/// Bootloader configuration — request kernel mapping at higher half.
pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::FixedAddress(0xffff_a000_0000_0000));
    config.kernel_stack_size = 256 * 1024; // 256 KiB kernel stack
    config
};
entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

const ENABLE_DEMO_THREADS: bool = false;
const ENABLE_NET_TESTS: bool = false;

/// Kernel entry point — called by the bootloader after basic setup.
///
/// At this point we have:
/// - A valid GDT with kernel code/data segments
/// - Identity-mapped + higher-half mapped kernel
/// - A kernel stack
/// - Boot information from the bootloader
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // ── Phase 1: Early initialization ──────────────────────────────────
    // Initialize serial output first so we can log everything else
    arch::x86_64::serial::init();

    kprintln!("=========================================");
    kprintln!("  KontsnorOS v{}", env!("CARGO_PKG_VERSION"));
    kprintln!("  A Unix-Compatible Hybrid Kernel");
    kprintln!("  Written in Rust — Safe, Fast, Modern");
    kprintln!("=========================================");
    kprintln!();

    // ── Phase 2: Architecture-specific initialization ──────────────────
    kprintln!("[boot] Initializing GDT...");
    arch::x86_64::gdt::init();
    kprintln!("[boot] GDT initialized.");

    kprintln!("[boot] Initializing IDT...");
    arch::x86_64::interrupts::init_idt();
    kprintln!("[boot] IDT initialized.");

    kprintln!("[boot] Initializing PIC...");
    arch::x86_64::interrupts::init_pics();
    kprintln!("[boot] PIC initialized.");

    kprintln!("[boot] Enabling SSE and FSGSBASE...");
    unsafe {
        arch::x86_64::boot::enable_sse();
        arch::x86_64::boot::enable_fsgsbase();
    }
    kprintln!("[boot] SSE and FSGSBASE enabled.");

    // ── Phase 3: Memory initialization ─────────────────────────────────
    kprintln!("[boot] Initializing memory subsystem...");
    let phys_mem_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("Physical memory offset not provided by bootloader");

    // Initialize the physical frame allocator
    memory::physical::init(&boot_info.memory_regions);
    kprintln!("[boot] Physical frame allocator initialized.");

    // Initialize the virtual memory manager
    memory::r#virtual::init(phys_mem_offset);
    kprintln!("[boot] Virtual memory manager initialized.");

    // Initialize the kernel heap
    memory::heap::init().expect("Kernel heap initialization failed");
    kprintln!("[boot] Kernel heap initialized.");

    // Initialize CSPRNG using boot-time entropy
    crypto::prng::init_entropy(boot_info);
    kprintln!("[boot] CSPRNG initialized.");

    // Run integration test for COW reference counting
    memory::physical::test_cow_refcounts();

    // Initialize dynamic per-core GDT/TSS configuration
    arch::x86_64::gdt::init_heap();
    kprintln!("[boot] Dynamic GDT/TSS initialized.");

    // ── Phase 4: Hardware discovery ────────────────────────────────────
    kprintln!("[boot] Initializing ACPI...");
    let rsdp_addr = boot_info.rsdp_addr.into_option();
    acpi::init(rsdp_addr);
    kprintln!("[boot] ACPI initialized.");

    kprintln!("[boot] Initializing APIC...");
    arch::x86_64::apic::init();
    kprintln!("[boot] APIC initialized.");

    kprintln!("[boot] Initializing SMP CPU Manager...");
    arch::x86_64::smp::init();
    kprintln!("[boot] SMP CPU Manager initialized.");

    // ── Phase 5: Subsystem initialization ──────────────────────────────
    kprintln!("[boot] Initializing VFS...");
    fs::init();
    kprintln!("[boot] VFS initialized.");

    // Verify ext RAM disk file retrieval
    kprintln!("[boot] Testing ext RAM disk file retrieval...");
    if let Some(inode) = fs::vfs::lookup("/disk/hello.txt") {
        let size = inode.inode().size as usize;
        let mut buf = alloc::vec![0u8; size];
        match inode.read(0, &mut buf) {
            Ok(bytes_read) => {
                if let Ok(content_str) = core::str::from_utf8(&buf[0..bytes_read]) {
                    kprintln!(
                        "[ext] Successfully read /disk/hello.txt ({} bytes): \"{}\"",
                        bytes_read,
                        content_str
                    );
                } else {
                    kprintln!("[ext] Read file but content is not valid UTF-8.");
                }
            }
            Err(e) => {
                kprintln!("[ext] Failed to read /disk/hello.txt: error {}", e);
            }
        }
    } else {
        kprintln!("[ext] File /disk/hello.txt not found on mounted ext disk!");
    }

    kprintln!("[boot] Initializing process subsystem...");
    process::init();
    kprintln!("[boot] Process subsystem initialized.");

    // Spawn demo multitasking threads
    if ENABLE_DEMO_THREADS {
        kprintln!("[boot] Spawning kernel multitasking demo threads...");
        process::spawn_kernel_thread(alloc::string::String::from("demo_1"), demo_thread_1);
        process::spawn_kernel_thread(alloc::string::String::from("demo_2"), demo_thread_2);
    }

    kprintln!("[boot] Initializing network stack...");
    net::init();
    kprintln!("[boot] Network stack initialized.");

    kprintln!("[boot] Initializing driver framework...");
    drivers::init();
    kprintln!("[boot] Driver framework initialized.");

    kprintln!("[boot] Initializing IPC subsystem...");
    ipc::init();
    kprintln!("[boot] IPC subsystem initialized.");

    kprintln!("[boot] Initializing syscall interface...");
    syscall::init();
    kprintln!("[boot] Syscall interface initialized.");

    #[cfg(not(feature = "test"))]
    {
        // Clear the graphics console to enter terminal mode
        if let Some(ref mut console) = *crate::drivers::gpu::bochs::GRAPHICS_CONSOLE.lock() {
            console.clear(crate::drivers::gpu::framebuffer::Color::BLACK);
            console.gpu.blit();
        }

        // Disable standard text mode console log prints from mirroring to the graphics screen
        crate::drivers::gpu::bochs::DISABLE_CONSOLE_MIRROR
            .store(true, core::sync::atomic::Ordering::Relaxed);

        // Allocate a new PTY master/slave pair and set up active routing
        let master = crate::fs::pty::allocate_new_pty().expect("Failed to allocate PTY");
        *crate::fs::pty::ACTIVE_PTY_MASTER.lock() = Some(master.clone());
        crate::fs::pty::start_pty_io_loop();

        // Spawn Ring 3 user init from ext RAM disk as PID 1
        let init_path = "/sbin/init";
        kprintln!("[boot] Spawning Ring 3 → Ring 3 init: {}...", init_path);
        if let Some(inode) = fs::vfs::lookup(init_path) {
            let size = inode.inode().size as usize;
            let mut buf = alloc::vec![0u8; size];
            match inode.read(0, &mut buf) {
                Ok(bytes_read) => {
                    kprintln!(
                        "[boot] Loaded {} ({} bytes) from VFS, spawning...",
                        init_path,
                        bytes_read
                    );
                    let pid = process::spawn_user_process_with_pid(
                        alloc::string::String::from("init"),
                        &buf,
                        crate::process::pid::Pid::INIT,
                    );

                    // Connect spawned init's fd 0, 1, 2 to the PTY slave device (/dev/pts/0)
                    let slave =
                        crate::fs::vfs::lookup("/dev/pts/0").expect("Failed to lookup PTY slave");
                    if let Some(task_arc) = process::scheduler::get_task_arc(pid) {
                        let mut task = task_arc.lock();
                        let mut fd_table = task.fd_table.lock();
                        fd_table.entries.clear();
                        let slave_desc =
                            alloc::sync::Arc::new(crate::fs::file::FileDescription::new(
                                slave,
                                crate::fs::file::OpenFlags(crate::fs::file::OpenFlags::O_RDWR),
                            ));
                        fd_table.entries.push(Some(slave_desc.clone())); // fd 0
                        fd_table.entries.push(Some(slave_desc.clone())); // fd 1
                        fd_table.entries.push(Some(slave_desc.clone())); // fd 2
                    }
                }
                Err(e) => {
                    kprintln!("[boot] Failed to read {}: error {}", init_path, e);
                }
            }
        } else {
            kprintln!("[boot] {} not found on mounted ext disk!", init_path);
        }

        // Spawn freestanding network test binary
        if ENABLE_NET_TESTS {
            kprintln!("[boot] Spawning freestanding network test binary...");
            let net_test_elf = process::create_net_test_elf();
            process::spawn_user_process(alloc::string::String::from("net_test"), net_test_elf);
        }
    }

    // ── Boot complete ──────────────────────────────────────────────────
    kprintln!();
    kprintln!("=========================================");
    kprintln!("  KontsnorOS boot complete!");
    kprintln!("  All subsystems initialized.");
    kprintln!("=========================================");
    kprintln!();

    // Enable interrupts and enter the scheduler
    x86_64::instructions::interrupts::enable();
    kprintln!("[kernel] Interrupts enabled. Yielding to ready threads...");

    #[cfg(feature = "test")]
    {
        kprintln!("[boot] Running in test mode...");
        test_main();
    }

    // Yield control to let the spawned demo threads run
    #[cfg(not(feature = "test"))]
    process::scheduler::yield_now();

    kprintln!("[main] Returned to bootstrap thread. Halting.");
    idle_loop()
}

/// The kernel idle loop — halts the CPU until the next interrupt.
///
/// This is the main loop that runs when no tasks are scheduled.
/// The `hlt` instruction puts the CPU into a low-power state until
/// an interrupt fires.
fn idle_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

/// Demo thread 1: prints and cooperatively yields control.
#[allow(dead_code)]
fn demo_thread_1() {
    for i in 0..5 {
        kprintln!("[demo_thread_1] Executing step {} — yielding", i);
        process::scheduler::yield_now();
    }
    kprintln!("[demo_thread_1] Completed task.");
}

/// Demo thread 2: prints and cooperatively yields control.
#[allow(dead_code)]
fn demo_thread_2() {
    for i in 0..5 {
        kprintln!("[demo_thread_2] Executing step {} — yielding", i);
        process::scheduler::yield_now();
    }
    kprintln!("[demo_thread_2] Completed task.");
}
