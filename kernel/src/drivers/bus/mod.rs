//! Bus abstraction layer.
//!
//! Provides device bus enumeration and management for PCI,
//! USB, and platform devices.

pub mod pci;
pub mod platform;
pub mod usb;

/// Initialize all bus subsystems.
pub fn init() {
    pci::init();
    platform::init();
    usb::init();
}
