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

/// Linux fb_fix_screeninfo struct (FBIOGET_FSCREENINFO = 0x4602).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FbFixScreenInfo {
    pub id: [u8; 16],
    pub smem_start: u64,
    pub smem_len: u32,
    pub type_: u32,
    pub type_aux: u32,
    pub visual: u32,
    pub xpanstep: u16,
    pub ypanstep: u16,
    pub ywrapstep: u16,
    pub line_length: u32,
    pub mmio_start: u64,
    pub mmio_len: u32,
    pub accel: u32,
    pub capabilities: u16,
    pub reserved: [u16; 2],
}

/// Linux fb_bitfield struct.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FbBitfield {
    pub offset: u32,
    pub length: u32,
    pub msb_right: u32,
}

/// Linux fb_var_screeninfo struct (FBIOGET_VSCREENINFO = 0x4600).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FbVarScreenInfo {
    pub xres: u32,
    pub yres: u32,
    pub xres_virtual: u32,
    pub yres_virtual: u32,
    pub xoffset: u32,
    pub yoffset: u32,
    pub bits_per_pixel: u32,
    pub grayscale: u32,
    pub red: FbBitfield,
    pub green: FbBitfield,
    pub blue: FbBitfield,
    pub transp: FbBitfield,
    pub nonstd: u32,
    pub activate: u32,
    pub height: u32,
    pub width: u32,
    pub accel_flags: u32,
    pub pixclock: u32,
    pub left_margin: u32,
    pub right_margin: u32,
    pub upper_margin: u32,
    pub lower_margin: u32,
    pub hsync_len: u32,
    pub vsync_len: u32,
    pub sync: u32,
    pub vmode: u32,
    pub rotate: u32,
    pub colorspace: u32,
    pub reserved: [u32; 4],
}

/// `/dev/fb0` character device inode for userspace framebuffer access.
pub struct DevFb0 {
    pub inode: crate::fs::inode::Inode,
}

impl DevFb0 {
    pub fn new() -> Self {
        Self {
            inode: crate::fs::inode::Inode::new(18, crate::fs::inode::FileType::CharDevice)
                .with_dev(crate::fs::devfs::DEVFS_DEV_ID),
        }
    }
}

impl crate::fs::inode::InodeOps for DevFb0 {
    fn inode(&self) -> &crate::fs::inode::Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let console = super::bochs::GRAPHICS_CONSOLE.lock();
        if let Some(ref gc) = *console {
            let fb_size = gc.gpu.size;
            if offset >= fb_size {
                return Ok(0);
            }
            let to_read = core::cmp::min(buf.len() as u64, fb_size - offset) as usize;
            unsafe {
                let src = (gc.gpu.lfb_virt + offset) as *const u8;
                core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), to_read);
            }
            Ok(to_read)
        } else {
            Err(-6) // ENXIO
        }
    }

    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, i32> {
        let console = super::bochs::GRAPHICS_CONSOLE.lock();
        if let Some(ref gc) = *console {
            let fb_size = gc.gpu.size;
            if offset >= fb_size {
                return Ok(0);
            }
            let to_write = core::cmp::min(buf.len() as u64, fb_size - offset) as usize;
            unsafe {
                let dst = (gc.gpu.lfb_virt + offset) as *mut u8;
                core::ptr::copy_nonoverlapping(buf.as_ptr(), dst, to_write);
            }
            Ok(to_write)
        } else {
            Err(-6) // ENXIO
        }
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        let console = super::bochs::GRAPHICS_CONSOLE.lock();
        let gc = match *console {
            Some(ref gc) => gc,
            None => return Err(-6), // ENXIO
        };

        match request {
            0x4600 | 0x4602 => {
                // FBIOGET_VSCREENINFO or FBIOGET_FSCREENINFO
                if request == 0x4602 {
                    // FBIOGET_FSCREENINFO
                    if !crate::syscall::fs::validate_user_ptr(
                        arg as *const u8,
                        core::mem::size_of::<FbFixScreenInfo>(),
                    ) {
                        return Err(-14); // EFAULT
                    }
                    let mut id = [0u8; 16];
                    let name = b"bochs-vbe\0";
                    id[..name.len()].copy_from_slice(name);

                    let fix = FbFixScreenInfo {
                        id,
                        smem_start: gc.gpu.lfb_phys,
                        smem_len: gc.gpu.size as u32,
                        type_: 0, // FB_TYPE_PACKED_PIXELS
                        type_aux: 0,
                        visual: 2, // FB_VISUAL_TRUECOLOR
                        xpanstep: 0,
                        ypanstep: 0,
                        ywrapstep: 0,
                        line_length: gc.gpu.width * 4,
                        mmio_start: 0,
                        mmio_len: 0,
                        accel: 0,
                        capabilities: 0,
                        reserved: [0; 2],
                    };
                    unsafe {
                        core::ptr::write(arg as *mut FbFixScreenInfo, fix);
                    }
                } else {
                    // FBIOGET_VSCREENINFO
                    if !crate::syscall::fs::validate_user_ptr(
                        arg as *const u8,
                        core::mem::size_of::<FbVarScreenInfo>(),
                    ) {
                        return Err(-14); // EFAULT
                    }
                    let var = FbVarScreenInfo {
                        xres: gc.gpu.width,
                        yres: gc.gpu.height,
                        xres_virtual: gc.gpu.width,
                        yres_virtual: gc.gpu.height,
                        xoffset: 0,
                        yoffset: 0,
                        bits_per_pixel: gc.gpu.bpp,
                        grayscale: 0,
                        red: FbBitfield {
                            offset: 16,
                            length: 8,
                            msb_right: 0,
                        },
                        green: FbBitfield {
                            offset: 8,
                            length: 8,
                            msb_right: 0,
                        },
                        blue: FbBitfield {
                            offset: 0,
                            length: 8,
                            msb_right: 0,
                        },
                        transp: FbBitfield {
                            offset: 24,
                            length: 8,
                            msb_right: 0,
                        },
                        nonstd: 0,
                        activate: 0,
                        height: 0,
                        width: 0,
                        accel_flags: 0,
                        pixclock: 0,
                        left_margin: 0,
                        right_margin: 0,
                        upper_margin: 0,
                        lower_margin: 0,
                        hsync_len: 0,
                        vsync_len: 0,
                        sync: 0,
                        vmode: 0,
                        rotate: 0,
                        colorspace: 0,
                        reserved: [0; 4],
                    };
                    unsafe {
                        core::ptr::write(arg as *mut FbVarScreenInfo, var);
                    }
                }
                Ok(0)
            }
            0x4601 => {
                // FBIOPUT_VSCREENINFO
                Ok(0)
            }
            _ => Err(-22), // EINVAL
        }
    }
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
