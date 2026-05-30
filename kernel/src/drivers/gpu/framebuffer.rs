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
        ((self.a as u32) << 24)
            | ((self.r as u32) << 16)
            | ((self.g as u32) << 8)
            | (self.b as u32)
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
            let offset = (y * self.info.stride / 4 + x) as isize;
            // SAFETY: We bounds-checked x and y above.
            unsafe {
                self.buffer.offset(offset).write_volatile(color.to_argb32());
            }
        }
    }

    /// Fill the entire framebuffer with a color.
    pub fn clear(&mut self, color: Color) {
        let pixel_value = color.to_argb32();
        for y in 0..self.info.height {
            for x in 0..self.info.width {
                let offset = (y * self.info.stride / 4 + x) as isize;
                // SAFETY: We are within the framebuffer bounds.
                unsafe {
                    self.buffer.offset(offset).write_volatile(pixel_value);
                }
            }
        }
    }

    /// Draw a filled rectangle.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        let pixel_value = color.to_argb32();
        for dy in 0..h {
            for dx in 0..w {
                let px = x + dx;
                let py = y + dy;
                if px < self.info.width && py < self.info.height {
                    let offset = (py * self.info.stride / 4 + px) as isize;
                    unsafe {
                        self.buffer.offset(offset).write_volatile(pixel_value);
                    }
                }
            }
        }
    }
}
