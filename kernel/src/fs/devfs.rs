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

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use spin::RwLock;
use crate::kprintln;

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

/// `/dev/null` — discards all writes, reads return EOF.
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
}

/// `/dev/zero` — reads return zero bytes.
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

/// Initialize devfs and register standard device nodes.
pub fn init() {
    let mut entries = BTreeMap::new();

    // Create standard device nodes
    entries.insert(
        String::from("null"),
        Arc::new(DevNull {
            inode: Inode::new(2, FileType::CharDevice),
        }) as Arc<dyn InodeOps>,
    );

    entries.insert(
        String::from("zero"),
        Arc::new(DevZero {
            inode: Inode::new(3, FileType::CharDevice),
        }) as Arc<dyn InodeOps>,
    );

    // Create /dev/pts directory
    let pts = Arc::new(DevFsDir {
        inode: Inode::new(14, FileType::Directory),
        entries: RwLock::new(BTreeMap::new()),
    });
    *PTS_DIR.write() = Some(pts.clone());

    entries.insert(
        String::from("pts"),
        pts as Arc<dyn InodeOps>,
    );

    // Create /dev/ptmx dummy device
    entries.insert(
        String::from("ptmx"),
        Arc::new(DevPtmxDummy {
            inode: Inode::new(15, FileType::CharDevice),
        }) as Arc<dyn InodeOps>,
    );

    let root = Arc::new(DevFsDir {
        inode: Inode::new(1, FileType::Directory),
        entries: RwLock::new(entries),
    });

    let devfs = Arc::new(DevFs { root });

    // Mount at /dev
    super::vfs::mount(String::from("/dev"), devfs.clone());
    *DEVFS.write() = Some(devfs);

    // Register TTY character devices: stdin, stdout, stderr, tty
    register_device("stdin",  super::tty::make_stdin());
    register_device("stdout", super::tty::make_stdout());
    register_device("stderr", super::tty::make_stderr());
    register_device("tty",    super::tty::make_tty());
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
