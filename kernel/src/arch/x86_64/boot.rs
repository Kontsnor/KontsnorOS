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
    cr0.insert(Cr0Flags::MONITOR_COPROCESSOR);
    unsafe {
        Cr0::write(cr0);
    }
}
