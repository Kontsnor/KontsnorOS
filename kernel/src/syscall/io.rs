//! I/O control syscalls — ioctl, pipe, dup, dup2.

use super::{Errno, SyscallResult};
use crate::process::fd as proc_fd;

/// `ioctl(fd, request, ...)` — Device-specific I/O control.
///
/// This is the catch-all for device-specific operations that don't
/// fit into the standard read/write model.
pub fn sys_ioctl(fd: i32, request: u64, arg: u64) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }

    let inode = match proc_fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };

    match inode.ioctl(request, arg) {
        Ok(res) => res as SyscallResult,
        Err(e) => e as SyscallResult,
    }
}
