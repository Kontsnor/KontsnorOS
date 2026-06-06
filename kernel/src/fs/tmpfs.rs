//! Temporary in-memory filesystem (tmpfs).
//!
//! A RAM-based filesystem that provides fast file storage without
//! requiring any block device. Data is lost on reboot.
//!
//! Used for:
//! - `/tmp` — temporary files
//! - Early boot before real filesystems are available

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use spin::RwLock;

use core::sync::atomic::{AtomicU64, Ordering};

use super::inode::{DirEntry, FileType, Inode, InodeOps};
use super::vfs::FileSystem;

/// Counter for generating unique inode numbers.
static NEXT_INO: AtomicU64 = AtomicU64::new(100);

fn alloc_ino() -> u64 {
    NEXT_INO.fetch_add(1, Ordering::Relaxed)
}

/// The tmpfs filesystem.
pub struct TmpFs {
    root: Arc<TmpFsDir>,
}

impl FileSystem for TmpFs {
    fn root(&self) -> Option<Arc<dyn InodeOps>> {
        Some(self.root.clone())
    }

    fn name(&self) -> &str {
        "tmpfs"
    }
}

/// A tmpfs directory.
pub struct TmpFsDir {
    inode: Inode,
    entries: RwLock<BTreeMap<String, Arc<dyn InodeOps>>>,
}

impl InodeOps for TmpFsDir {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn lookup(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        self.entries.read().get(name).cloned()
    }

    fn create(&self, name: &str, file_type: FileType) -> Option<Arc<dyn InodeOps>> {
        let node: Arc<dyn InodeOps> = match file_type {
            FileType::Regular => Arc::new(TmpFsFile {
                inode: RwLock::new(Inode::new(alloc_ino(), FileType::Regular)),
                data: RwLock::new(Vec::new()),
            }),
            FileType::Directory => Arc::new(TmpFsDir {
                inode: Inode::new(alloc_ino(), FileType::Directory),
                entries: RwLock::new(BTreeMap::new()),
            }),
            _ => return None,
        };

        self.entries
            .write()
            .insert(String::from(name), node.clone());
        Some(node)
    }

    fn mkdir(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        self.create(name, FileType::Directory)
    }

    fn unlink(&self, name: &str) -> Result<(), i32> {
        match self.entries.write().remove(name) {
            Some(_) => Ok(()),
            None => Err(-2), // ENOENT
        }
    }

    fn rmdir(&self, name: &str) -> Result<(), i32> {
        let entries = self.entries.read();
        if let Some(entry) = entries.get(name) {
            if !entry.inode().is_dir() {
                return Err(-20); // ENOTDIR
            }
            if !entry.readdir().is_empty() {
                // Check for entries beyond . and ..
                let child_entries = entry.readdir();
                let real_entries: Vec<_> = child_entries
                    .iter()
                    .filter(|e| e.name != "." && e.name != "..")
                    .collect();
                if !real_entries.is_empty() {
                    return Err(-39); // ENOTEMPTY
                }
            }
        } else {
            return Err(-2); // ENOENT
        }
        drop(entries);

        self.entries.write().remove(name);
        Ok(())
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

/// A tmpfs regular file — stores data in a `Vec<u8>`.
struct TmpFsFile {
    inode: RwLock<Inode>,
    data: RwLock<Vec<u8>>,
}

impl InodeOps for TmpFsFile {
    fn inode(&self) -> &Inode {
        // Note: This returns a reference to the RwLock guard, which
        // requires some careful handling. For now, we use a simple approach.
        // In a production kernel, we'd use a different pattern.
        unsafe {
            // SAFETY: We hold the lock briefly to get the reference.
            // This is a simplification; a real implementation would
            // use interior mutability differently.
            &*(&*self.inode.read() as *const Inode)
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let data = self.data.read();
        let offset = offset as usize;

        if offset >= data.len() {
            return Ok(0); // EOF
        }

        let available = data.len() - offset;
        let to_read = buf.len().min(available);
        buf[..to_read].copy_from_slice(&data[offset..offset + to_read]);

        Ok(to_read)
    }

    fn write(&self, offset: u64, new_data: &[u8]) -> Result<usize, i32> {
        let mut data = self.data.write();
        let offset = offset as usize;

        // Extend the file if needed
        if offset + new_data.len() > data.len() {
            data.resize(offset + new_data.len(), 0);
        }

        data[offset..offset + new_data.len()].copy_from_slice(new_data);

        // Update inode size
        let mut inode = self.inode.write();
        inode.size = data.len() as u64;

        Ok(new_data.len())
    }

    fn truncate(&self, size: u64) -> Result<(), i32> {
        let mut data = self.data.write();
        data.resize(size as usize, 0);
        self.inode.write().size = size;
        Ok(())
    }
}

/// Initialize tmpfs and mount at `/tmp`.
pub fn init() {
    let root = Arc::new(TmpFsDir {
        inode: Inode::new(alloc_ino(), FileType::Directory),
        entries: RwLock::new(BTreeMap::new()),
    });

    let tmpfs = Arc::new(TmpFs { root });
    super::vfs::mount(String::from("/tmp"), tmpfs);
}
