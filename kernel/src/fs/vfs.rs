//! VFS core — mount table and filesystem dispatch.
//!
//! The VFS layer sits between the syscall interface and the actual
//! filesystem implementations, routing operations to the correct
//! filesystem driver based on the mount point.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::RwLock;
use crate::kprintln;

use super::inode::InodeOps;

/// The global VFS instance.
static VFS: RwLock<Option<Vfs>> = RwLock::new(None);

/// A registered filesystem type.
pub struct FileSystemType {
    /// Name of the filesystem (e.g., "tmpfs", "ext2", "devfs").
    pub name: String,
    /// Create a new instance of this filesystem.
    pub mount_fn: fn() -> Arc<dyn FileSystem>,
}

/// Trait for filesystem implementations.
///
/// Each filesystem (tmpfs, devfs, ext2, etc.) implements this trait
/// to provide file operations through a common interface.
pub trait FileSystem: Send + Sync {
    /// Get the root inode of this filesystem.
    fn root(&self) -> Option<Arc<dyn InodeOps>>;

    /// Get the filesystem name.
    fn name(&self) -> &str;

    /// Sync all dirty data to persistent storage.
    fn sync(&self) {}

    /// Get filesystem statistics.
    fn statfs(&self) -> FsStats {
        FsStats::default()
    }
}

/// Filesystem statistics (similar to POSIX `statvfs`).
#[derive(Debug, Clone, Default)]
pub struct FsStats {
    /// Total blocks in filesystem.
    pub total_blocks: u64,
    /// Free blocks.
    pub free_blocks: u64,
    /// Total inodes.
    pub total_inodes: u64,
    /// Free inodes.
    pub free_inodes: u64,
    /// Block size in bytes.
    pub block_size: u64,
    /// Maximum filename length.
    pub max_name_len: u64,
}

/// A mount point entry.
struct MountEntry {
    /// The path where this filesystem is mounted.
    path: String,
    /// The mounted filesystem.
    filesystem: Arc<dyn FileSystem>,
}

/// The Virtual File System manager.
pub struct Vfs {
    /// Mount table: maps mount paths to filesystems.
    mounts: BTreeMap<String, MountEntry>,
    /// Registered filesystem types.
    fs_types: Vec<FileSystemType>,
}

impl Vfs {
    /// Create a new VFS.
    fn new() -> Self {
        Self {
            mounts: BTreeMap::new(),
            fs_types: Vec::new(),
        }
    }

    /// Register a new filesystem type.
    pub fn register_fs_type(&mut self, fs_type: FileSystemType) {
        kprintln!("[vfs] Registered filesystem type: {}", fs_type.name);
        self.fs_types.push(fs_type);
    }

    /// Mount a filesystem at the given path.
    pub fn mount(&mut self, path: String, filesystem: Arc<dyn FileSystem>) {
        kprintln!("[vfs] Mounting {} at {}", filesystem.name(), path);
        self.mounts.insert(
            path.clone(),
            MountEntry {
                path,
                filesystem,
            },
        );
    }

    /// Unmount the filesystem at the given path.
    pub fn unmount(&mut self, path: &str) -> bool {
        if let Some(entry) = self.mounts.remove(path) {
            entry.filesystem.sync();
            kprintln!("[vfs] Unmounted {}", path);
            true
        } else {
            false
        }
    }

    /// Find the filesystem that handles the given path.
    ///
    /// Returns the filesystem and the remaining path within it.
    pub fn resolve_mount(&self, path: &str) -> Option<(Arc<dyn FileSystem>, String)> {
        // Find the longest matching mount point
        let mut best_match: Option<(&str, &MountEntry)> = None;

        for (mount_path, entry) in &self.mounts {
            if path.starts_with(mount_path.as_str()) {
                match best_match {
                    Some((best_path, _)) if mount_path.len() <= best_path.len() => {}
                    _ => best_match = Some((mount_path.as_str(), entry)),
                }
            }
        }

        best_match.map(|(mount_path, entry)| {
            let remaining = &path[mount_path.len()..];
            let remaining = if remaining.is_empty() { "/" } else { remaining };
            (entry.filesystem.clone(), String::from(remaining))
        })
    }

    /// Lookup an inode by path.
    pub fn lookup(&self, path: &str) -> Option<Arc<dyn InodeOps>> {
        let (fs, remaining_path) = self.resolve_mount(path)?;
        let root = fs.root()?;

        // Walk the path components
        let mut current = root;
        for component in remaining_path.split('/').filter(|c| !c.is_empty()) {
            current = current.lookup(component)?;
        }

        Some(current)
    }
}

/// Initialize the VFS.
pub fn init() {
    let vfs = Vfs::new();
    *VFS.write() = Some(vfs);
}

/// Mount a filesystem at the given path.
pub fn mount(path: String, filesystem: Arc<dyn FileSystem>) {
    if let Some(ref mut vfs) = *VFS.write() {
        vfs.mount(path, filesystem);
    }
}

/// Lookup an inode by path.
pub fn lookup(path: &str) -> Option<Arc<dyn InodeOps>> {
    VFS.read().as_ref()?.lookup(path)
}

/// Register a filesystem type.
pub fn register_fs_type(fs_type: FileSystemType) {
    if let Some(ref mut vfs) = *VFS.write() {
        vfs.register_fs_type(fs_type);
    }
}

/// Helper to resolve a user-supplied path to an absolute, normalized path
/// based on the current task's working directory.
pub fn resolve_relative_path(path: &str) -> String {
    if path.starts_with('/') {
        crate::fs::path::normalize(path)
    } else {
        // Retrieve current task's cwd
        let cwd = if let Some(pid) = crate::process::scheduler::current_pid() {
            let sched = crate::process::scheduler::SCHEDULER.lock();
            if let Some(ref s) = *sched {
                if let Some(task) = s.get_task(pid) {
                    task.cwd.clone()
                } else {
                    alloc::string::String::from("/")
                }
            } else {
                alloc::string::String::from("/")
            }
        } else {
            alloc::string::String::from("/")
        };
        crate::fs::path::normalize(&crate::fs::path::join(&cwd, path))
    }
}

