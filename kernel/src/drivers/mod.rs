//! Driver framework for KontsnorOS.
//!
//! This module provides the infrastructure for hardware drivers,
//! including:
//! - Core driver traits (CharDevice, BlockDevice, NetDevice, GpuDevice)
//! - Driver lifecycle management (probe, bind, unbind)
//! - Bus abstraction (PCI, platform)
//! - Built-in drivers (serial console, framebuffer)
//!
//! ## Driver Architecture
//!
//! KontsnorOS uses a trait-based driver model that encourages safe,
//! modular driver development. Third-party drivers (e.g., from NVIDIA
//! or AMD) implement the public traits from the `driver-sdk` crate.

pub mod bus;
pub mod console;
pub mod gpu;
pub mod keyboard;
pub mod traits;
pub mod ramdisk;
pub mod block;

use alloc::vec::Vec;
use spin::Mutex;
use crate::kprintln;

use traits::DriverInfo;

/// Global driver registry.
static DRIVER_REGISTRY: Mutex<Vec<DriverInfo>> = Mutex::new(Vec::new());

/// Initialize the driver framework.
pub fn init() {
    // Initialize bus subsystems
    bus::init();

    // Initialize built-in drivers
    keyboard::init();
    console::init();
    gpu::init();

    kprintln!("[drivers] Driver framework initialized.");
}

/// Register a driver with the kernel.
pub fn register_driver(info: DriverInfo) {
    kprintln!("[drivers] Registered driver: {} v{}", info.name, info.version);
    DRIVER_REGISTRY.lock().push(info);
}

/// List all registered drivers.
pub fn list_drivers() -> Vec<DriverInfo> {
    DRIVER_REGISTRY.lock().clone()
}
