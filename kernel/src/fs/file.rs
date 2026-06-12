//! File descriptor table and file operations.
//!
//! Each process has a file descriptor table that maps integer file
//! descriptors to open file descriptions. This implements the Unix
//! file descriptor model.

use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use super::inode::InodeOps;

/// Maximum number of open file descriptors per process.
pub const MAX_FDS: usize = 256;

/// Flags for opening files (POSIX O_* flags).
#[derive(Debug, Clone, Copy)]
pub struct OpenFlags(pub u32);

impl OpenFlags {
    /// Open for reading only.
    pub const O_RDONLY: u32 = 0;
    /// Open for writing only.
    pub const O_WRONLY: u32 = 1;
    /// Open for reading and writing.
    pub const O_RDWR: u32 = 2;
    /// Create file if it doesn't exist.
    pub const O_CREAT: u32 = 0o100;
    /// Fail if O_CREAT and file exists.
    pub const O_EXCL: u32 = 0o200;
    /// Truncate file to zero length.
    pub const O_TRUNC: u32 = 0o1000;
    /// Append to end of file.
    pub const O_APPEND: u32 = 0o2000;
    /// Non-blocking I/O.
    pub const O_NONBLOCK: u32 = 0o4000;
    /// Directory.
    pub const O_DIRECTORY: u32 = 0o200000;
    /// Close on execve.
    pub const O_CLOEXEC: u32 = 0x80000;

    /// Check if the file is opened for reading.
    pub fn is_readable(self) -> bool {
        let access = self.0 & 3;
        access == Self::O_RDONLY || access == Self::O_RDWR
    }

    /// Check if the file is opened for writing.
    pub fn is_writable(self) -> bool {
        let access = self.0 & 3;
        access == Self::O_WRONLY || access == Self::O_RDWR
    }
}

/// An open file description.
///
/// Multiple file descriptors can refer to the same file description
/// (e.g., after `dup()` or `fork()`).
pub struct FileDescription {
    /// The underlying inode.
    pub inode: Arc<dyn InodeOps>,
    /// Current read/write offset.
    pub offset: Mutex<u64>,
    /// Open flags.
    pub flags: Mutex<OpenFlags>,
    /// Reference count (how many fds point here).
    pub ref_count: Mutex<u32>,
}

impl FileDescription {
    /// Create a new file description.
    pub fn new(inode: Arc<dyn InodeOps>, flags: OpenFlags) -> Self {
        Self {
            inode,
            offset: Mutex::new(0),
            flags: Mutex::new(flags),
            ref_count: Mutex::new(1),
        }
    }

    /// Read from this file.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, i32> {
        let flags = self.flags.lock();
        if !flags.is_readable() {
            return Err(-9); // EBADF
        }

        let mut offset = self.offset.lock();
        let bytes_read = self.inode.read(*offset, buf)?;
        *offset += bytes_read as u64;
        Ok(bytes_read)
    }

    /// Write to this file.
    pub fn write(&self, data: &[u8]) -> Result<usize, i32> {
        let flags = self.flags.lock();
        if !flags.is_writable() {
            return Err(-9); // EBADF
        }

        let mut offset = self.offset.lock();

        // Handle O_APPEND
        if flags.0 & OpenFlags::O_APPEND != 0 {
            *offset = self.inode.inode().size;
        }

        let bytes_written = self.inode.write(*offset, data)?;
        *offset += bytes_written as u64;
        Ok(bytes_written)
    }

    /// Seek to a new offset.
    pub fn seek(&self, offset: i64, whence: i32) -> Result<u64, i32> {
        let mut current = self.offset.lock();
        let new_offset = match whence {
            0 => {
                // SEEK_SET
                if offset < 0 {
                    return Err(-22); // EINVAL
                }
                offset as u64
            }
            1 => {
                // SEEK_CUR
                let result = *current as i64 + offset;
                if result < 0 {
                    return Err(-22); // EINVAL
                }
                result as u64
            }
            2 => {
                // SEEK_END
                let size = self.inode.inode().size as i64;
                let result = size + offset;
                if result < 0 {
                    return Err(-22); // EINVAL
                }
                result as u64
            }
            _ => return Err(-22), // EINVAL
        };

        *current = new_offset;
        Ok(new_offset)
    }
}

impl Drop for FileDescription {
    fn drop(&mut self) {
        if self.inode.inode().file_type == crate::fs::inode::FileType::Regular {
            let _ = crate::memory::page_cache::flush_all_for_inode(&self.inode);
        }
    }
}

/// Per-process file descriptor table.
pub struct FdTable {
    /// Array of file descriptors. `None` means the fd is available.
    fds: Vec<Option<Arc<FileDescription>>>,
}

impl FdTable {
    /// Create a new, empty file descriptor table.
    pub fn new() -> Self {
        let mut fds = Vec::with_capacity(MAX_FDS);
        fds.resize_with(MAX_FDS, || None);
        Self { fds }
    }

    /// Allocate the lowest available file descriptor.
    pub fn alloc(&mut self, desc: Arc<FileDescription>) -> Option<i32> {
        for (i, slot) in self.fds.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(desc);
                return Some(i as i32);
            }
        }
        None // Too many open files
    }

    /// Get the file description for a file descriptor.
    pub fn get(&self, fd: i32) -> Option<&Arc<FileDescription>> {
        if fd < 0 || fd as usize >= self.fds.len() {
            return None;
        }
        self.fds[fd as usize].as_ref()
    }

    /// Close a file descriptor.
    pub fn close(&mut self, fd: i32) -> bool {
        if fd < 0 || fd as usize >= self.fds.len() {
            return false;
        }
        self.fds[fd as usize].take().is_some()
    }

    /// Duplicate a file descriptor to the lowest available slot.
    pub fn dup(&mut self, oldfd: i32) -> Option<i32> {
        let desc = self.get(oldfd)?.clone();
        *desc.ref_count.lock() += 1;
        self.alloc(desc)
    }

    /// Duplicate a file descriptor to a specific slot.
    pub fn dup2(&mut self, oldfd: i32, newfd: i32) -> Option<i32> {
        if newfd < 0 || newfd as usize >= self.fds.len() {
            return None;
        }

        // Close newfd if it's open
        self.close(newfd);

        let desc = self.get(oldfd)?.clone();
        *desc.ref_count.lock() += 1;
        self.fds[newfd as usize] = Some(desc);
        Some(newfd)
    }
}

impl Default for FdTable {
    fn default() -> Self {
        Self::new()
    }
}
