//! VFS core — mount table and filesystem dispatch.
//!
//! The VFS layer sits between the syscall interface and the actual
//! filesystem implementations, routing operations to the correct
//! filesystem driver based on the mount point.

use crate::kprintln;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::RwLock;

use super::inode::{FileType, InodeOps};
use crate::drivers::traits::BlockDevice;
use crate::syscall::Errno;

/// The global VFS instance.
static VFS: RwLock<Option<Vfs>> = RwLock::new(None);

/// VFS drive map (global block devices registry).
pub static BLOCK_DEVICES: RwLock<BTreeMap<String, Arc<dyn BlockDevice>>> =
    RwLock::new(BTreeMap::new());

/// Register a block device in the VFS drive map.
pub fn register_block_device(name: String, device: Arc<dyn BlockDevice>) {
    kprintln!("[vfs] Registered block device: {}", name);
    BLOCK_DEVICES.write().insert(name, device);
}

/// Retrieve a block device by name from the VFS drive map.
pub fn get_block_device(name: &str) -> Option<Arc<dyn BlockDevice>> {
    BLOCK_DEVICES.read().get(name).cloned()
}

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
        FsStats {
            total_blocks: 1024 * 1024, // 4GB with 4KB block size
            free_blocks: 512 * 1024,
            total_inodes: 1024 * 1024,
            free_inodes: 512 * 1024,
            block_size: 4096,
            max_name_len: 255,
        }
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
    /// Dentry cache mapping absolute path strings to target inodes.
    dentry_cache: RwLock<BTreeMap<String, Arc<dyn InodeOps>>>,
}

impl Vfs {
    /// Create a new VFS.
    fn new() -> Self {
        Self {
            mounts: BTreeMap::new(),
            fs_types: Vec::new(),
            dentry_cache: RwLock::new(BTreeMap::new()),
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
        self.mounts
            .insert(path.clone(), MountEntry { path, filesystem });
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
        self.lookup_follow(path, true)
    }

    /// Lookup an inode by path, optionally following symlinks.
    pub fn lookup_follow(&self, path: &str, follow_last: bool) -> Option<Arc<dyn InodeOps>> {
        let mut resolved_path = resolve_relative_path(path);
        let mut symlink_count = 0;

        loop {
            let (fs, remaining_path) = self.resolve_mount(&resolved_path)?;
            let root = fs.root()?;

            let mut current = root;
            let components: Vec<&str> = remaining_path
                .split('/')
                .filter(|c| !c.is_empty())
                .collect();

            let mount_path = if remaining_path == "/" {
                resolved_path.as_str()
            } else {
                &resolved_path[..resolved_path.len() - remaining_path.len()]
            };
            let mut resolved_till_now = String::from(mount_path);

            let n_comp = components.len();
            let mut i = 0;
            let mut symlink_target = None;

            for component in components {
                // Verify execute permission on the directory component before traversing/looking up the next one
                if let Err(_) =
                    crate::fs::inode::check_permission(current.inode(), crate::fs::inode::MAY_EXEC)
                {
                    return None;
                }

                i += 1;
                let path_key = if resolved_till_now.is_empty() || resolved_till_now == "/" {
                    format!("/{}", component)
                } else {
                    format!("{}/{}", resolved_till_now, component)
                };

                let next = {
                    let cache = self.dentry_cache.read();
                    cache.get(&path_key).cloned()
                };

                let next = if let Some(n) = next {
                    n
                } else {
                    let n = current.lookup(component)?;
                    let mut cache = self.dentry_cache.write();
                    cache.insert(path_key.clone(), n.clone());
                    n
                };

                // Check if this component is a symlink
                if next.inode().file_type == FileType::Symlink {
                    let is_last = i == n_comp;
                    if !is_last || follow_last {
                        // Read symlink target
                        let mut target_buf = alloc::vec![0u8; 4096];
                        if let Ok(n) = next.read(0, &mut target_buf) {
                            if let Ok(target_str) = core::str::from_utf8(&target_buf[..n]) {
                                symlink_target =
                                    Some((resolved_till_now.clone(), String::from(target_str)));
                                break;
                            }
                        }
                        return None; // failed to read/parse symlink
                    }
                }

                current = next;
                resolved_till_now = path_key;
            }

            if let Some((dir_path, target)) = symlink_target {
                symlink_count += 1;
                if symlink_count > 20 {
                    kprintln!("[vfs] symlink loop limit exceeded");
                    return None;
                }

                if target.starts_with('/') {
                    resolved_path = crate::fs::path::normalize(&target);
                } else {
                    resolved_path =
                        crate::fs::path::normalize(&crate::fs::path::join(&dir_path, &target));
                }
                continue;
            }

            return Some(current);
        }
    }

    /// Invalidate a dentry and all its descendants.
    pub fn invalidate_dentry(&self, path: &str) {
        let mut cache = self.dentry_cache.write();
        cache.remove(path);
        let prefix = if path.ends_with('/') {
            String::from(path)
        } else {
            format!("{}/", path)
        };
        cache.retain(|k, _| !k.starts_with(&prefix));
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

/// Lookup an inode by path, optionally following symlinks.
pub fn lookup_follow(path: &str, follow_last: bool) -> Option<Arc<dyn InodeOps>> {
    VFS.read().as_ref()?.lookup_follow(path, follow_last)
}

/// Find the filesystem that handles the given path.
pub fn resolve_mount(path: &str) -> Option<(Arc<dyn FileSystem>, String)> {
    VFS.read().as_ref()?.resolve_mount(path)
}

/// Register a filesystem type.
pub fn register_fs_type(fs_type: FileSystemType) {
    if let Some(ref mut vfs) = *VFS.write() {
        vfs.register_fs_type(fs_type);
    }
}

/// Invalidate a directory entry in the cache.
pub fn invalidate_dentry(path: &str) {
    if let Some(ref vfs) = *VFS.read() {
        vfs.invalidate_dentry(path);
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
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
                task_arc.lock().cwd.clone()
            } else {
                alloc::string::String::from("/")
            }
        } else {
            alloc::string::String::from("/")
        };
        crate::fs::path::normalize(&crate::fs::path::join(&cwd, path))
    }
}

/// Helper to resolve paths relative to a directory file descriptor.
pub fn resolve_relative_path_at(dfd: i32, path: &str) -> Result<String, Errno> {
    if path.starts_with('/') {
        return Ok(crate::fs::path::normalize(path));
    }
    if dfd == -100 {
        // AT_FDCWD
        return Ok(resolve_relative_path(path));
    }

    let desc = crate::process::fd::current_task_get_file_desc(dfd).ok_or(Errno::EBADF)?;

    if desc.inode.inode().file_type != FileType::Directory {
        return Err(Errno::ENOTDIR);
    }

    let desc_path = desc.path.as_deref().unwrap_or("/");
    Ok(crate::fs::path::normalize(&crate::fs::path::join(
        desc_path, path,
    )))
}
