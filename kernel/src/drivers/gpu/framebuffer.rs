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

//! Framebuffer abstraction.
//!
//! Provides a software framebuffer that GPU drivers can render into.
//! This is the simplest form of display output and serves as a
//! fallback when no accelerated GPU driver is available.

use super::super::traits::FramebufferInfo;

/// A pixel color in ARGB8888 format.
#[derive(Debug, Clone, Copy)]
pub struct Color {
    /// Blue component (0–255).
    pub b: u8,
    /// Green component (0–255).
    pub g: u8,
    /// Red component (0–255).
    pub r: u8,
    /// Alpha component (0–255).
    pub a: u8,
}

impl Color {
    /// Create a new opaque color.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Black.
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    /// White.
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    /// KontsnorOS brand blue.
    pub const BRAND_BLUE: Color = Color::rgb(0, 120, 215);
    /// KontsnorOS brand accent.
    pub const BRAND_ACCENT: Color = Color::rgb(255, 185, 0);

    /// Convert to a 32-bit ARGB value.
    pub const fn to_argb32(self) -> u32 {
        ((self.a as u32) << 24) | ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

/// A software framebuffer.
///
/// This can be used by GPU drivers to provide a simple display
/// output, or as a fallback when no GPU driver is available.
pub struct Framebuffer {
    /// Pointer to the framebuffer memory.
    buffer: *mut u32,
    /// Framebuffer info.
    info: FramebufferInfo,
}

// SAFETY: The framebuffer is accessed through synchronized methods.
unsafe impl Send for Framebuffer {}
unsafe impl Sync for Framebuffer {}

impl Framebuffer {
    /// Create a new framebuffer from a physical address.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - `phys_addr` points to valid framebuffer memory
    /// - The memory is mapped and writable
    /// - No other code writes to this memory concurrently
    pub unsafe fn new(info: FramebufferInfo) -> Self {
        // TODO: Map the physical framebuffer address to virtual memory
        Self {
            buffer: info.phys_addr as *mut u32,
            info,
        }
    }

    /// Get framebuffer info.
    pub fn info(&self) -> &FramebufferInfo {
        &self.info
    }

    /// Set a pixel at (x, y) to the given color.
    pub fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x < self.info.width && y < self.info.height {
            let stride_pixels = (self.info.stride / 4) as usize;
            let offset = (y as usize) * stride_pixels + (x as usize);
            // SAFETY: We bounds-checked x and y against framebuffer dimensions.
            unsafe {
                self.buffer.add(offset).write_volatile(color.to_argb32());
            }
        }
    }

    /// Fill the entire framebuffer with a color.
    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.info.width, self.info.height, color);
    }

    /// Draw a filled rectangle.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        let max_x = x.saturating_add(w).min(self.info.width);
        let max_y = y.saturating_add(h).min(self.info.height);
        if x >= max_x || y >= max_y {
            return;
        }

        let stride_pixels = (self.info.stride / 4) as usize;
        let pixel_value = color.to_argb32();

        for py in y..max_y {
            let row_offset = (py as usize) * stride_pixels;
            for px in x..max_x {
                let offset = row_offset + (px as usize);
                // SAFETY: x and y loops are pre-clamped within 0..info.width and 0..info.height.
                unsafe {
                    self.buffer.add(offset).write_volatile(pixel_value);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_color_argb32() {
        let c = Color {
            r: 0x12,
            g: 0x34,
            b: 0x56,
            a: 0x78,
        };
        assert_eq!(c.to_argb32(), 0x78123456);

        let rgb = Color::rgb(0xAA, 0xBB, 0xCC);
        assert_eq!(rgb.a, 255);
        assert_eq!(rgb.to_argb32(), 0xFFAABBCC);
    }

    #[test_case]
    fn test_framebuffer_drawing() {
        let mut storage = [0u32; 16 * 16];
        let info = FramebufferInfo {
            phys_addr: storage.as_mut_ptr() as u64,
            width: 10,
            height: 10,
            stride: 16 * 4, // 16 pixels per row stride
            bpp: 32,
        };

        let mut fb = unsafe { Framebuffer::new(info) };

        // Test clear
        fb.clear(Color::BLACK);
        assert_eq!(storage[0], Color::BLACK.to_argb32());
        assert_eq!(storage[9 * 16 + 9], Color::BLACK.to_argb32());

        // Test set_pixel
        fb.set_pixel(2, 3, Color::WHITE);
        assert_eq!(storage[3 * 16 + 2], Color::WHITE.to_argb32());

        // Test set_pixel out of bounds
        fb.set_pixel(10, 10, Color::WHITE);
        assert_eq!(storage[10 * 16 + 10], Color::BLACK.to_argb32());

        // Test fill_rect within bounds
        fb.fill_rect(1, 1, 3, 2, Color::BRAND_BLUE);
        assert_eq!(storage[1 * 16 + 1], Color::BRAND_BLUE.to_argb32());
        assert_eq!(storage[1 * 16 + 3], Color::BRAND_BLUE.to_argb32());
        assert_eq!(storage[2 * 16 + 1], Color::BRAND_BLUE.to_argb32());
        assert_eq!(storage[2 * 16 + 3], Color::BRAND_BLUE.to_argb32());
        assert_eq!(storage[3 * 16 + 2], Color::WHITE.to_argb32()); // unchanged from set_pixel

        // Test fill_rect clipping out of bounds
        fb.fill_rect(8, 8, 5, 5, Color::BRAND_ACCENT);
        assert_eq!(storage[8 * 16 + 8], Color::BRAND_ACCENT.to_argb32());
        assert_eq!(storage[9 * 16 + 9], Color::BRAND_ACCENT.to_argb32());
    }
}
