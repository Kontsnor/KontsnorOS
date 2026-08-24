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

//! Core driver traits — re-exported from the kernel for stable ABI.
//!
//! These traits define what a driver must implement to work with
//! KontsnorOS. They are versioned and backward-compatible.

use alloc::string::String;
use alloc::vec::Vec;

/// Information about a registered driver.
#[derive(Debug, Clone)]
pub struct DriverInfo {
    /// Driver name (e.g., "nvidia-gpu", "amd-radeon").
    pub name: String,
    /// Driver version string (semver recommended).
    pub version: String,
    /// Driver author or vendor.
    pub author: String,
    /// Driver license identifier.
    pub license: String,
    /// Brief description of the driver.
    pub description: String,
}

/// Error type for driver operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverError {
    /// The operation is not supported.
    NotSupported,
    /// An I/O error occurred.
    IoError,
    /// The device is not ready.
    NotReady,
    /// The device is busy.
    Busy,
    /// Invalid parameter.
    InvalidParam,
    /// Out of memory.
    OutOfMemory,
    /// Hardware timeout.
    Timeout,
    /// Device not found.
    NotFound,
    /// Permission denied.
    PermissionDenied,
}

/// Trait for character devices (serial ports, terminals, etc.).
pub trait CharDevice: Send + Sync {
    /// Read bytes from the device.
    fn read(&self, buf: &mut [u8]) -> Result<usize, DriverError>;
    /// Write bytes to the device.
    fn write(&self, data: &[u8]) -> Result<usize, DriverError>;
    /// Device-specific I/O control.
    fn ioctl(&self, _request: u64, _arg: u64) -> Result<u64, DriverError> {
        Err(DriverError::NotSupported)
    }
    /// Get driver information.
    fn info(&self) -> DriverInfo;
}

/// Trait for block devices (disks, SSDs, etc.).
pub trait BlockDevice: Send + Sync {
    /// Read blocks from the device.
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), DriverError>;
    /// Write blocks to the device.
    fn write_block(&self, block: u64, data: &[u8]) -> Result<(), DriverError>;
    /// Get the block size in bytes.
    fn block_size(&self) -> u64;
    /// Get the total number of blocks.
    fn block_count(&self) -> u64;
    /// Flush cached writes.
    fn flush(&self) -> Result<(), DriverError> {
        Ok(())
    }
    /// Get driver information.
    fn info(&self) -> DriverInfo;
}

/// Trait for network devices.
pub trait NetDevice: Send + Sync {
    /// Send a packet.
    fn send(&self, data: &[u8]) -> Result<(), DriverError>;
    /// Receive a packet.
    fn recv(&self, buf: &mut [u8]) -> Result<usize, DriverError>;
    /// Get the MAC address.
    fn mac_address(&self) -> [u8; 6];
    /// Get driver information.
    fn info(&self) -> DriverInfo;
}

/// Display mode configuration.
#[derive(Debug, Clone, Copy)]
pub struct DisplayMode {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Refresh rate in Hz.
    pub refresh_rate: u32,
    /// Bits per pixel.
    pub bpp: u32,
}

/// Display output information.
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    /// Display index.
    pub id: u32,
    /// Display name.
    pub name: String,
    /// Whether a monitor is connected.
    pub connected: bool,
    /// Supported modes.
    pub modes: Vec<DisplayMode>,
}

/// Framebuffer information.
#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    /// Physical address.
    pub phys_addr: u64,
    /// Size in bytes.
    pub size: u64,
    /// Stride (bytes per row).
    pub stride: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per pixel.
    pub bpp: u32,
}

/// GPU memory handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuMemHandle(pub u64);

/// Trait for GPU devices — designed for NVIDIA, AMD, and other GPU vendors.
pub trait GpuDevice: Send + Sync {
    /// Initialize GPU hardware.
    fn init_hw(&self) -> Result<(), DriverError>;
    /// Get display output information.
    fn get_display_info(&self) -> Vec<DisplayInfo>;
    /// Set a display mode.
    fn set_mode(&self, display: u32, mode: &DisplayMode) -> Result<(), DriverError>;
    /// Get the framebuffer for a display.
    fn get_framebuffer(&self, display: u32) -> Result<FramebufferInfo, DriverError>;
    /// Submit a command buffer.
    fn submit_commands(&self, _commands: &[u8]) -> Result<u64, DriverError> {
        Err(DriverError::NotSupported)
    }
    /// Wait for a fence to complete.
    fn wait_fence(&self, _fence_id: u64) -> Result<(), DriverError> {
        Err(DriverError::NotSupported)
    }
    /// Allocate GPU memory.
    fn alloc_vram(&self, _size: u64) -> Result<GpuMemHandle, DriverError> {
        Err(DriverError::NotSupported)
    }
    /// Free GPU memory.
    fn free_vram(&self, _handle: GpuMemHandle) -> Result<(), DriverError> {
        Err(DriverError::NotSupported)
    }
    /// Get driver information.
    fn info(&self) -> DriverInfo;
}
