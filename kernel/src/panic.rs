//! Panic handler for KontsnorOS.
//!
//! When a panic occurs in the kernel, there is no runtime to catch it.
//! This module provides a custom panic handler that outputs diagnostic
//! information to the serial console and halts the CPU.

use crate::kprintln;
use core::panic::PanicInfo;

/// Custom panic handler — prints panic info to serial and halts.
///
/// # Safety
///
/// This function is called by the Rust runtime when a panic occurs.
/// It must never return, and it must not allocate or panic itself.
#[cfg(not(feature = "test"))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Disable interrupts to prevent further complications
    x86_64::instructions::interrupts::disable();

    kprintln!();
    kprintln!("!!! KERNEL PANIC !!!");
    kprintln!("====================");

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

    kprintln!("====================");
    kprintln!("System halted.");
    kprintln!();

    // Halt the CPU in an infinite loop
    loop {
        x86_64::instructions::hlt();
    }
}

/// Allocation error handler — called when a heap allocation fails.
#[alloc_error_handler]
fn alloc_error_handler(layout: alloc::alloc::Layout) -> ! {
    panic!(
        "Kernel heap allocation failed: size={}, align={}",
        layout.size(),
        layout.align()
    );
}
