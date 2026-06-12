//! x86_64 architecture support for KontsnorOS.
//!
//! This module contains all x86_64-specific code including:
//! - GDT (Global Descriptor Table) setup
//! - IDT (Interrupt Descriptor Table) and interrupt handling
//! - Serial port (UART) driver for early console output
//! - CPU boot sequence

pub mod apic;
pub mod boot;
pub mod gdt;
pub mod interrupts;
pub mod serial;
pub mod smp;
