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
