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
