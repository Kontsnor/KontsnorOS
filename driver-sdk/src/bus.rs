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
