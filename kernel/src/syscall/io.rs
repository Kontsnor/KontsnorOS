//! I/O control syscalls — ioctl, pipe, dup, dup2.

use super::{Errno, SyscallResult};
use crate::kprintln;
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

/// `pipe(pipefd)` — Create a unidirectional data channel.
///
/// Creates a pipe and places two file descriptors in `pipefd`:
/// - pipefd[0]: read end
/// - pipefd[1]: write end
pub fn sys_pipe(_pipefd: *mut [i32; 2]) -> SyscallResult {
    // TODO: Create pipe buffer
    // TODO: Allocate two file descriptors
    // TODO: Link them to the pipe
    kprintln!("[syscall] pipe()");
    Errno::ENOSYS.into()
}

/// `dup(oldfd)` — Duplicate a file descriptor.
pub fn sys_dup(oldfd: i32) -> SyscallResult {
    if oldfd < 0 {
        return Errno::EBADF.into();
    }

    // TODO: Find lowest available fd, duplicate oldfd to it
    kprintln!("[syscall] dup(fd={})", oldfd);
    Errno::ENOSYS.into()
}

/// `dup2(oldfd, newfd)` — Duplicate a file descriptor to a specific number.
pub fn sys_dup2(oldfd: i32, newfd: i32) -> SyscallResult {
    if oldfd < 0 || newfd < 0 {
        return Errno::EBADF.into();
    }

    // TODO: Close newfd if open, then duplicate oldfd to newfd
    kprintln!("[syscall] dup2(oldfd={}, newfd={})", oldfd, newfd);
    Errno::ENOSYS.into()
}
