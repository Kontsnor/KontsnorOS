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

//! Boot sequence for x86_64.
//!
//! This module handles the early boot process after the bootloader
//! hands control to the kernel. It coordinates the initialization
//! of all architecture-specific components.
//!
//! The boot sequence is:
//! 1. Serial port initialization (for logging)
//! 2. GDT setup (memory segmentation)
//! 3. IDT setup (interrupt handling)
//! 4. PIC initialization (hardware interrupts)
//! 5. Memory subsystem initialization
//! 6. Enable interrupts

/// Initialize all x86_64 architecture components.
///
/// This is called from `kernel_main` and sets up all hardware-specific
/// components in the correct order.
pub fn init() {
    super::serial::init();
    super::gdt::init();
    super::interrupts::init_idt();
    super::interrupts::init_pics();

    // Enable SSE support (required for user-space programs compiled with SSE)
    unsafe {
        enable_sse();
        enable_fsgsbase();
    }
}

pub unsafe fn enable_sse() {
    use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};

    let mut cr4 = Cr4::read();
    cr4.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE);
    unsafe {
        Cr4::write(cr4);
    }

    let mut cr0 = Cr0::read();
    cr0.remove(Cr0Flags::EMULATE_COPROCESSOR);
    cr0.insert(Cr0Flags::MONITOR_COPROCESSOR | Cr0Flags::WRITE_PROTECT);
    unsafe {
        Cr0::write(cr0);
    }
}

pub unsafe fn enable_fsgsbase() {
    // SAFETY: Enabling FSGSBASE CR4 bit is safe on x86_64 processors
    unsafe {
        core::arch::asm!(
            "mov rax, cr4",
            "or rax, 0x10000", // Bit 16 (FSGSBASE)
            "mov cr4, rax",
            out("rax") _,
        );
    }
}
