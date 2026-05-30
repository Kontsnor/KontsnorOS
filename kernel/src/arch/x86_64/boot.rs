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
}
