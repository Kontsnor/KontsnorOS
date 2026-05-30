//! Core driver traits for KontsnorOS.
//!
//! These traits define the interface between the kernel and hardware
//! drivers. They are designed to be:
//!
//! 1. **Safe** — Drivers use safe Rust by default; `unsafe` is confined
//!    to the kernel's internal implementations.
//! 2. **Stable** — The trait interface is versioned and changes are
//!    backward-compatible when possible.
//! 3. **Ergonomic** — Clear, well-documented methods with sensible defaults.
//!
//! ## For Third-Party Driver Developers
//!
//! If you're writing a driver for KontsnorOS (e.g., a GPU driver for
//! NVIDIA or AMD hardware), implement the appropriate trait(s) from
//! the `driver-sdk` crate, which re-exports these traits with a stable ABI.

use alloc::string::String;
use alloc::vec::Vec;

/// Information about a registered driver.
#[derive(Debug, Clone)]
pub struct DriverInfo {
    /// Driver name (e.g., "nvidia-gpu", "amd-radeon").
    pub name: String,
    /// Driver version.
    pub version: String,
    /// Driver author/vendor.
    pub author: String,
    /// Driver license (e.g., "MIT", "GPL-2.0", "Proprietary").
    pub license: String,
    /// Brief description.
    pub description: String,
}

/// Trait for character devices.
///
/// Character devices transfer data as a stream of bytes (e.g., serial
/// ports, terminals, keyboards, mice).
pub trait CharDevice: Send + Sync {
    /// Read bytes from the device.
    fn read(&self, buf: &mut [u8]) -> Result<usize, DriverError>;

    /// Write bytes to the device.
    fn write(&self, data: &[u8]) -> Result<usize, DriverError>;

    /// Device-specific I/O control.
    fn ioctl(&self, _request: u64, _arg: u64) -> Result<u64, DriverError> {
        Err(DriverError::NotSupported)
    }

    /// Check if data is available for reading.
    fn poll(&self) -> PollResult {
        PollResult::empty()
    }

    /// Get driver information.
    fn info(&self) -> DriverInfo;
}

/// Trait for block devices.
///
/// Block devices transfer data in fixed-size blocks (e.g., hard drives,
/// SSDs, USB storage).
pub trait BlockDevice: Send + Sync {
    /// Read blocks from the device.
    ///
    /// `block` is the starting block number, `buf` must be a multiple
    /// of the block size.
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), DriverError>;

    /// Write blocks to the device.
    fn write_block(&self, block: u64, data: &[u8]) -> Result<(), DriverError>;

    /// Get the block size in bytes (typically 512 or 4096).
    fn block_size(&self) -> u64;

    /// Get the total number of blocks.
    fn block_count(&self) -> u64;

    /// Flush any cached writes to the device.
    fn flush(&self) -> Result<(), DriverError> {
        Ok(())
    }

    /// Get driver information.
    fn info(&self) -> DriverInfo;
}

/// Trait for network devices.
///
/// Network devices transmit and receive packets.
pub trait NetDevice: Send + Sync {
    /// Send a packet.
    fn send(&self, data: &[u8]) -> Result<(), DriverError>;

    /// Receive a packet.
    fn recv(&self, buf: &mut [u8]) -> Result<usize, DriverError>;

    /// Get the MAC address.
    fn mac_address(&self) -> [u8; 6];

    /// Get the link status.
    fn link_status(&self) -> LinkStatus;

    /// Set the device up (enable).
    fn up(&self) -> Result<(), DriverError>;

    /// Set the device down (disable).
    fn down(&self) -> Result<(), DriverError>;

    /// Get driver information.
    fn info(&self) -> DriverInfo;
}

/// Trait for GPU devices.
///
/// GPU devices provide graphics acceleration and compute capabilities.
/// This trait is specifically designed to be attractive to companies
/// like NVIDIA and AMD for driver development.
pub trait GpuDevice: Send + Sync {
    /// Initialize the GPU hardware.
    fn init_hw(&self) -> Result<(), DriverError>;

    /// Get information about available display outputs.
    fn get_display_info(&self) -> Vec<DisplayInfo>;

    /// Set a display mode.
    fn set_mode(&self, display: u32, mode: &DisplayMode) -> Result<(), DriverError>;

    /// Get the framebuffer for a display.
    fn get_framebuffer(&self, display: u32) -> Result<FramebufferInfo, DriverError>;

    /// Submit a command buffer for GPU execution.
    fn submit_commands(&self, _commands: &[u8]) -> Result<u64, DriverError> {
        Err(DriverError::NotSupported)
    }

    /// Wait for a submitted command buffer to complete.
    fn wait_fence(&self, _fence_id: u64) -> Result<(), DriverError> {
        Err(DriverError::NotSupported)
    }

    /// Allocate GPU memory (VRAM).
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

// ═══════════════════════════════════════════════════════════════════════
// Supporting types
// ═══════════════════════════════════════════════════════════════════════

/// Driver error type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverError {
    /// The operation is not supported by this device.
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

/// Polling result flags.
#[derive(Debug, Clone, Copy)]
pub struct PollResult {
    /// Data is available for reading.
    pub readable: bool,
    /// The device is ready for writing.
    pub writable: bool,
    /// An error or exceptional condition occurred.
    pub error: bool,
}

impl PollResult {
    /// No events pending.
    pub fn empty() -> Self {
        Self {
            readable: false,
            writable: false,
            error: false,
        }
    }
}

/// Network link status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStatus {
    /// Link is up and operational.
    Up,
    /// Link is down.
    Down,
    /// Link status is unknown.
    Unknown,
}

/// Display output information.
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    /// Display index.
    pub id: u32,
    /// Display name (e.g., "HDMI-1", "DP-0").
    pub name: String,
    /// Whether a monitor is connected.
    pub connected: bool,
    /// Supported display modes.
    pub modes: Vec<DisplayMode>,
}

/// A display mode (resolution + refresh rate).
#[derive(Debug, Clone, Copy)]
pub struct DisplayMode {
    /// Horizontal resolution in pixels.
    pub width: u32,
    /// Vertical resolution in pixels.
    pub height: u32,
    /// Refresh rate in Hz.
    pub refresh_rate: u32,
    /// Color depth in bits per pixel.
    pub bpp: u32,
}

/// Information about a framebuffer.
#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    /// Physical address of the framebuffer.
    pub phys_addr: u64,
    /// Size of the framebuffer in bytes.
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

/// A handle to GPU-allocated memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuMemHandle(pub u64);
