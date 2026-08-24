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

//! DMA (Direct Memory Access) buffer management.
//!
//! Provides safe abstractions for DMA memory that must be:
//! - Physically contiguous
//! - Cache-coherent (or explicitly managed)
//! - Not moved by the allocator

/// A DMA-safe buffer.
///
/// DMA buffers are allocated from a special memory pool that guarantees
/// physical contiguity and proper alignment for hardware access.
#[derive(Debug)]
pub struct DmaBuffer {
    /// Virtual address of the buffer.
    pub virt_addr: u64,
    /// Physical address (for programming into hardware DMA registers).
    pub phys_addr: u64,
    /// Size of the buffer in bytes.
    pub size: usize,
}

/// DMA transfer direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaDirection {
    /// Transfer from device to memory (device writes, CPU reads).
    FromDevice,
    /// Transfer from memory to device (CPU writes, device reads).
    ToDevice,
    /// Bidirectional transfer.
    Bidirectional,
}

impl DmaBuffer {
    /// Get a slice of the buffer contents.
    ///
    /// # Safety
    ///
    /// The caller must ensure no DMA transfer is in progress.
    pub unsafe fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.virt_addr as *const u8, self.size) }
    }

    /// Get a mutable slice of the buffer contents.
    ///
    /// # Safety
    ///
    /// The caller must ensure no DMA transfer is in progress.
    pub unsafe fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.virt_addr as *mut u8, self.size) }
    }
}
