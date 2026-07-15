//! Inode abstraction — the core file system object.
//!
//! In Unix, an inode represents a file system object (file, directory,
//! device, pipe, socket). The inode contains metadata about the object
//! and pointers to its data.

use crate::syscall::Errno;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

/// Types of file system objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// Regular file.
    Regular,
    /// Directory.
    Directory,
    /// Character device.
    CharDevice,
    /// Block device.
    BlockDevice,
    /// Named pipe (FIFO).
    Pipe,
    /// Symbolic link.
    Symlink,
    /// Unix domain socket.
    Socket,
}

/// File permissions (Unix mode bits).
#[derive(Debug, Clone, Copy)]
pub struct FilePermissions {
    /// The raw mode bits (e.g., 0o755).
    pub mode: u16,
}

impl FilePermissions {
    /// Create new permissions from a mode value.
    pub const fn new(mode: u16) -> Self {
        Self { mode }
    }

    /// Default directory permissions (rwxr-xr-x).
    pub const fn default_dir() -> Self {
        Self { mode: 0o755 }
    }

    /// Default file permissions (rw-r--r--).
    pub const fn default_file() -> Self {
        Self { mode: 0o644 }
    }

    /// Owner read permission.
    pub const fn owner_read(&self) -> bool {
        self.mode & 0o400 != 0
    }

    /// Owner write permission.
    pub const fn owner_write(&self) -> bool {
        self.mode & 0o200 != 0
    }

    /// Owner execute permission.
    pub const fn owner_exec(&self) -> bool {
        self.mode & 0o100 != 0
    }
}

impl Default for FilePermissions {
    fn default() -> Self {
        Self::default_file()
    }
}

/// Inode metadata — information about a file system object.
#[derive(Debug, Clone)]
pub struct Inode {
    /// Inode number (unique within a filesystem).
    pub ino: u64,
    /// Type of file system object.
    pub file_type: FileType,
    /// File permissions.
    pub permissions: FilePermissions,
    /// Number of hard links.
    pub nlink: u32,
    /// Owner user ID.
    pub uid: u32,
    /// Owner group ID.
    pub gid: u32,
    /// File size in bytes.
    pub size: u64,
    /// Number of 512-byte blocks allocated.
    pub blocks: u64,
    /// Last access time (Unix timestamp).
    pub atime: u64,
    /// Last modification time (Unix timestamp).
    pub mtime: u64,
    /// Last status change time (Unix timestamp).
    pub ctime: u64,
    /// Device ID (for device files).
    pub rdev: u64,
}

impl Inode {
    /// Create a new inode with default values.
    pub fn new(ino: u64, file_type: FileType) -> Self {
        let permissions = match file_type {
            FileType::Directory => FilePermissions::default_dir(),
            _ => FilePermissions::default_file(),
        };

        Self {
            ino,
            file_type,
            permissions,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: 0,
            blocks: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
            rdev: 0,
        }
    }

    /// Check if this inode is a directory.
    pub fn is_dir(&self) -> bool {
        self.file_type == FileType::Directory
    }

    /// Check if this inode is a regular file.
    pub fn is_file(&self) -> bool {
        self.file_type == FileType::Regular
    }
}

/// A directory entry.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Name of the entry.
    pub name: String,
    /// Inode number.
    pub ino: u64,
    /// File type.
    pub file_type: FileType,
}

pub const MAY_EXEC: u16 = 0o100;
pub const MAY_WRITE: u16 = 0o200;
pub const MAY_READ: u16 = 0o400;

pub const POLLIN: u32 = 0x0001;
pub const POLLOUT: u32 = 0x0004;
pub const POLLERR: u32 = 0x0008;
pub const POLLHUP: u32 = 0x0010;

/// Check permission logic.
pub fn check_permission(inode: &Inode, mask: u16) -> Result<(), Errno> {
    let (euid, egid) = if let Some(pid) = crate::process::scheduler::current_pid() {
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
            let task = task_arc.lock();
            (task.euid, task.egid)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    if euid == 0 {
        // Root Bypass
        if (mask & MAY_EXEC) != 0 && inode.file_type == FileType::Regular {
            if (inode.permissions.mode & 0o111) == 0 {
                return Err(Errno::EACCES);
            }
        }
        return Ok(());
    }

    if euid == inode.uid {
        if (inode.permissions.mode & mask) == mask {
            return Ok(());
        }
    } else if egid == inode.gid {
        if ((inode.permissions.mode << 3) & mask) == mask {
            return Ok(());
        }
    } else {
        if ((inode.permissions.mode << 6) & mask) == mask {
            return Ok(());
        }
    }

    Err(Errno::EACCES)
}

/// Trait for inode operations — implemented by each filesystem.
///
/// This is the primary interface between the VFS and filesystem drivers.
/// Each filesystem provides its own implementation of these operations.
pub trait InodeOps: Send + Sync {
    /// Get the inode metadata.
    fn inode(&self) -> &Inode;

    /// Set permissions.
    fn set_permissions(&self, _mode: u16) -> Result<(), i32> {
        Err(-1) // EPERM
    }

    /// Set owner and group.
    fn set_owner(&self, _uid: u32, _gid: u32) -> Result<(), i32> {
        Err(-1) // EPERM
    }

    /// Return the inner socket if this inode is a socket.
    fn as_socket(&self) -> Option<Arc<Mutex<crate::net::socket::Socket>>> {
        None
    }

    /// Look up a child by name (for directories).
    fn lookup(&self, _name: &str) -> Option<Arc<dyn InodeOps>> {
        None
    }

    /// Read data from this inode.
    fn read(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize, i32> {
        Err(-1) // EPERM
    }

    /// Read data directly from the storage backing (bypassing the page cache).
    fn read_direct(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        self.read(offset, buf)
    }

    /// Write data to this inode.
    fn write(&self, _offset: u64, _data: &[u8]) -> Result<usize, i32> {
        Err(-1) // EPERM
    }

    /// Write data directly to the storage backing (bypassing the page cache).
    fn write_direct(&self, offset: u64, data: &[u8]) -> Result<usize, i32> {
        self.write(offset, data)
    }

    /// Create a new file in this directory.
    fn create(&self, _name: &str, _file_type: FileType) -> Option<Arc<dyn InodeOps>> {
        None
    }

    /// Remove a file from this directory.
    fn unlink(&self, _name: &str) -> Result<(), i32> {
        Err(-1) // EPERM
    }

    /// Create a subdirectory.
    fn mkdir(&self, _name: &str) -> Option<Arc<dyn InodeOps>> {
        None
    }

    /// Remove a subdirectory.
    fn rmdir(&self, _name: &str) -> Result<(), i32> {
        Err(-1) // EPERM
    }

    /// List directory entries.
    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }

    /// Truncate the file to the given size.
    fn truncate(&self, _size: u64) -> Result<(), i32> {
        Err(-1) // EPERM
    }

    /// Device-specific I/O control.
    fn ioctl(&self, _request: u64, _arg: u64) -> Result<u64, i32> {
        Err(-22) // EINVAL
    }

    /// Set non-blocking state of the inode.
    fn set_nonblocking(&self, _nonblocking: bool) {}

    /// Poll for I/O readiness.
    fn poll(&self, _events: u32) -> u32 {
        0
    }

    /// Downcast helpers
    fn as_epoll(&self) -> Option<&crate::fs::epoll::EpollInstance> {
        None
    }
    fn as_timerfd(&self) -> Option<&crate::fs::timerfd::TimerFd> {
        None
    }
    fn as_signalfd(&self) -> Option<&crate::fs::signalfd::SignalFd> {
        None
    }
    fn as_eventfd(&self) -> Option<&crate::fs::eventfd::EventFd> {
        None
    }
}

/// Helper function to check if a given inode's open file description in the current task has O_NONBLOCK set.
pub fn is_inode_nonblocking(inode: &dyn InodeOps) -> bool {
    if let Some(pid) = crate::process::scheduler::current_pid() {
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
            let task = task_arc.lock();
            let fd_table = task.fd_table.lock();
            for slot in fd_table.entries.iter() {
                if let Some(desc) = slot {
                    let p1 = inode as *const dyn InodeOps as *const u8;
                    let p2 = desc.inode.as_ref() as *const dyn InodeOps as *const u8;
                    if p1 == p2 {
                        if (desc.flags.lock().0 & 0o4000) != 0 {
                            // O_NONBLOCK = 0o4000
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}
