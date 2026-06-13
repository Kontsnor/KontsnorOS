//! Security filesystem (securityfs) — `/sys/kernel/security`.
//!
//! Provides dummy configurations and security nodes to satisfy userspace checks
//! from SELinux, AppArmor, or other security suites.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::inode::{DirEntry, FileType, Inode, InodeOps};
use super::vfs::FileSystem;

/// The securityfs filesystem.
pub struct SecurityFs {
    root: Arc<SecurityFsDir>,
}

impl FileSystem for SecurityFs {
    fn root(&self) -> Option<Arc<dyn InodeOps>> {
        Some(self.root.clone())
    }

    fn name(&self) -> &str {
        "securityfs"
    }
}

/// A securityfs directory node.
struct SecurityFsDir {
    inode: Inode,
    entries: Vec<(String, Arc<dyn InodeOps>)>,
}

impl InodeOps for SecurityFsDir {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn lookup(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        self.entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, node)| node.clone())
    }

    fn readdir(&self) -> Vec<DirEntry> {
        let mut result = vec![
            DirEntry {
                name: String::from("."),
                ino: self.inode.ino,
                file_type: FileType::Directory,
            },
            DirEntry {
                name: String::from(".."),
                ino: 1,
                file_type: FileType::Directory,
            },
        ];

        for (name, node) in &self.entries {
            result.push(DirEntry {
                name: name.clone(),
                ino: node.inode().ino,
                file_type: node.inode().file_type,
            });
        }

        result
    }
}

/// A securityfs control file node.
struct SecurityFsFile {
    inode: Inode,
    generator: fn() -> String,
}

impl InodeOps for SecurityFsFile {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let content = (self.generator)();
        let bytes = content.as_bytes();
        let offset = offset as usize;

        if offset >= bytes.len() {
            return Ok(0);
        }

        let available = bytes.len() - offset;
        let to_read = buf.len().min(available);
        buf[..to_read].copy_from_slice(&bytes[offset..offset + to_read]);

        Ok(to_read)
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        Ok(data.len())
    }
}

fn gen_apparmor_profiles() -> String {
    String::from("\n")
}

fn gen_apparmor_revision() -> String {
    String::from("0\n")
}

/// Create a new instance of SecurityFs.
pub fn create_securityfs() -> Arc<SecurityFs> {
    let mut root_inode = Inode::new(500, FileType::Directory);
    root_inode.permissions.mode = 0o755;

    let mut profiles_inode = Inode::new(502, FileType::Regular);
    profiles_inode.permissions.mode = 0o644;

    let mut revision_inode = Inode::new(503, FileType::Regular);
    revision_inode.permissions.mode = 0o644;

    let mut features_inode = Inode::new(504, FileType::Directory);
    features_inode.permissions.mode = 0o755;

    let features_dir = Arc::new(SecurityFsDir {
        inode: features_inode,
        entries: Vec::new(),
    });

    let apparmor_entries = vec![
        (
            String::from("profiles"),
            Arc::new(SecurityFsFile {
                inode: profiles_inode,
                generator: gen_apparmor_profiles,
            }) as Arc<dyn InodeOps>,
        ),
        (
            String::from("revision"),
            Arc::new(SecurityFsFile {
                inode: revision_inode,
                generator: gen_apparmor_revision,
            }) as Arc<dyn InodeOps>,
        ),
        (String::from("features"), features_dir as Arc<dyn InodeOps>),
    ];

    let mut apparmor_inode = Inode::new(501, FileType::Directory);
    apparmor_inode.permissions.mode = 0o755;

    let apparmor_dir = Arc::new(SecurityFsDir {
        inode: apparmor_inode,
        entries: apparmor_entries,
    });

    let entries = vec![(String::from("apparmor"), apparmor_dir as Arc<dyn InodeOps>)];

    let root = Arc::new(SecurityFsDir {
        inode: root_inode,
        entries,
    });

    Arc::new(SecurityFs { root })
}
