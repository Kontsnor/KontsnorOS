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

//! Open, openat, and close system calls.

use super::super::{Errno, SyscallResult};
use crate::process::fd as proc_fd;
use crate::syscall::validation::copy_string_from_user;
use alloc::string::String;

/// `open(pathname, flags, mode)` — Open a file.
///
/// Resolves `pathname` through the VFS, allocates a file descriptor in the
/// current task's `fd_table`, and returns the new fd number.
pub fn sys_open(pathname: *const u8, flags: i32, mode: u32) -> SyscallResult {
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    sys_open_with_resolved_path(resolved_path, flags, mode)
}

/// Core open logic with an already resolved path.
pub fn sys_open_with_resolved_path(resolved_path: String, flags: i32, _mode: u32) -> SyscallResult {
    // kprintln!("[syscall] open(\"{}\", flags={:#x})", resolved_path, flags);

    let flags_u32 = flags as u32;

    let inode = if resolved_path == "/dev/ptmx" {
        match crate::fs::pty::allocate_new_pty() {
            Ok(master_inode) => master_inode,
            Err(e) => return e as SyscallResult,
        }
    } else {
        let follow_last = (flags_u32 & 0x20000) == 0; // AT_SYMLINK_NOFOLLOW/O_NOFOLLOW
        let exists = crate::fs::vfs::lookup_follow(&resolved_path, follow_last);
        match exists {
            Some(i) => {
                if !follow_last && i.inode().file_type == crate::fs::inode::FileType::Symlink {
                    return Errno::ELOOP.into();
                }
                // If O_CREAT and O_EXCL are both set, return EEXIST
                if (flags_u32 & crate::fs::file::OpenFlags::O_CREAT != 0)
                    && (flags_u32 & crate::fs::file::OpenFlags::O_EXCL != 0)
                {
                    return Errno::EEXIST.into();
                }
                // If O_DIRECTORY is set and it is not a directory, return ENOTDIR
                if (flags_u32 & crate::fs::file::OpenFlags::O_DIRECTORY != 0) && !i.inode().is_dir()
                {
                    return Errno::ENOTDIR.into();
                }
                // If opened for writing and the inode is a directory, return EISDIR
                if i.inode().is_dir() && crate::fs::file::OpenFlags(flags_u32).is_writable() {
                    return Errno::EISDIR.into();
                }

                // Check permissions on the existing file
                let open_flags = crate::fs::file::OpenFlags(flags_u32);
                if open_flags.is_readable() {
                    if let Err(e) =
                        crate::fs::inode::check_permission(i.inode(), crate::fs::inode::MAY_READ)
                    {
                        return e as SyscallResult;
                    }
                }
                if open_flags.is_writable() {
                    if let Err(e) =
                        crate::fs::inode::check_permission(i.inode(), crate::fs::inode::MAY_WRITE)
                    {
                        return e as SyscallResult;
                    }
                }

                // If O_TRUNC is set and it is a regular file, truncate it to 0 size
                if (flags_u32 & crate::fs::file::OpenFlags::O_TRUNC != 0) && i.inode().is_file() {
                    if let Err(e) = i.truncate(0) {
                        return e as SyscallResult;
                    }
                }
                i
            }
            None => {
                if flags_u32 & crate::fs::file::OpenFlags::O_CREAT != 0 {
                    // Split path to find parent directory
                    let (parent_path, name) = crate::fs::path::split_path(&resolved_path);
                    let parent_inode = match crate::fs::vfs::lookup(parent_path) {
                        Some(i) => i,
                        None => return Errno::ENOENT.into(),
                    };
                    if !parent_inode.inode().is_dir() {
                        return Errno::ENOTDIR.into();
                    }

                    // Verify write and execute permissions on the parent directory
                    if let Err(e) = crate::fs::inode::check_permission(
                        parent_inode.inode(),
                        crate::fs::inode::MAY_WRITE,
                    ) {
                        return e as SyscallResult;
                    }
                    if let Err(e) = crate::fs::inode::check_permission(
                        parent_inode.inode(),
                        crate::fs::inode::MAY_EXEC,
                    ) {
                        return e as SyscallResult;
                    }

                    let umask = if let Some(pid) = crate::process::scheduler::current_pid() {
                        if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
                            task_arc.lock().umask
                        } else {
                            0o022
                        }
                    } else {
                        0o022
                    };
                    let file_mode = ((_mode & 0x0FFF) & !umask) as u16;

                    match parent_inode.create(name, crate::fs::inode::FileType::Regular) {
                        Some(new_i) => {
                            let _ = new_i.set_permissions(file_mode);
                            new_i
                        }
                        None => return Errno::EACCES.into(),
                    }
                } else {
                    return Errno::ENOENT.into();
                }
            }
        }
    };

    match proc_fd::current_task_alloc_fd_with_flags_and_path(
        inode,
        crate::fs::file::OpenFlags(flags_u32),
        Some(resolved_path),
    ) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}

/// `openat(dfd, pathname, flags, mode)` — Open file relative to directory file descriptor.
pub fn sys_openat(dfd: i32, pathname: *const u8, flags: i32, mode: u32) -> SyscallResult {
    if pathname.is_null() {
        return Errno::EFAULT.into();
    }
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_path = match crate::fs::vfs::resolve_relative_path_at(dfd, &raw_path) {
        Ok(path) => path,
        Err(e) => return e.into(),
    };

    sys_open_with_resolved_path(resolved_path, flags, mode)
}

/// `close(fd)` — Close a file descriptor.
pub fn sys_close(fd: i32) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }

    // Retrieve PID and Inode number prior to close to clean up fcntl locks
    let lock_cleanup_info = if let Some(desc) = proc_fd::current_task_get_file_desc(fd) {
        let current_pid = crate::process::scheduler::current_pid()
            .map(|p| p.as_u64())
            .unwrap_or(0);
        let ino = desc.inode.inode().ino;
        Some((current_pid, ino))
    } else {
        None
    };

    let is_pipe = proc_fd::current_task_read_fd(fd)
        .map(|i| i.inode().file_type == crate::fs::inode::FileType::Pipe)
        .unwrap_or(false);
    if is_pipe {
        let pid_str = crate::process::scheduler::current_pid()
            .map(|p| p.as_u64())
            .unwrap_or(0);
        crate::kprintln!("[syscall pid={}] sys_close on pipe fd {}", pid_str, fd);
    }
    if proc_fd::current_task_close_fd(fd) {
        if let Some((pid, ino)) = lock_cleanup_info {
            crate::syscall::fs::io::release_fcntl_locks_for_pid_and_ino(pid, ino);
        }
        0
    } else {
        Errno::EBADF.into()
    }
}

/// `truncate(pathname, length)` — Truncate a file to a specified length.
pub fn sys_truncate(pathname: *const u8, length: i64) -> SyscallResult {
    if pathname.is_null() {
        return Errno::EFAULT.into();
    }
    if length < 0 {
        return Errno::EINVAL.into();
    }
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    let inode = match crate::fs::vfs::lookup_follow(&resolved_path, true) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    let file_type = inode.inode().file_type;
    if file_type == crate::fs::inode::FileType::Directory {
        return Errno::EISDIR.into();
    }
    if file_type != crate::fs::inode::FileType::Regular {
        return Errno::EINVAL.into();
    }

    // Check write permissions on the file
    if let Err(e) = crate::fs::inode::check_permission(inode.inode(), crate::fs::inode::MAY_WRITE) {
        return e as SyscallResult;
    }

    match inode.truncate(length as u64) {
        Ok(()) => 0,
        Err(e) => e as i64,
    }
}
