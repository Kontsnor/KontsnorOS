//! Bus registration helpers for drivers.

/// PCI device identifier for driver matching.
#[derive(Debug, Clone)]
pub struct PciDeviceId {
    /// Vendor ID (e.g., 0x10DE for NVIDIA).
    pub vendor_id: u16,
    /// Device ID.
    pub device_id: u16,
    /// Class code (optional, 0 = don't care).
    pub class_code: u8,
    /// Subclass code (optional, 0 = don't care).
    pub subclass: u8,
}

impl PciDeviceId {
    /// Create a new PCI device identifier.
    pub const fn new(vendor_id: u16, device_id: u16) -> Self {
        Self {
            vendor_id,
            device_id,
            class_code: 0,
            subclass: 0,
        }
    }

    /// Match by vendor and class.
    pub const fn by_class(vendor_id: u16, class_code: u8, subclass: u8) -> Self {
        Self {
            vendor_id,
            device_id: 0,
            class_code,
            subclass,
        }
    }
}
