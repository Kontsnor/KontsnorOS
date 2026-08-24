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

//! I/O port and MMIO (Memory-Mapped I/O) helpers.
//!
//! Provides safe abstractions for hardware I/O operations that
//! drivers need to communicate with devices.

/// A port I/O address.
#[derive(Debug, Clone, Copy)]
pub struct IoPort {
    /// The I/O port number.
    pub port: u16,
}

/// A memory-mapped I/O region.
///
/// MMIO regions are mapped into the kernel's virtual address space
/// and accessed using volatile reads/writes.
#[derive(Debug)]
pub struct MmioRegion {
    /// Base virtual address of the MMIO region.
    pub base: u64,
    /// Size of the region in bytes.
    pub size: u64,
}

impl MmioRegion {
    /// Read a 32-bit value from the MMIO region at the given offset.
    ///
    /// # Safety
    ///
    /// The offset must be within the region bounds and properly aligned.
    pub unsafe fn read_u32(&self, offset: u64) -> u32 {
        let addr = (self.base + offset) as *const u32;
        unsafe { core::ptr::read_volatile(addr) }
    }

    /// Write a 32-bit value to the MMIO region at the given offset.
    ///
    /// # Safety
    ///
    /// The offset must be within the region bounds and properly aligned.
    pub unsafe fn write_u32(&self, offset: u64, value: u32) {
        let addr = (self.base + offset) as *mut u32;
        unsafe { core::ptr::write_volatile(addr, value) }
    }

    /// Read a 64-bit value from the MMIO region.
    ///
    /// # Safety
    ///
    /// The offset must be within the region bounds and properly aligned.
    pub unsafe fn read_u64(&self, offset: u64) -> u64 {
        let addr = (self.base + offset) as *const u64;
        unsafe { core::ptr::read_volatile(addr) }
    }

    /// Write a 64-bit value to the MMIO region.
    ///
    /// # Safety
    ///
    /// The offset must be within the region bounds and properly aligned.
    pub unsafe fn write_u64(&self, offset: u64, value: u64) {
        let addr = (self.base + offset) as *mut u64;
        unsafe { core::ptr::write_volatile(addr, value) }
    }
}
