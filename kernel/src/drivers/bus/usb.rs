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

//! USB bus driver framework.
//!
//! Provides the foundation for USB host controller drivers (xHCI, EHCI)
//! and USB device class drivers (HID, mass storage, etc.).
//!
//! ## USB Architecture
//!
//! ```text
//! ┌──────────────────────────────────────┐
//! │       USB Device Class Drivers       │  HID, Mass Storage, Audio, etc.
//! ├──────────────────────────────────────┤
//! │         USB Core Layer               │  Device enumeration, configuration
//! ├──────────────────────────────────────┤
//! │   Host Controller Drivers (HCD)      │  xHCI, EHCI, UHCI, OHCI
//! ├──────────────────────────────────────┤
//! │         Hardware (HCI)               │  USB Host Controllers
//! └──────────────────────────────────────┘
//! ```

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::kprintln;

/// USB speed classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbSpeed {
    /// USB 1.0 — 1.5 Mbps.
    Low,
    /// USB 1.1 — 12 Mbps.
    Full,
    /// USB 2.0 — 480 Mbps.
    High,
    /// USB 3.0 — 5 Gbps.
    Super,
    /// USB 3.1 — 10 Gbps.
    SuperPlus,
    /// USB 3.2 / USB4 — 20+ Gbps.
    SuperPlusPlus,
}

/// USB device class codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UsbClass {
    /// Use interface descriptors.
    PerInterface = 0x00,
    /// Audio device.
    Audio = 0x01,
    /// Communications (CDC).
    Cdc = 0x02,
    /// Human Interface Device.
    Hid = 0x03,
    /// Physical.
    Physical = 0x05,
    /// Image (scanner, camera).
    Image = 0x06,
    /// Printer.
    Printer = 0x07,
    /// Mass Storage.
    MassStorage = 0x08,
    /// USB Hub.
    Hub = 0x09,
    /// Video.
    Video = 0x0E,
    /// Wireless.
    Wireless = 0xE0,
    /// Vendor specific.
    VendorSpecific = 0xFF,
}

/// A USB device descriptor (18 bytes).
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct UsbDeviceDescriptor {
    /// Descriptor length (18).
    pub b_length: u8,
    /// Descriptor type (1 = device).
    pub b_descriptor_type: u8,
    /// USB spec version (BCD, e.g., 0x0200 = USB 2.0).
    pub bcd_usb: u16,
    /// Device class.
    pub b_device_class: u8,
    /// Device subclass.
    pub b_device_sub_class: u8,
    /// Device protocol.
    pub b_device_protocol: u8,
    /// Max packet size for endpoint 0.
    pub b_max_packet_size0: u8,
    /// Vendor ID.
    pub id_vendor: u16,
    /// Product ID.
    pub id_product: u16,
    /// Device release number (BCD).
    pub bcd_device: u16,
    /// Manufacturer string index.
    pub i_manufacturer: u8,
    /// Product string index.
    pub i_product: u8,
    /// Serial number string index.
    pub i_serial_number: u8,
    /// Number of configurations.
    pub b_num_configurations: u8,
}

/// A discovered USB device.
#[derive(Debug, Clone)]
pub struct UsbDevice {
    /// Bus number.
    pub bus: u8,
    /// Device address (1-127).
    pub address: u8,
    /// Device speed.
    pub speed: UsbSpeed,
    /// Vendor ID.
    pub vendor_id: u16,
    /// Product ID.
    pub product_id: u16,
    /// Device class.
    pub device_class: u8,
    /// Device name.
    pub name: String,
}

/// USB transfer types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferType {
    /// Control transfers (for setup/config).
    Control,
    /// Interrupt transfers (for HID devices).
    Interrupt,
    /// Bulk transfers (for mass storage).
    Bulk,
    /// Isochronous transfers (for audio/video).
    Isochronous,
}

/// USB request types for control transfers.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum UsbRequestType {
    /// GET_STATUS.
    GetStatus = 0x00,
    /// CLEAR_FEATURE.
    ClearFeature = 0x01,
    /// SET_FEATURE.
    SetFeature = 0x03,
    /// SET_ADDRESS.
    SetAddress = 0x05,
    /// GET_DESCRIPTOR.
    GetDescriptor = 0x06,
    /// SET_DESCRIPTOR.
    SetDescriptor = 0x07,
    /// GET_CONFIGURATION.
    GetConfiguration = 0x08,
    /// SET_CONFIGURATION.
    SetConfiguration = 0x09,
}

/// Trait for USB host controller drivers.
pub trait UsbHostController: Send + Sync {
    /// Get the controller name.
    fn name(&self) -> &str;

    /// Reset the controller.
    fn reset(&mut self) -> Result<(), UsbError>;

    /// Start the controller.
    fn start(&mut self) -> Result<(), UsbError>;

    /// Enumerate connected devices.
    fn enumerate(&mut self) -> Result<Vec<UsbDevice>, UsbError>;

    /// Submit a control transfer.
    fn control_transfer(
        &mut self,
        device: u8,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> Result<usize, UsbError>;
}

/// USB error codes.
#[derive(Debug, Clone, Copy)]
pub enum UsbError {
    /// Device not found.
    DeviceNotFound,
    /// Transfer timed out.
    Timeout,
    /// Stall condition (endpoint error).
    Stall,
    /// CRC error.
    CrcError,
    /// Buffer overrun.
    Overrun,
    /// No memory.
    NoMemory,
    /// Controller not ready.
    NotReady,
    /// Invalid parameter.
    InvalidParam,
}

/// Global USB device registry.
static USB_DEVICES: Mutex<Option<Vec<UsbDevice>>> = Mutex::new(None);

/// Initialize the USB subsystem.
pub fn init() {
    *USB_DEVICES.lock() = Some(Vec::new());
    kprintln!("[usb] USB subsystem initialized.");
}

/// Register a discovered USB device.
pub fn register_device(device: UsbDevice) {
    if let Some(ref mut devices) = *USB_DEVICES.lock() {
        kprintln!(
            "[usb] Found device: {:04x}:{:04x} - {}",
            device.vendor_id,
            device.product_id,
            device.name
        );
        devices.push(device);
    }
}
