//! PCI bus enumeration and configuration.
//!
//! This module provides access to PCI devices through configuration
//! space reads/writes. It enumerates all devices on the PCI bus and
//! makes them available for driver binding.
//!
//! ## PCI Configuration Space Access
//!
//! On x86_64, PCI configuration space is accessed through I/O ports:
//! - Port 0xCF8: CONFIG_ADDRESS (32-bit)
//! - Port 0xCFC: CONFIG_DATA (32-bit)

use crate::kprintln;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::port::Port;

/// CONFIG_ADDRESS I/O port.
const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
/// CONFIG_DATA I/O port.
const PCI_CONFIG_DATA: u16 = 0xCFC;

/// List of discovered PCI devices.
static PCI_DEVICES: Mutex<Vec<PciDevice>> = Mutex::new(Vec::new());

/// A PCI device identified during bus enumeration.
#[derive(Debug, Clone)]
pub struct PciDevice {
    /// PCI bus number (0–255).
    pub bus: u8,
    /// Device number on the bus (0–31).
    pub device: u8,
    /// Function number (0–7).
    pub function: u8,
    /// Vendor ID.
    pub vendor_id: u16,
    /// Device ID.
    pub device_id: u16,
    /// Class code (identifies device type).
    pub class_code: u8,
    /// Subclass code.
    pub subclass: u8,
    /// Programming interface.
    pub prog_if: u8,
    /// Revision ID.
    pub revision: u8,
    /// Header type.
    pub header_type: u8,
}

impl PciDevice {
    /// Check if this is a multi-function device.
    pub fn is_multifunction(&self) -> bool {
        self.header_type & 0x80 != 0
    }

    /// Get a human-readable class description.
    pub fn class_name(&self) -> &'static str {
        match (self.class_code, self.subclass) {
            (0x00, _) => "Unclassified",
            (0x01, 0x00) => "SCSI Bus Controller",
            (0x01, 0x01) => "IDE Controller",
            (0x01, 0x06) => "SATA Controller",
            (0x01, 0x08) => "NVMe Controller",
            (0x01, _) => "Mass Storage Controller",
            (0x02, 0x00) => "Ethernet Controller",
            (0x02, _) => "Network Controller",
            (0x03, 0x00) => "VGA Compatible Controller",
            (0x03, 0x01) => "XGA Controller",
            (0x03, 0x02) => "3D Controller",
            (0x03, _) => "Display Controller",
            (0x04, _) => "Multimedia Controller",
            (0x05, _) => "Memory Controller",
            (0x06, 0x00) => "Host Bridge",
            (0x06, 0x01) => "ISA Bridge",
            (0x06, 0x04) => "PCI-to-PCI Bridge",
            (0x06, _) => "Bridge Device",
            (0x07, _) => "Communication Controller",
            (0x08, _) => "System Peripheral",
            (0x0C, 0x03) => "USB Controller",
            (0x0C, _) => "Serial Bus Controller",
            _ => "Unknown",
        }
    }

    /// Get the vendor name (for common vendors).
    pub fn vendor_name(&self) -> &'static str {
        match self.vendor_id {
            0x8086 => "Intel",
            0x10DE => "NVIDIA",
            0x1002 => "AMD/ATI",
            0x1022 => "AMD",
            0x14E4 => "Broadcom",
            0x10EC => "Realtek",
            0x1B36 => "Red Hat (QEMU)",
            0x1AF4 => "Red Hat (VirtIO)",
            _ => "Unknown",
        }
    }
}

/// Read a 32-bit value from PCI configuration space.
fn pci_config_read(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address: u32 = (1u32 << 31)  // Enable bit
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);

    // SAFETY: PCI configuration space I/O ports are standard on x86.
    unsafe {
        let mut addr_port = Port::<u32>::new(PCI_CONFIG_ADDRESS);
        let mut data_port = Port::<u32>::new(PCI_CONFIG_DATA);
        addr_port.write(address);
        data_port.read()
    }
}

/// Probe a specific bus/device/function for a PCI device.
fn probe_device(bus: u8, device: u8, function: u8) -> Option<PciDevice> {
    let vendor_device = pci_config_read(bus, device, function, 0);
    let vendor_id = (vendor_device & 0xFFFF) as u16;

    if vendor_id == 0xFFFF {
        return None; // No device present
    }

    let device_id = ((vendor_device >> 16) & 0xFFFF) as u16;

    let class_rev = pci_config_read(bus, device, function, 8);
    let revision = (class_rev & 0xFF) as u8;
    let prog_if = ((class_rev >> 8) & 0xFF) as u8;
    let subclass = ((class_rev >> 16) & 0xFF) as u8;
    let class_code = ((class_rev >> 24) & 0xFF) as u8;

    let header = pci_config_read(bus, device, function, 0x0C);
    let header_type = ((header >> 16) & 0xFF) as u8;

    Some(PciDevice {
        bus,
        device,
        function,
        vendor_id,
        device_id,
        class_code,
        subclass,
        prog_if,
        revision,
        header_type,
    })
}

/// Enumerate all PCI devices.
fn enumerate_bus() -> Vec<PciDevice> {
    let mut devices = Vec::new();

    for bus in 0..=255u16 {
        for device in 0..32u8 {
            // Check function 0
            if let Some(dev) = probe_device(bus as u8, device, 0) {
                let is_multi = dev.is_multifunction();
                devices.push(dev);

                // Check remaining functions if multi-function
                if is_multi {
                    for function in 1..8u8 {
                        if let Some(dev) = probe_device(bus as u8, device, function) {
                            devices.push(dev);
                        }
                    }
                }
            }
        }
    }

    devices
}

/// Initialize PCI bus enumeration.
pub fn init() {
    let devices = enumerate_bus();
    let count = devices.len();

    kprintln!("[pci] Enumerated {} PCI device(s):", count);
    for dev in &devices {
        kprintln!(
            "  [{:02x}:{:02x}.{:01x}] {:04x}:{:04x} {} — {}",
            dev.bus,
            dev.device,
            dev.function,
            dev.vendor_id,
            dev.device_id,
            dev.vendor_name(),
            dev.class_name()
        );
    }

    *PCI_DEVICES.lock() = devices;
}

/// Get all discovered PCI devices.
pub fn devices() -> Vec<PciDevice> {
    PCI_DEVICES.lock().clone()
}

/// Find PCI devices by vendor and device ID.
pub fn find_device(vendor_id: u16, device_id: u16) -> Vec<PciDevice> {
    PCI_DEVICES
        .lock()
        .iter()
        .filter(|d| d.vendor_id == vendor_id && d.device_id == device_id)
        .cloned()
        .collect()
}

/// Find PCI devices by class code.
pub fn find_by_class(class_code: u8, subclass: u8) -> Vec<PciDevice> {
    PCI_DEVICES
        .lock()
        .iter()
        .filter(|d| d.class_code == class_code && d.subclass == subclass)
        .cloned()
        .collect()
}

/// Read a 32-bit value from PCI configuration space.
pub fn read_config(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pci_config_read(bus, device, function, offset)
}

/// Write a 32-bit value to PCI configuration space.
pub fn write_config(bus: u8, device: u8, function: u8, offset: u8, val: u32) {
    let address: u32 = (1u32 << 31)  // Enable bit
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);

    // SAFETY: PCI configuration space I/O ports are standard on x86.
    unsafe {
        let mut addr_port = Port::<u32>::new(PCI_CONFIG_ADDRESS);
        let mut data_port = Port::<u32>::new(PCI_CONFIG_DATA);
        addr_port.write(address);
        data_port.write(val);
    }
}
