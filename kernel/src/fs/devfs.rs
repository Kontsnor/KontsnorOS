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
    pub line_length: u32,
    pub mmio_start: u64,
    pub mmio_len: u32,
    pub accel: u32,
    pub capabilities: u16,
    pub reserved: [u16; 2],
}

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
        const FBIOBLANK: u64 = 0x4611;

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
            _ => Err(-22), // EINVAL
        }
    }

    fn poll(&self, _events: u32) -> u32 {
        super::inode::POLLIN | super::inode::POLLOUT
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
