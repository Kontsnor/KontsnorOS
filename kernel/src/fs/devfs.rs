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

//! Device filesystem (devfs) — `/dev`.
//!
//! Provides device nodes that allow user-space programs to access
//! hardware devices through the standard file I/O interface.
//!
//! Standard device nodes:
//! - `/dev/null` — discards all writes, reads return EOF
//! - `/dev/zero` — reads return zero bytes
//! - `/dev/random` — reads return random bytes
//! - `/dev/console` — kernel console

use crate::drivers::traits::BlockDevice;
use crate::kprintln;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use spin::RwLock;

use super::inode::{DirEntry, FileType, Inode, InodeOps};
use super::vfs::FileSystem;

/// The global devfs instance.
static DEVFS: RwLock<Option<Arc<DevFs>>> = RwLock::new(None);
static PTS_DIR: RwLock<Option<Arc<DevFsDir>>> = RwLock::new(None);

/// The device filesystem.
pub struct DevFs {
    root: Arc<DevFsDir>,
}

impl FileSystem for DevFs {
    fn root(&self) -> Option<Arc<dyn InodeOps>> {
        Some(self.root.clone())
    }

    fn name(&self) -> &str {
        "devfs"
    }
}

/// A devfs directory node.
struct DevFsDir {
    inode: Inode,
    entries: RwLock<BTreeMap<String, Arc<dyn InodeOps>>>,
}

impl InodeOps for DevFsDir {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn lookup(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        self.entries.read().get(name).cloned()
    }

    fn readdir(&self) -> Vec<DirEntry> {
        let entries = self.entries.read();
        let mut result = vec![
            DirEntry {
                name: String::from("."),
                ino: self.inode.ino,
                file_type: FileType::Directory,
            },
            DirEntry {
                name: String::from(".."),
                ino: self.inode.ino,
                file_type: FileType::Directory,
            },
        ];

        for (name, node) in entries.iter() {
            result.push(DirEntry {
                name: name.clone(),
                ino: node.inode().ino,
                file_type: node.inode().file_type,
            });
        }

        result
    }
}

/// `/dev/null` — discards all writes, reads return EOF. (Major 1, Minor 3)
struct DevNull {
    inode: Inode,
}

impl InodeOps for DevNull {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize, i32> {
        Ok(0) // EOF
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        Ok(data.len()) // Discard all data
    }

    fn poll(&self, _events: u32) -> u32 {
        super::inode::POLLIN | super::inode::POLLOUT
    }
}

/// `/dev/zero` — reads return zero bytes. (Major 1, Minor 5)
struct DevZero {
    inode: Inode,
}

impl InodeOps for DevZero {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        for byte in buf.iter_mut() {
            *byte = 0;
        }
        Ok(buf.len())
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        Ok(data.len())
    }

    fn poll(&self, _events: u32) -> u32 {
        super::inode::POLLIN | super::inode::POLLOUT
    }
}

/// `/dev/full` — reads return zero bytes, writes fail with -ENOSPC. (Major 1, Minor 7)
struct DevFull {
    inode: Inode,
}

impl InodeOps for DevFull {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        for byte in buf.iter_mut() {
            *byte = 0;
        }
        Ok(buf.len())
    }

    fn write(&self, _offset: u64, _data: &[u8]) -> Result<usize, i32> {
        Err(-28) // -ENOSPC: No space left on device
    }

    fn poll(&self, _events: u32) -> u32 {
        super::inode::POLLIN | super::inode::POLLOUT
    }
}

/// Dummy /dev/ptmx node so lookup succeeds
struct DevPtmxDummy {
    inode: Inode,
}

impl InodeOps for DevPtmxDummy {
    fn inode(&self) -> &Inode {
        &self.inode
    }
}

/// `/dev/random` and `/dev/urandom` — reads return random bytes from the kernel CSPRNG.
/// Major 1, Minor 8 (/dev/random) and Minor 9 (/dev/urandom).
struct DevRandom {
    inode: Inode,
}

impl InodeOps for DevRandom {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if crate::crypto::prng::fill_bytes(buf) {
            Ok(buf.len())
        } else {
            Err(-11) // -EAGAIN
        }
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        if data.len() >= 32 {
            let mut entropy = [0u8; 32];
            entropy.copy_from_slice(&data[..32]);
            crate::crypto::prng::reseed(&entropy);
        } else if !data.is_empty() {
            let mut entropy = [0u8; 32];
            for (i, &b) in data.iter().enumerate() {
                entropy[i] = b;
            }
            crate::crypto::prng::reseed(&entropy);
        }
        Ok(data.len())
    }

    fn poll(&self, _events: u32) -> u32 {
        super::inode::POLLIN | super::inode::POLLOUT
    }
}

/// Linux framebuffer bitfield descriptor.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FbBitfield {
    pub offset: u32,
    pub length: u32,
    pub msb_right: u32,
}

/// Linux variable screen info structure (`struct fb_var_screeninfo`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FbVarScreeninfo {
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

/// Linux fixed screen info structure (`struct fb_fix_screeninfo`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FbFixScreeninfo {
    pub id: [u8; 16],
    pub smem_start: u64,
    pub smem_len: u32,
    pub type_: u32,
    pub type_aux: u32,
    pub visual: u32,
    pub xpanstep: u16,
    pub ypanstep: u16,
    pub ywrapstep: u16,
    pub _pad: u16,
    pub line_length: u32,
    pub mmio_start: u64,
    pub mmio_len: u32,
    pub accel: u32,
    pub capabilities: u16,
    pub reserved: [u16; 2],
}

const _: () = assert!(
    core::mem::size_of::<FbFixScreeninfo>() == 80,
    "FbFixScreeninfo must be 80 bytes to match Linux uapi on x86-64"
);

/// Linux colormap structure (`struct fb_cmap`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FbCmap {
    pub start: u32,
    pub len: u32,
    pub red: u64,
    pub green: u64,
    pub blue: u64,
    pub transp: u64,
}

/// `/dev/fb0` — Linux linear framebuffer character device (Major 29, Minor 0).
pub struct DevFb0 {
    pub inode: Inode,
}

impl InodeOps for DevFb0 {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let vram_virt = 0xffff_c000_0000_0000u64;
        let vram_size = 16 * 1024 * 1024;
        if offset >= vram_size {
            return Ok(0);
        }
        let to_read = (buf.len() as u64).min(vram_size - offset) as usize;
        // SAFETY: vram_virt is mapped by bochs::init() to the physical VRAM BAR with size 16MB.
        unsafe {
            let src = (vram_virt + offset) as *const u8;
            core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), to_read);
        }
        Ok(to_read)
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<usize, i32> {
        let vram_virt = 0xffff_c000_0000_0000u64;
        let vram_size = 16 * 1024 * 1024;
        if offset >= vram_size {
            return Ok(0);
        }
        let to_write = (data.len() as u64).min(vram_size - offset) as usize;
        // SAFETY: vram_virt is mapped by bochs::init() to the physical VRAM BAR with size 16MB.
        unsafe {
            let dst = (vram_virt + offset) as *mut u8;
            core::ptr::copy_nonoverlapping(data.as_ptr(), dst, to_write);
        }
        Ok(to_write)
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        const FBIOGET_VSCREENINFO: u64 = 0x4600;
        const FBIOPUT_VSCREENINFO: u64 = 0x4601;
        const FBIOGET_FSCREENINFO: u64 = 0x4602;
        const FBIOGETCMAP: u64 = 0x4604;
        const FBIOPUTCMAP: u64 = 0x4605;
        const FBIOPAN_DISPLAY: u64 = 0x4606;
        const FBIOGET_CON2FBMAP: u64 = 0x460F;
        const FBIOBLANK: u64 = 0x4611;
        const FBIO_WAITFORVSYNC: u64 = 0x4620;

        match request {
            FBIOGET_FSCREENINFO => {
                crate::syscall::validation::validate_user_ptr_write(
                    arg as *mut u8,
                    core::mem::size_of::<FbFixScreeninfo>(),
                )
                .map_err(|_| -14)?;

                let (w, _h, bpp) = crate::drivers::gpu::bochs::get_current_mode();
                let bytes_per_pixel = ((bpp + 7) / 8).max(1);
                let line_length = w * bytes_per_pixel;
                let smem_start = crate::drivers::gpu::bochs::get_lfb_phys();
                let smem_len = 16 * 1024 * 1024;

                let mut id = [0u8; 16];
                let name = b"bochsfbi";
                id[..name.len()].copy_from_slice(name);

                let visual = if bpp == 8 {
                    3 // FB_VISUAL_PSEUDOCOLOR
                } else {
                    2 // FB_VISUAL_TRUECOLOR
                };

                let fix = FbFixScreeninfo {
                    id,
                    smem_start,
                    smem_len,
                    type_: 0, // FB_TYPE_PACKED_PIXELS
                    type_aux: 0,
                    visual,
                    xpanstep: 0,
                    ypanstep: 0,
                    ywrapstep: 0,
                    _pad: 0,
                    line_length,
                    mmio_start: 0,
                    mmio_len: 0,
                    accel: 0,
                    capabilities: 0,
                    reserved: [0; 2],
                };

                // SAFETY: Pointer is validated above with validate_user_ptr_write.
                unsafe {
                    *(arg as *mut FbFixScreeninfo) = fix;
                }
                Ok(0)
            }
            FBIOGET_VSCREENINFO => {
                crate::syscall::validation::validate_user_ptr_write(
                    arg as *mut u8,
                    core::mem::size_of::<FbVarScreeninfo>(),
                )
                .map_err(|_| -14)?;

                let (w, h, bpp) = crate::drivers::gpu::bochs::get_current_mode();
                let var = FbVarScreeninfo {
                    xres: w,
                    yres: h,
                    xres_virtual: w,
                    yres_virtual: h,
                    xoffset: 0,
                    yoffset: 0,
                    bits_per_pixel: bpp,
                    grayscale: 0,
                    red: if bpp == 32 {
                        FbBitfield {
                            offset: 16,
                            length: 8,
                            msb_right: 0,
                        }
                    } else {
                        FbBitfield {
                            offset: 0,
                            length: 8,
                            msb_right: 0,
                        }
                    },
                    green: if bpp == 32 {
                        FbBitfield {
                            offset: 8,
                            length: 8,
                            msb_right: 0,
                        }
                    } else {
                        FbBitfield {
                            offset: 0,
                            length: 8,
                            msb_right: 0,
                        }
                    },
                    blue: if bpp == 32 {
                        FbBitfield {
                            offset: 0,
                            length: 8,
                            msb_right: 0,
                        }
                    } else {
                        FbBitfield {
                            offset: 0,
                            length: 8,
                            msb_right: 0,
                        }
                    },
                    transp: if bpp == 32 {
                        FbBitfield {
                            offset: 24,
                            length: 8,
                            msb_right: 0,
                        }
                    } else {
                        FbBitfield {
                            offset: 0,
                            length: 0,
                            msb_right: 0,
                        }
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

                // SAFETY: Pointer is validated above with validate_user_ptr_write.
                unsafe {
                    *(arg as *mut FbVarScreeninfo) = var;
                }
                Ok(0)
            }
            FBIOPUT_VSCREENINFO => {
                crate::syscall::validation::validate_user_ptr_write(
                    arg as *mut u8,
                    core::mem::size_of::<FbVarScreeninfo>(),
                )
                .map_err(|_| -14)?;

                // SAFETY: Pointer is validated above with validate_user_ptr_write.
                let mut var = unsafe { *(arg as *const FbVarScreeninfo) };
                let width = var.xres as u16;
                let height = var.yres as u16;
                let bpp = var.bits_per_pixel as u16;

                if width > 0 && height > 0 && (bpp == 8 || bpp == 16 || bpp == 24 || bpp == 32) {
                    let _ = crate::drivers::gpu::bochs::set_video_mode(width, height, bpp);
                }

                let (cur_w, cur_h, cur_bpp) = crate::drivers::gpu::bochs::get_current_mode();
                var.xres = cur_w;
                var.yres = cur_h;
                var.xres_virtual = cur_w;
                var.yres_virtual = cur_h;
                var.bits_per_pixel = cur_bpp;
                if cur_bpp == 32 {
                    var.red = FbBitfield {
                        offset: 16,
                        length: 8,
                        msb_right: 0,
                    };
                    var.green = FbBitfield {
                        offset: 8,
                        length: 8,
                        msb_right: 0,
                    };
                    var.blue = FbBitfield {
                        offset: 0,
                        length: 8,
                        msb_right: 0,
                    };
                    var.transp = FbBitfield {
                        offset: 24,
                        length: 8,
                        msb_right: 0,
                    };
                }

                // SAFETY: Pointer is validated above with validate_user_ptr_write.
                unsafe {
                    *(arg as *mut FbVarScreeninfo) = var;
                }
                Ok(0)
            }
            FBIOPAN_DISPLAY => {
                crate::syscall::validation::validate_user_ptr_write(
                    arg as *mut u8,
                    core::mem::size_of::<FbVarScreeninfo>(),
                )
                .map_err(|_| -14)?;
                Ok(0)
            }
            FBIO_WAITFORVSYNC => Ok(0),
            FBIOGET_CON2FBMAP => Ok(0),
            FBIOGETCMAP | FBIOPUTCMAP => {
                if arg != 0 {
                    if !crate::syscall::validation::validate_user_ptr(
                        arg as *const u8,
                        core::mem::size_of::<FbCmap>(),
                    ) {
                        return Err(-14); // -EFAULT
                    }
                }
                Ok(0)
            }
            FBIOBLANK => Ok(0),
            _ => Err(-25), // ENOTTY
        }
    }

    fn poll(&self, _events: u32) -> u32 {
        super::inode::POLLIN | super::inode::POLLOUT
    }
}

/// Linux input_event structure, exactly 24 bytes on x86-64.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InputEvent {
    pub tv_sec: u64,  // seconds
    pub tv_usec: u64, // microseconds
    pub type_: u16,   // EV_SYN=0, EV_KEY=1, EV_REL=2
    pub code: u16,    // key code or axis code
    pub value: i32,   // 1=down, 0=up, signed delta for EV_REL
}

const _: () = assert!(
    core::mem::size_of::<InputEvent>() == 24,
    "InputEvent must be 24 bytes to match Linux uapi on x86-64"
);

/// Linux input_id structure (8 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InputId {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

const _: () = assert!(
    core::mem::size_of::<InputId>() == 8,
    "InputId must be 8 bytes to match Linux uapi"
);

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_REL: u16 = 2;
pub const SYN_REPORT: u16 = 0;

pub const REL_X: usize = 0;
pub const REL_Y: usize = 1;

pub const BTN_LEFT: usize = 0x110;
pub const BTN_RIGHT: usize = 0x111;
pub const BTN_MIDDLE: usize = 0x112;

/// Set bit `bit` in a byte slice bitmask (Linux evdev bit-field format).
fn set_bit(mask: &mut [u8], bit: usize) {
    if bit / 8 < mask.len() {
        mask[bit / 8] |= 1 << (bit % 8);
    }
}

/// Minimal ASCII to Linux keycode mapping (PS/2 Set 1 subset).
fn ascii_to_keycode(ascii: u8) -> u16 {
    match ascii {
        b'a'..=b'z' => 30 + (ascii - b'a') as u16,
        b'A'..=b'Z' => 30 + (ascii - b'A') as u16,
        b'1'..=b'9' => 2 + (ascii - b'1') as u16,
        b'0' => 11,
        b'\n' => 28,   // KEY_ENTER
        b'\x1b' => 1,  // KEY_ESC
        b'\x08' => 14, // KEY_BACKSPACE
        b'\t' => 15,   // KEY_TAB
        b' ' => 57,    // KEY_SPACE
        b'-' => 12,    // KEY_MINUS
        b'=' => 13,    // KEY_EQUAL
        _ => 0,
    }
}

/// Reverse mapping from keycode to ASCII byte (if applicable).
fn keycode_to_ascii(code: u16) -> Option<u8> {
    match code {
        30..=55 => Some(b'a' + (code - 30) as u8),
        2..=10 => Some(b'1' + (code - 2) as u8),
        11 => Some(b'0'),
        28 => Some(b'\n'),
        1 => Some(b'\x1b'),
        14 => Some(b'\x08'),
        15 => Some(b'\t'),
        57 => Some(b' '),
        12 => Some(b'-'),
        13 => Some(b'='),
        _ => None,
    }
}

/// Generic handler for evdev capability ioctls on keyboard and mouse devices.
fn handle_evdev_ioctl(
    request: u64,
    arg: u64,
    name: &str,
    phys: &str,
    is_mouse: bool,
) -> Result<u64, i32> {
    let req32 = request as u32;
    let base_with_dir = req32 & !0x3FFF_0000;
    let base_raw = req32 & 0xFFFF;
    let len = ((req32 >> 16) & 0x3FFF) as usize;

    if req32 == 0x4501 || req32 == 0x8004_4501 {
        // EVIOCGVERSION
        crate::syscall::validation::validate_user_ptr_write(
            arg as *mut u8,
            core::mem::size_of::<i32>(),
        )
        .map_err(|_| -14i32)?;
        // SAFETY: arg is validated to be writable for 4 bytes.
        unsafe {
            *(arg as *mut i32) = 0x0001_0001i32;
        }
        return Ok(0);
    }

    if req32 == 0x4502 || req32 == 0x8008_4502 {
        // EVIOCGID
        crate::syscall::validation::validate_user_ptr_write(
            arg as *mut u8,
            core::mem::size_of::<InputId>(),
        )
        .map_err(|_| -14i32)?;
        // SAFETY: arg is validated to be writable for 8 bytes.
        unsafe {
            *(arg as *mut InputId) = InputId {
                bustype: 0x11, // BUS_I8042
                vendor: 0,
                product: 0,
                version: 1,
            };
        }
        return Ok(0);
    }

    if base_with_dir == 0x8000_4506 || base_raw == 0x4506 {
        // EVIOCGNAME(len)
        let max_len = if len > 0 { len } else { 256 };
        let bytes = name.as_bytes();
        let copy_len = core::cmp::min(bytes.len(), max_len.saturating_sub(1));
        crate::syscall::validation::validate_user_ptr_write(arg as *mut u8, copy_len + 1)
            .map_err(|_| -14i32)?;
        // SAFETY: Destination pointer is validated for copy_len + 1 bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), arg as *mut u8, copy_len);
            *(arg.wrapping_add(copy_len as u64) as *mut u8) = 0;
        }
        return Ok(copy_len as u64);
    }

    if base_with_dir == 0x8000_4508 || base_raw == 0x4508 {
        // EVIOCGPHYS(len)
        let max_len = if len > 0 { len } else { 256 };
        let bytes = phys.as_bytes();
        let copy_len = core::cmp::min(bytes.len(), max_len.saturating_sub(1));
        crate::syscall::validation::validate_user_ptr_write(arg as *mut u8, copy_len + 1)
            .map_err(|_| -14i32)?;
        // SAFETY: Destination pointer is validated for copy_len + 1 bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), arg as *mut u8, copy_len);
            *(arg.wrapping_add(copy_len as u64) as *mut u8) = 0;
        }
        return Ok(copy_len as u64);
    }

    if base_with_dir == 0x8000_4509 || base_raw == 0x4509 {
        // EVIOCGUNIQ(len)
        let max_len = if len > 0 { len } else { 256 };
        if max_len > 0 {
            crate::syscall::validation::validate_user_ptr_write(arg as *mut u8, 1)
                .map_err(|_| -14i32)?;
            // SAFETY: Destination pointer is validated for 1 byte.
            unsafe {
                *(arg as *mut u8) = 0;
            }
        }
        return Ok(0);
    }

    // EVIOCGBIT queries: 0x8000_4520 + ev_type
    if (base_with_dir >= 0x8000_4520 && base_with_dir <= 0x8000_453F)
        || (base_raw >= 0x4520 && base_raw <= 0x453F)
    {
        let ev_type = if base_with_dir >= 0x8000_4520 && base_with_dir <= 0x8000_453F {
            base_with_dir - 0x8000_4520
        } else {
            base_raw - 0x4520
        };

        if len == 0 {
            return Ok(0);
        }

        crate::syscall::validation::validate_user_ptr_write(arg as *mut u8, len)
            .map_err(|_| -14i32)?;

        let mut mask = [0u8; 128];
        let slice = if len <= mask.len() {
            &mut mask[..len]
        } else {
            // SAFETY: arg is validated for len bytes.
            unsafe {
                core::ptr::write_bytes(arg as *mut u8, 0, len);
            }
            &mut mask[..]
        };

        match ev_type {
            0 => {
                // EV_SYN (0), EV_KEY (1)
                set_bit(slice, EV_SYN as usize);
                set_bit(slice, EV_KEY as usize);
                if is_mouse {
                    set_bit(slice, EV_REL as usize);
                }
            }
            1 => {
                // EV_KEY
                if is_mouse {
                    set_bit(slice, BTN_LEFT);
                    set_bit(slice, BTN_RIGHT);
                    set_bit(slice, BTN_MIDDLE);
                } else {
                    for k in 30..=55 {
                        set_bit(slice, k); // KEY_A..KEY_Z
                    }
                    for k in 2..=11 {
                        set_bit(slice, k); // KEY_1..KEY_0
                    }
                    set_bit(slice, 28); // KEY_ENTER
                    set_bit(slice, 1); // KEY_ESC
                    set_bit(slice, 14); // KEY_BACKSPACE
                    set_bit(slice, 15); // KEY_TAB
                    set_bit(slice, 57); // KEY_SPACE
                    set_bit(slice, 12); // KEY_MINUS
                    set_bit(slice, 13); // KEY_EQUAL
                }
            }
            2 => {
                // EV_REL
                if is_mouse {
                    set_bit(slice, REL_X);
                    set_bit(slice, REL_Y);
                }
            }
            _ => {}
        }

        let to_copy = core::cmp::min(len, slice.len());
        // SAFETY: Destination pointer is validated for len bytes, slice contains to_copy bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(slice.as_ptr(), arg as *mut u8, to_copy);
        }
        return Ok(len as u64);
    }

    Err(-25) // ENOTTY
}

/// `/dev/input/event0` — evdev keyboard node (Major 13, Minor 64).
pub struct DevInputKeyboard {
    pub inode: Inode,
    pub pending: crate::sync::spinlock::TicketLock<Option<([InputEvent; 4], usize)>>,
}

impl InodeOps for DevInputKeyboard {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn wait_queue(&self) -> Option<Arc<crate::sync::wait_queue::WaitQueue>> {
        Some(crate::drivers::keyboard::stdin_wait_queue())
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        const SZ: usize = core::mem::size_of::<InputEvent>();
        if buf.len() < SZ {
            return Err(-22); // EINVAL: buffer too small
        }

        let mut pending_lock = self.pending.lock();
        if let Some((events, mut idx)) = *pending_lock {
            let count_available = 4 - idx;
            let count_can_fit = buf.len() / SZ;
            let count_to_copy = core::cmp::min(count_available, count_can_fit);
            let bytes_to_copy = count_to_copy * SZ;

            // SAFETY: `events[idx..]` contains valid InputEvent structs and `buf` has space for `bytes_to_copy`.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &events[idx] as *const InputEvent as *const u8,
                    buf.as_mut_ptr(),
                    bytes_to_copy,
                );
            }

            idx += count_to_copy;
            if idx >= 4 {
                *pending_lock = None;
            } else {
                *pending_lock = Some((events, idx));
            }
            return Ok(bytes_to_copy);
        }

        match crate::drivers::keyboard::try_read_char() {
            Some(ascii) => {
                let keycode = ascii_to_keycode(ascii);
                let events = [
                    InputEvent {
                        tv_sec: 0,
                        tv_usec: 0,
                        type_: EV_KEY,
                        code: keycode,
                        value: 1, // key-down
                    },
                    InputEvent {
                        tv_sec: 0,
                        tv_usec: 0,
                        type_: EV_SYN,
                        code: SYN_REPORT,
                        value: 0,
                    },
                    InputEvent {
                        tv_sec: 0,
                        tv_usec: 0,
                        type_: EV_KEY,
                        code: keycode,
                        value: 0, // key-up
                    },
                    InputEvent {
                        tv_sec: 0,
                        tv_usec: 0,
                        type_: EV_SYN,
                        code: SYN_REPORT,
                        value: 0,
                    },
                ];

                if buf.len() >= 4 * SZ {
                    // SAFETY: `events` has 4 InputEvent structs and buf.len() >= 4 * SZ.
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            events.as_ptr() as *const u8,
                            buf.as_mut_ptr(),
                            4 * SZ,
                        );
                    }
                    Ok(4 * SZ)
                } else {
                    let count_can_fit = buf.len() / SZ;
                    let bytes_to_copy = count_can_fit * SZ;
                    // SAFETY: `events` has 4 elements, count_can_fit >= 1, buf has capacity.
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            events.as_ptr() as *const u8,
                            buf.as_mut_ptr(),
                            bytes_to_copy,
                        );
                    }
                    *pending_lock = Some((events, count_can_fit));
                    Ok(bytes_to_copy)
                }
            }
            None => Err(-11), // EAGAIN
        }
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        handle_evdev_ioctl(
            request,
            arg,
            "KontsnorOS PS/2 Keyboard",
            "isa0060/serio0",
            false,
        )
    }

    fn poll(&self, _events: u32) -> u32 {
        if self.pending.lock().is_some() || crate::drivers::keyboard::has_input() {
            super::inode::POLLIN
        } else {
            0
        }
    }
}

/// `/dev/input/mice` — PS/2 mouse aggregator node (Major 13, Minor 63).
pub struct DevInputMice {
    pub inode: Inode,
}

impl InodeOps for DevInputMice {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn wait_queue(&self) -> Option<Arc<crate::sync::wait_queue::WaitQueue>> {
        Some(crate::drivers::ps2_mouse::mouse_wait_queue())
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.len() < 3 {
            return Err(-22); // EINVAL: buffer too small
        }
        match crate::drivers::ps2_mouse::read_mice_packet(buf) {
            Some(n) => Ok(n),
            None => Err(-11), // EAGAIN
        }
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        handle_evdev_ioctl(
            request,
            arg,
            "KontsnorOS PS/2 Mouse",
            "isa0060/serio1",
            true,
        )
    }

    fn poll(&self, _events: u32) -> u32 {
        if crate::drivers::ps2_mouse::has_data() {
            super::inode::POLLIN
        } else {
            0
        }
    }
}

/// `/dev/input/uinput` — virtual evdev injector node (Major 10, Minor 223).
pub struct DevUinput {
    pub inode: Inode,
}

impl InodeOps for DevUinput {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn ioctl(&self, request: u64, _arg: u64) -> Result<u64, i32> {
        let req32 = request as u32;
        match req32 {
            // UI_DEV_CREATE (0x5501)
            0x5501 => Ok(0),
            // UI_DEV_DESTROY (0x5502)
            0x5502 => Ok(0),
            // UI_SET_EVBIT (0x4004_5564 or 0x5564)
            0x4004_5564 | 0x5564 => Ok(0),
            // UI_SET_KEYBIT (0x4004_5565 or 0x5565)
            0x4004_5565 | 0x5565 => Ok(0),
            // UI_SET_RELBIT (0x4004_5566 or 0x5566)
            0x4004_5566 | 0x5566 => Ok(0),
            _ => {
                let base = req32 & 0xFFFF;
                if base == 0x5501 || base == 0x5502 || (base >= 0x5564 && base <= 0x5566) {
                    Ok(0)
                } else {
                    Err(-25) // ENOTTY
                }
            }
        }
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> Result<usize, i32> {
        const SZ: usize = core::mem::size_of::<InputEvent>();
        if buf.len() < SZ {
            return Ok(0);
        }

        let mut dx = 0i8;
        let mut dy = 0i8;
        let mut has_rel = false;
        let num_events = buf.len() / SZ;

        for i in 0..num_events {
            let offset = i * SZ;
            let mut ev = InputEvent::default();
            // SAFETY: `buf` has at least `offset + SZ` bytes, and `InputEvent` is repr(C).
            unsafe {
                core::ptr::copy_nonoverlapping(
                    buf.as_ptr().add(offset),
                    &mut ev as *mut InputEvent as *mut u8,
                    SZ,
                );
            }

            match ev.type_ {
                EV_KEY => {
                    if ev.code == 0x110 {
                        crate::drivers::ps2_mouse::set_button_state(0, ev.value != 0);
                    } else if ev.code == 0x111 {
                        crate::drivers::ps2_mouse::set_button_state(1, ev.value != 0);
                    } else if ev.code == 0x112 {
                        crate::drivers::ps2_mouse::set_button_state(2, ev.value != 0);
                    } else if ev.value == 1 {
                        if let Some(ascii) = keycode_to_ascii(ev.code) {
                            crate::drivers::keyboard::push_char(ascii);
                        }
                    }
                }
                EV_REL => {
                    if ev.code == 0 {
                        dx = dx.saturating_add(ev.value as i8);
                        has_rel = true;
                    } else if ev.code == 1 {
                        dy = dy.saturating_add(ev.value as i8);
                        has_rel = true;
                    }
                }
                EV_SYN => {
                    if has_rel {
                        crate::drivers::ps2_mouse::push_synthesised_packet(dx, dy);
                        dx = 0;
                        dy = 0;
                        has_rel = false;
                    }
                }
                _ => {}
            }
        }

        if has_rel {
            crate::drivers::ps2_mouse::push_synthesised_packet(dx, dy);
        }

        Ok(num_events * SZ)
    }

    fn poll(&self, _events: u32) -> u32 {
        super::inode::POLLOUT
    }
}

/// Helper to create a character device inode with Major/Minor numbers and 0666 permissions.
fn make_chardev_inode(ino: u64, major: u64, minor: u64) -> Inode {
    let mut inode = Inode::new(ino, FileType::CharDevice).with_dev(DEVFS_DEV_ID);
    inode.rdev = (major << 8) | (minor & 0xff);
    inode.permissions = super::inode::FilePermissions::new(0o666);
    inode
}

/// Filesystem device ID for devfs.
pub const DEVFS_DEV_ID: u64 = 3;

/// Create a new devfs instance.
pub fn create_devfs() -> Arc<DevFs> {
    let mut entries = BTreeMap::new();

    // Create standard device nodes
    // /dev/null (Major 1, Minor 3)
    entries.insert(
        String::from("null"),
        Arc::new(DevNull {
            inode: make_chardev_inode(2, 1, 3),
        }) as Arc<dyn InodeOps>,
    );

    // /dev/zero (Major 1, Minor 5)
    entries.insert(
        String::from("zero"),
        Arc::new(DevZero {
            inode: make_chardev_inode(3, 1, 5),
        }) as Arc<dyn InodeOps>,
    );

    // /dev/full (Major 1, Minor 7)
    entries.insert(
        String::from("full"),
        Arc::new(DevFull {
            inode: make_chardev_inode(4, 1, 7),
        }) as Arc<dyn InodeOps>,
    );

    // /dev/random (Major 1, Minor 8)
    entries.insert(
        String::from("random"),
        Arc::new(DevRandom {
            inode: make_chardev_inode(16, 1, 8),
        }) as Arc<dyn InodeOps>,
    );

    // /dev/urandom (Major 1, Minor 9)
    entries.insert(
        String::from("urandom"),
        Arc::new(DevRandom {
            inode: make_chardev_inode(17, 1, 9),
        }) as Arc<dyn InodeOps>,
    );

    // Create /dev/pts directory
    let pts = Arc::new(DevFsDir {
        inode: Inode::new(14, FileType::Directory).with_dev(DEVFS_DEV_ID),
        entries: RwLock::new(BTreeMap::new()),
    });
    *PTS_DIR.write() = Some(pts.clone());

    entries.insert(String::from("pts"), pts as Arc<dyn InodeOps>);

    // Create /dev/ptmx dummy device
    entries.insert(
        String::from("ptmx"),
        Arc::new(DevPtmxDummy {
            inode: Inode::new(15, FileType::CharDevice).with_dev(DEVFS_DEV_ID),
        }) as Arc<dyn InodeOps>,
    );

    // Create /dev/fb0 linear framebuffer device (Major 29, Minor 0)
    entries.insert(
        String::from("fb0"),
        Arc::new(DevFb0 {
            inode: make_chardev_inode(18, 29, 0),
        }) as Arc<dyn InodeOps>,
    );

    // Create /dev/input directory with event0, mice, and uinput
    let mut input_entries = BTreeMap::new();
    input_entries.insert(
        String::from("event0"),
        Arc::new(DevInputKeyboard {
            inode: make_chardev_inode(50, 13, 64),
            pending: crate::sync::spinlock::TicketLock::new(None),
        }) as Arc<dyn InodeOps>,
    );
    input_entries.insert(
        String::from("mice"),
        Arc::new(DevInputMice {
            inode: make_chardev_inode(51, 13, 63),
        }) as Arc<dyn InodeOps>,
    );
    input_entries.insert(
        String::from("uinput"),
        Arc::new(DevUinput {
            inode: make_chardev_inode(52, 10, 223),
        }) as Arc<dyn InodeOps>,
    );
    let input_dir = Arc::new(DevFsDir {
        inode: Inode::new(30, FileType::Directory).with_dev(DEVFS_DEV_ID),
        entries: RwLock::new(input_entries),
    });
    entries.insert(String::from("input"), input_dir as Arc<dyn InodeOps>);

    let root = Arc::new(DevFsDir {
        inode: Inode::new(1, FileType::Directory).with_dev(DEVFS_DEV_ID),
        entries: RwLock::new(entries),
    });

    let devfs = Arc::new(DevFs { root });
    *DEVFS.write() = Some(devfs.clone());

    devfs
}

/// Initialize devfs and register standard device nodes.
pub fn init() {
    let devfs = create_devfs();

    // Mount at /dev
    super::vfs::mount(String::from("/dev"), devfs);

    // Register TTY character devices: stdin, stdout, stderr, tty
    register_device("stdin", super::tty::make_stdin());
    register_device("stdout", super::tty::make_stdout());
    register_device("stderr", super::tty::make_stderr());
    register_device("tty", super::tty::make_tty());
}

/// Register a new device node in devfs.
pub fn register_device(name: &str, device: Arc<dyn InodeOps>) {
    if let Some(ref devfs) = *DEVFS.read() {
        devfs
            .root
            .entries
            .write()
            .insert(String::from(name), device);
        kprintln!("[devfs] Registered device: /dev/{}", name);
    }
}

/// Register a new device node in /dev/pts.
pub fn register_pts_device(name: String, device: Arc<dyn InodeOps>) {
    if let Some(ref pts) = *PTS_DIR.read() {
        pts.entries.write().insert(name.clone(), device);
        kprintln!("[devfs] Registered pts device: /dev/pts/{}", name);
    }
}

static NEXT_DEVFS_INO: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(100);

/// A block device node in devfs, translating byte-stream file operations
/// to block-aligned reads/writes on an underlying `BlockDevice`.
pub struct BlockDevNode {
    inode: Inode,
    device: Arc<dyn BlockDevice>,
}

impl BlockDevNode {
    pub fn new(device: Arc<dyn BlockDevice>, major: u64, minor: u64) -> Self {
        let ino = NEXT_DEVFS_INO.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let mut inode = Inode::new(ino, FileType::BlockDevice).with_dev(DEVFS_DEV_ID);
        inode.rdev = (major << 8) | (minor & 0xff);
        inode.permissions = super::inode::FilePermissions::new(0o660);
        inode.size = device.block_count().saturating_mul(device.block_size());
        inode.blocks = device.block_count();
        Self { inode, device }
    }
}

impl InodeOps for BlockDevNode {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let block_size = self.device.block_size();
        if block_size == 0 || buf.is_empty() {
            return Ok(0);
        }

        let total_size = self.device.block_count().saturating_mul(block_size);
        if offset >= total_size {
            return Ok(0);
        }

        let to_read = (buf.len() as u64).min(total_size - offset) as usize;
        let mut bytes_read = 0;
        let mut temp_block = alloc::vec![0u8; block_size as usize];

        while bytes_read < to_read {
            let curr_offset = offset + bytes_read as u64;
            let block_idx = curr_offset / block_size;
            let block_offset = (curr_offset % block_size) as usize;
            let chunk = (to_read - bytes_read).min(block_size as usize - block_offset);

            if block_offset == 0 && chunk == block_size as usize {
                if self
                    .device
                    .read_block(block_idx, &mut buf[bytes_read..bytes_read + chunk])
                    .is_err()
                {
                    return if bytes_read > 0 {
                        Ok(bytes_read)
                    } else {
                        Err(-5)
                    };
                }
            } else {
                if self.device.read_block(block_idx, &mut temp_block).is_err() {
                    return if bytes_read > 0 {
                        Ok(bytes_read)
                    } else {
                        Err(-5)
                    };
                }
                buf[bytes_read..bytes_read + chunk]
                    .copy_from_slice(&temp_block[block_offset..block_offset + chunk]);
            }

            bytes_read += chunk;
        }

        Ok(bytes_read)
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<usize, i32> {
        let block_size = self.device.block_size();
        if block_size == 0 || data.is_empty() {
            return Ok(0);
        }

        let total_size = self.device.block_count().saturating_mul(block_size);
        if offset >= total_size {
            return Err(-28); // ENOSPC
        }

        let to_write = (data.len() as u64).min(total_size - offset) as usize;
        let mut bytes_written = 0;
        let mut temp_block = alloc::vec![0u8; block_size as usize];

        while bytes_written < to_write {
            let curr_offset = offset + bytes_written as u64;
            let block_idx = curr_offset / block_size;
            let block_offset = (curr_offset % block_size) as usize;
            let chunk = (to_write - bytes_written).min(block_size as usize - block_offset);

            if block_offset == 0 && chunk == block_size as usize {
                if self
                    .device
                    .write_block(block_idx, &data[bytes_written..bytes_written + chunk])
                    .is_err()
                {
                    return if bytes_written > 0 {
                        Ok(bytes_written)
                    } else {
                        Err(-5)
                    };
                }
            } else {
                if self.device.read_block(block_idx, &mut temp_block).is_err() {
                    return if bytes_written > 0 {
                        Ok(bytes_written)
                    } else {
                        Err(-5)
                    };
                }
                temp_block[block_offset..block_offset + chunk]
                    .copy_from_slice(&data[bytes_written..bytes_written + chunk]);
                if self.device.write_block(block_idx, &temp_block).is_err() {
                    return if bytes_written > 0 {
                        Ok(bytes_written)
                    } else {
                        Err(-5)
                    };
                }
            }

            bytes_written += chunk;
        }

        Ok(bytes_written)
    }

    fn ioctl(&self, _request: u64, _arg: u64) -> Result<u64, i32> {
        Ok(0)
    }
}

/// Register a block device in devfs.
pub fn register_block_device_node(name: &str, device: Arc<dyn BlockDevice>) {
    let node = Arc::new(BlockDevNode::new(device, 259, 0));
    register_device(name, node);
}
