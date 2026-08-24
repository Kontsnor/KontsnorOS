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

//! I/O control syscalls — ioctl, pipe, dup, dup2.

use super::{Errno, SyscallResult};
use crate::process::fd as proc_fd;

/// `ioctl(fd, request, ...)` — Device-specific I/O control.
///
/// This is the catch-all for device-specific operations that don't
/// fit into the standard read/write model.
pub fn sys_ioctl(fd: i32, request: u64, arg: u64) -> SyscallResult {
    crate::kprintln!(
        "[syscall] ioctl(fd={}, request={:#x}, arg={:#x})",
        fd,
        request,
        arg
    );
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
