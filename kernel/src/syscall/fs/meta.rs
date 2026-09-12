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

//! File metadata and directory system calls.

use super::super::{Errno, SyscallResult};
use crate::fs::inode::{check_permission, FileType, MAY_EXEC, MAY_READ, MAY_WRITE};
use crate::kprintln;
use crate::process::fd as proc_fd;
use crate::syscall::validation::{
    copy_string_from_user, validate_user_ptr, validate_user_ptr_write,
};
use alloc::string::String;
use alloc::sync::Arc;

#[repr(C)]
struct LinuxDirent64 {
    d_ino: u64,
    d_off: i64,
    d_reclen: u16,
    d_type: u8,
}

/// `getdents64(fd, dirp, count)` — Get directory entries.
pub fn sys_getdents64(fd: i32, dirp: *mut u8, count: usize) -> SyscallResult {
    if fd < 0 || dirp.is_null() || count == 0 {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(dirp as *const u8, count) {
        return Errno::EFAULT.into();
    }

    let inode = match proc_fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };

    if !inode.inode().is_dir() {
        return Errno::ENOTDIR.into();
    }

    let entries = inode.readdir();
    let mut current_idx = proc_fd::get_fd_offset(fd).unwrap_or(0) as usize;
    let mut bytes_written = 0;

    while current_idx < entries.len() {
        let entry = &entries[current_idx];
        let name_bytes = entry.name.as_bytes();
        let name_len = name_bytes.len();

        // 19 bytes before name (8 + 8 + 2 + 1), align up to 8
        let reclen = (19 + name_len + 1 + 7) & !7;

        if bytes_written + reclen > count {
            if bytes_written == 0 {
                return Errno::EINVAL.into();
            }
            break;
        }

        let dest_ptr = unsafe { dirp.add(bytes_written) };

        let d_type = match entry.file_type {
            FileType::Directory => 4,
            FileType::Regular => 8,
            FileType::CharDevice => 2,
            FileType::BlockDevice => 6,
            FileType::Pipe => 1,
            FileType::Socket => 12,
            FileType::Symlink => 10,
        };

        let header = LinuxDirent64 {
            d_ino: entry.ino,
            d_off: (current_idx + 1) as i64,
            d_reclen: reclen as u16,
            d_type,
        };

        unsafe {
            core::ptr::write(dest_ptr as *mut LinuxDirent64, header);
            let name_dest = dest_ptr.add(19);
            core::ptr::copy_nonoverlapping(name_bytes.as_ptr(), name_dest, name_len);
            *name_dest.add(name_len) = 0;
        }

        bytes_written += reclen;
        current_idx += 1;
    }

    proc_fd::set_fd_offset(fd, current_idx as u64);
    bytes_written as SyscallResult
}

/// `chdir(pathname)` — Change working directory.
pub fn sys_chdir(pathname: *const u8) -> SyscallResult {
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);

    // Lookup the directory in VFS
    let inode = match crate::fs::vfs::lookup(&resolved_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    // Verify it is a directory
    if !inode.inode().is_dir() {
        return Errno::ENOTDIR.into();
    }

    // Update current task's cwd
    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let task_arc = match crate::process::scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    task_arc.lock().cwd = resolved_path;
    0 // Success
}

/// `getcwd(buf, size)` — Get current working directory.
pub fn sys_getcwd(buf: *mut u8, size: usize) -> SyscallResult {
    if buf.is_null() || size == 0 {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(buf as *const u8, size) {
        return Errno::EFAULT.into();
    }

    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return 0, // returns NULL on error
    };

    let task_arc = match crate::process::scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return 0,
    };
    let (cwd, jail_root) = {
        let task = task_arc.lock();
        let jail = task.fs_ctx.read().root.clone();
        (task.cwd.clone(), jail)
    };

    let jail_str = jail_root.as_str();
    let display_cwd = if jail_str == "/" {
        cwd
    } else if cwd == jail_str {
        alloc::string::String::from("/")
    } else if let Some(stripped) = cwd.strip_prefix(jail_str) {
        if stripped.starts_with('/') {
            alloc::string::String::from(stripped)
        } else {
            alloc::format!("/{}", stripped)
        }
    } else {
        alloc::string::String::from("/")
    };

    let cwd_bytes = display_cwd.as_bytes();
    if cwd_bytes.len() + 1 > size {
        return Errno::EINVAL.into(); // buffer too small
    }

    // Write to user space
    unsafe {
        core::ptr::copy_nonoverlapping(cwd_bytes.as_ptr(), buf, cwd_bytes.len());
        buf.add(cwd_bytes.len()).write(0); // null terminator
    }

    buf as SyscallResult
}

/// Linux stat structure layout (x86_64 ABI compatible)
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct LinuxStat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub __pad0: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: i64,
    pub st_mtime: i64,
    pub st_mtime_nsec: i64,
    pub st_ctime: i64,
    pub st_ctime_nsec: i64,
    pub __unused: [i64; 3],
}

fn file_type_to_st_mode(file_type: FileType) -> u32 {
    match file_type {
        FileType::Regular => 0o100000,
        FileType::Directory => 0o040000,
        FileType::CharDevice => 0o020000,
        FileType::BlockDevice => 0o060000,
        FileType::Pipe => 0o010000,
        FileType::Symlink => 0o120000,
        FileType::Socket => 0o140000,
    }
}

fn populate_stat(inode_ops: &dyn crate::fs::inode::InodeOps) -> LinuxStat {
    let inode = inode_ops.inode();
    let mode = file_type_to_st_mode(inode.file_type) | (inode.permissions.mode as u32);

    LinuxStat {
        st_dev: inode.dev,
        st_ino: inode.ino,
        st_nlink: inode.nlink as u64,
        st_mode: mode,
        st_uid: inode.uid,
        st_gid: inode.gid,
        __pad0: 0,
        st_rdev: inode.rdev,
        st_size: inode.size as i64,
        st_blksize: 1024,
        st_blocks: inode.blocks as i64,
        st_atime: inode.atime as i64,
        st_atime_nsec: 0,
        st_mtime: inode.mtime as i64,
        st_mtime_nsec: 0,
        st_ctime: inode.ctime as i64,
        st_ctime_nsec: 0,
        __unused: [0; 3],
    }
}

/// `fstat(fd, statbuf)` — Get file status by descriptor.
pub fn sys_fstat(fd: i32, statbuf: *mut LinuxStat) -> SyscallResult {
    if statbuf.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(statbuf as *const u8, core::mem::size_of::<LinuxStat>()) {
        return Errno::EFAULT.into();
    }
    let inode_ops = match proc_fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };
    let stat = populate_stat(inode_ops.as_ref());
    unsafe {
        statbuf.write(stat);
    }
    0
}

/// `newfstatat(dfd, pathname, statbuf, flags)` — Get file status relative to directory fd.
pub fn sys_newfstatat(
    dfd: i32,
    pathname: *const u8,
    statbuf: *mut LinuxStat,
    _flags: i32,
) -> SyscallResult {
    if pathname.is_null() || statbuf.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(statbuf as *const u8, core::mem::size_of::<LinuxStat>()) {
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

    let follow_last = (_flags & 0x100) == 0; // AT_SYMLINK_NOFOLLOW = 0x100

    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, follow_last) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    let stat = populate_stat(inode_ops.as_ref());
    unsafe {
        statbuf.write(stat);
    }
    0
}

/// `faccessat(dfd, pathname, mode, flags)` — Check user's permissions for a file relative to directory fd.
pub fn sys_faccessat(dfd: i32, pathname: *const u8, mode: i32, _flags: i32) -> SyscallResult {
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

    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, true) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    let inode = inode_ops.inode();
    if mode != 0 {
        let mut mask = 0;
        if (mode & 4) != 0 {
            mask |= MAY_READ;
        }
        if (mode & 2) != 0 {
            mask |= MAY_WRITE;
        }
        if (mode & 1) != 0 {
            mask |= MAY_EXEC;
        }
        if let Err(e) = check_permission(inode, mask) {
            return e as SyscallResult;
        }
    }

    0
}

/// `mkdir(pathname, mode)` — Create a directory.
pub fn sys_mkdir(pathname: *const u8, mode: u32) -> SyscallResult {
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    sys_mkdir_with_resolved_path(resolved_path, mode)
}

pub fn sys_mkdir_with_resolved_path(resolved_path: String, _mode: u32) -> SyscallResult {
    // kprintln!("[syscall] mkdir(\"{}\")", resolved_path);

    // Check if the destination already exists
    if crate::fs::vfs::lookup(&resolved_path).is_some() {
        return Errno::EEXIST.into();
    }

    // Split resolved_path into parent directory and base name
    let (parent_path, name) = crate::fs::path::split_path(&resolved_path);

    // Lookup parent directory
    let parent_inode = match crate::fs::vfs::lookup(parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    // Make sure parent is a directory
    if parent_inode.inode().file_type != FileType::Directory {
        return Errno::ENOTDIR.into();
    }

    if let Err(e) = check_permission(parent_inode.inode(), MAY_WRITE) {
        return e as SyscallResult;
    }
    if let Err(e) = check_permission(parent_inode.inode(), MAY_EXEC) {
        return e as SyscallResult;
    }

    match parent_inode.mkdir(name) {
        Some(_) => 0,
        None => Errno::EACCES.into(),
    }
}

/// `rmdir(pathname)` — Remove a directory.
pub fn sys_rmdir(pathname: *const u8) -> SyscallResult {
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    sys_rmdir_with_resolved_path(resolved_path)
}

pub fn sys_rmdir_with_resolved_path(resolved_path: String) -> SyscallResult {
    // kprintln!("[syscall] rmdir(\"{}\")", resolved_path);

    // Split resolved_path into parent directory and base name
    let (parent_path, name) = crate::fs::path::split_path(&resolved_path);

    // Lookup parent directory
    let parent_inode = match crate::fs::vfs::lookup(parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    // Make sure parent is a directory
    if parent_inode.inode().file_type != FileType::Directory {
        return Errno::ENOTDIR.into();
    }

    if let Err(e) = check_permission(parent_inode.inode(), MAY_WRITE) {
        return e as SyscallResult;
    }
    if let Err(e) = check_permission(parent_inode.inode(), MAY_EXEC) {
        return e as SyscallResult;
    }

    match parent_inode.rmdir(name) {
        Ok(_) => {
            crate::fs::vfs::invalidate_dentry(&resolved_path);
            0
        }
        Err(e) => e as SyscallResult,
    }
}

/// `unlink(pathname)` — Remove a file.
pub fn sys_unlink(pathname: *const u8) -> SyscallResult {
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    sys_unlink_with_resolved_path(resolved_path)
}

pub fn sys_unlink_with_resolved_path(resolved_path: String) -> SyscallResult {
    // kprintln!("[syscall] unlink(\"{}\")", resolved_path);

    // Split resolved_path into parent directory and base name
    let (parent_path, name) = crate::fs::path::split_path(&resolved_path);

    // Lookup parent directory
    let parent_inode = match crate::fs::vfs::lookup(parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    // Make sure parent is a directory
    if parent_inode.inode().file_type != FileType::Directory {
        return Errno::ENOTDIR.into();
    }

    if let Err(e) = check_permission(parent_inode.inode(), MAY_WRITE) {
        return e as SyscallResult;
    }
    if let Err(e) = check_permission(parent_inode.inode(), MAY_EXEC) {
        return e as SyscallResult;
    }

    match parent_inode.unlink(name) {
        Ok(_) => {
            crate::fs::vfs::invalidate_dentry(&resolved_path);
            0
        }
        Err(e) => e as SyscallResult,
    }
}

/// `stat(pathname, statbuf)` — Get file status by path.
pub fn sys_stat(pathname: *const u8, statbuf: *mut LinuxStat) -> SyscallResult {
    if statbuf.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(statbuf as *const u8, core::mem::size_of::<LinuxStat>()) {
        return Errno::EFAULT.into();
    }
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved = crate::fs::vfs::resolve_relative_path(&raw_path);
    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!("[syscall] stat(\"{}\")", resolved);
    }

    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved, true) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    let stat = populate_stat(inode_ops.as_ref());
    unsafe {
        statbuf.write(stat);
    }
    0
}

/// `lstat(pathname, statbuf)` — Get file status by path, not following symlinks.
pub fn sys_lstat(pathname: *const u8, statbuf: *mut LinuxStat) -> SyscallResult {
    if statbuf.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(statbuf as *const u8, core::mem::size_of::<LinuxStat>()) {
        return Errno::EFAULT.into();
    }
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved = crate::fs::vfs::resolve_relative_path(&raw_path);
    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!("[syscall] lstat(\"{}\")", resolved);
    }

    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved, false) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    let stat = populate_stat(inode_ops.as_ref());
    unsafe {
        statbuf.write(stat);
    }
    0
}

/// `access(pathname, mode)` — Check file accessibility.
///
/// We defer to `faccessat` with `AT_FDCWD` and no flags.
pub fn sys_access(pathname: *const u8, mode: i32) -> SyscallResult {
    sys_faccessat(-100, pathname, mode, 0)
}

/// `rename(oldpath, newpath)` — Rename a file or directory.
pub fn sys_rename(oldpath: *const u8, newpath: *const u8) -> SyscallResult {
    let raw_old = match unsafe { copy_string_from_user(oldpath) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let raw_new = match unsafe { copy_string_from_user(newpath) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_old = crate::fs::vfs::resolve_relative_path(&raw_old);
    let resolved_new = crate::fs::vfs::resolve_relative_path(&raw_new);
    sys_rename_with_resolved_paths(resolved_old, resolved_new)
}

pub fn sys_rename_with_resolved_paths(resolved_old: String, resolved_new: String) -> SyscallResult {
    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!(
            "[syscall] rename(\"{}\" -> \"{}\")",
            resolved_old,
            resolved_new
        );
    }

    // Split paths into parent + name
    let (old_parent_path, old_name) = crate::fs::path::split_path(&resolved_old);
    let (new_parent_path, new_name) = crate::fs::path::split_path(&resolved_new);

    let old_parent = match crate::fs::vfs::lookup(old_parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    let src_inode_ops = match crate::fs::vfs::lookup(&resolved_old) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    let new_parent = match crate::fs::vfs::lookup(new_parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    if src_inode_ops.inode().file_type == FileType::Directory {
        // Fast path: attempt atomic directory entry link transfer without copying trees
        if let Some(node) = old_parent.unlink_entry(old_name) {
            let _ = new_parent.rmdir(new_name);
            if new_parent.link_entry(new_name, node.clone()).is_ok() {
                crate::fs::vfs::invalidate_dentry(&resolved_old);
                crate::fs::vfs::invalidate_dentry(&resolved_new);
                return 0;
            } else {
                let _ = old_parent.link_entry(old_name, node);
            }
        }

        let new_dir = match new_parent
            .mkdir(new_name)
            .or_else(|| new_parent.create(new_name, FileType::Directory))
        {
            Some(i) => i,
            None => return Errno::ENOSPC.into(),
        };

        fn copy_dir_rec(
            src: &alloc::sync::Arc<dyn crate::fs::inode::InodeOps>,
            dst: &alloc::sync::Arc<dyn crate::fs::inode::InodeOps>,
        ) {
            for entry in src.readdir() {
                if entry.name == "." || entry.name == ".." {
                    continue;
                }
                if let Some(child_src) = src.lookup(&entry.name) {
                    if entry.file_type == FileType::Directory {
                        if let Some(child_dst) = dst
                            .mkdir(&entry.name)
                            .or_else(|| dst.create(&entry.name, FileType::Directory))
                        {
                            copy_dir_rec(&child_src, &child_dst);
                        }
                    } else {
                        if let Some(child_dst) = dst.create(&entry.name, entry.file_type) {
                            let _ = child_dst.set_permissions(child_src.inode().permissions.mode);
                            let _ =
                                child_dst.set_owner(child_src.inode().uid, child_src.inode().gid);
                            let file_size = child_src.inode().size as usize;
                            if file_size > 0 {
                                let mut buf = alloc::vec![0u8; file_size];
                                if child_src.read(0, &mut buf).is_ok() {
                                    let _ = child_dst.write(0, &buf);
                                }
                            }
                        }
                    }
                }
            }
        }

        fn remove_dir_rec(dir: &alloc::sync::Arc<dyn crate::fs::inode::InodeOps>) {
            for entry in dir.readdir() {
                if entry.name == "." || entry.name == ".." {
                    continue;
                }
                if entry.file_type == FileType::Directory {
                    if let Some(child) = dir.lookup(&entry.name) {
                        remove_dir_rec(&child);
                    }
                    let _ = dir.rmdir(&entry.name);
                } else {
                    let _ = dir.unlink(&entry.name);
                }
            }
        }

        copy_dir_rec(&src_inode_ops, &new_dir);
        remove_dir_rec(&src_inode_ops);
        let _ = old_parent.rmdir(old_name);
    } else {
        // Fast path: attempt atomic entry link transfer without copying data (preserves entire inode)
        if let Some(node) = old_parent.unlink_entry(old_name) {
            // Remove target if it already exists per POSIX
            let _ = new_parent.unlink(new_name);
            if new_parent.link_entry(new_name, node.clone()).is_ok() {
                crate::fs::vfs::invalidate_dentry(&resolved_old);
                crate::fs::vfs::invalidate_dentry(&resolved_new);
                return 0;
            } else {
                // If link_entry failed, restore old entry
                let _ = old_parent.link_entry(old_name, node);
            }
        }

        let file_size = src_inode_ops.inode().size as usize;
        let mut buf = alloc::vec![0u8; file_size];
        if file_size > 0 {
            let _ = src_inode_ops.read(0, &mut buf);
        }

        let src_mode = src_inode_ops.inode().permissions.mode;
        let src_uid = src_inode_ops.inode().uid;
        let src_gid = src_inode_ops.inode().gid;
        let src_type = src_inode_ops.inode().file_type;

        // If the target already exists, remove it first per POSIX rename semantics
        if new_parent.lookup(new_name).is_some() {
            let _ = new_parent.unlink(new_name);
        }

        let new_inode = match new_parent.create(new_name, src_type) {
            Some(i) => i,
            None => return Errno::ENOSPC.into(),
        };
        let _ = new_inode.set_permissions(src_mode);
        let _ = new_inode.set_owner(src_uid, src_gid);

        if file_size > 0 {
            let _ = new_inode.write(0, &buf);
        }

        let _ = old_parent.unlink(old_name);
    }

    crate::fs::vfs::invalidate_dentry(&resolved_old);
    crate::fs::vfs::invalidate_dentry(&resolved_new);
    0
}

/// `link(oldpath, newpath)` — Create a hard link.
pub fn sys_link(oldpath: *const u8, newpath: *const u8) -> SyscallResult {
    sys_linkat(-100, oldpath, -100, newpath, 0)
}

/// `linkat(olddirfd, oldpath, newdirfd, newpath, flags)` — Create a hard link relative to directory file descriptors.
pub fn sys_linkat(
    olddirfd: i32,
    oldpath: *const u8,
    newdirfd: i32,
    newpath: *const u8,
    flags: i32,
) -> SyscallResult {
    if oldpath.is_null() || newpath.is_null() {
        return Errno::EFAULT.into();
    }
    // SAFETY: copy_string_from_user validates bounds and reads until null terminator.
    let raw_old = match unsafe { copy_string_from_user(oldpath) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    // SAFETY: copy_string_from_user validates bounds and reads until null terminator.
    let raw_new = match unsafe { copy_string_from_user(newpath) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    if raw_old.is_empty() && (flags & 0x1000 == 0) {
        return Errno::ENOENT.into();
    }
    if raw_new.is_empty() {
        return Errno::ENOENT.into();
    }

    let resolved_old = match crate::fs::vfs::resolve_relative_path_at(olddirfd, &raw_old) {
        Ok(path) => path,
        Err(e) => return e.into(),
    };
    let resolved_new = match crate::fs::vfs::resolve_relative_path_at(newdirfd, &raw_new) {
        Ok(path) => path,
        Err(e) => return e.into(),
    };

    sys_link_with_resolved_paths(resolved_old, resolved_new, flags)
}

/// Core hardlink logic with already resolved paths.
pub fn sys_link_with_resolved_paths(
    resolved_old: String,
    resolved_new: String,
    flags: i32,
) -> SyscallResult {
    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!(
            "[syscall] link(\"{}\" -> \"{}\")",
            resolved_old,
            resolved_new
        );
    }

    let follow = (flags & 0x400) != 0; // AT_SYMLINK_FOLLOW
    let src_inode = match crate::fs::vfs::lookup_follow(&resolved_old, follow) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    // POSIX forbids creating hard links to directories
    if src_inode.inode().is_dir() {
        return Errno::EPERM.into();
    }

    let (new_parent_path, new_name) = crate::fs::path::split_path(&resolved_new);
    let new_parent = match crate::fs::vfs::lookup(new_parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    if !new_parent.inode().is_dir() {
        return Errno::ENOTDIR.into();
    }

    // Cross-device hard links are forbidden by POSIX
    if src_inode.inode().dev != new_parent.inode().dev {
        return Errno::EXDEV.into();
    }

    // Verify write and execute permissions on the target directory
    if let Err(e) =
        crate::fs::inode::check_permission(new_parent.inode(), crate::fs::inode::MAY_WRITE)
    {
        return e as SyscallResult;
    }
    if let Err(e) =
        crate::fs::inode::check_permission(new_parent.inode(), crate::fs::inode::MAY_EXEC)
    {
        return e as SyscallResult;
    }

    // If destination already exists, return EEXIST per POSIX
    if new_parent.lookup(new_name).is_some() {
        return Errno::EEXIST.into();
    }

    match new_parent.link_entry(new_name, src_inode.clone()) {
        Ok(()) => {
            if let Err(e) = src_inode.inc_nlink() {
                let _ = new_parent.unlink_entry(new_name);
                return e as SyscallResult;
            }
            crate::fs::vfs::invalidate_dentry(&resolved_new);
            0
        }
        Err(e) => e as SyscallResult,
    }
}

/// `readlink(pathname, buf, bufsize)` — Read the value of a symbolic link.
pub fn sys_readlink(pathname: *const u8, buf: *mut u8, bufsize: usize) -> SyscallResult {
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    sys_readlink_with_resolved_path(resolved_path, buf, bufsize)
}

/// Core readlink logic with an already resolved path.
pub fn sys_readlink_with_resolved_path(
    resolved_path: String,
    buf: *mut u8,
    bufsize: usize,
) -> SyscallResult {
    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!("[syscall] readlink(\"{}\")", resolved_path);
    }

    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, false) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    if inode_ops.inode().file_type != FileType::Symlink {
        return Errno::EINVAL.into();
    }

    if buf.is_null() || bufsize == 0 {
        return 0;
    }
    if !validate_user_ptr(buf as *const u8, bufsize) {
        return Errno::EFAULT.into();
    }

    let mut kernel_buf = alloc::vec![0u8; bufsize];
    match inode_ops.read(0, &mut kernel_buf) {
        Ok(n) => {
            // SAFETY: The destination user buffer is checked using validate_user_ptr.
            unsafe {
                core::ptr::copy_nonoverlapping(kernel_buf.as_ptr(), buf, n);
            }
            n as SyscallResult
        }
        Err(e) => e as SyscallResult,
    }
}

/// `readlinkat(dirfd, pathname, buf, bufsize)` — `readlink` relative to a directory fd.
pub fn sys_readlinkat(
    dirfd: i32,
    pathname: *const u8,
    buf: *mut u8,
    bufsize: usize,
) -> SyscallResult {
    if pathname.is_null() {
        return Errno::EFAULT.into();
    }
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let resolved_path = match crate::fs::vfs::resolve_relative_path_at(dirfd, &raw_path) {
        Ok(path) => path,
        Err(e) => return e.into(),
    };
    sys_readlink_with_resolved_path(resolved_path, buf, bufsize)
}

/// `symlink(target, linkpath)` — Create a symbolic link.
pub fn sys_symlink(target: *const u8, linkpath: *const u8) -> SyscallResult {
    let raw_target = match unsafe { copy_string_from_user(target) } {
        Some(t) => t,
        None => return Errno::EFAULT.into(),
    };
    let raw_linkpath = match unsafe { copy_string_from_user(linkpath) } {
        Some(l) => l,
        None => return Errno::EFAULT.into(),
    };

    let resolved_linkpath = crate::fs::vfs::resolve_relative_path(&raw_linkpath);
    sys_symlink_with_resolved_linkpath(raw_target, resolved_linkpath)
}

/// Core symlink logic with an already resolved linkpath.
pub fn sys_symlink_with_resolved_linkpath(
    raw_target: String,
    resolved_linkpath: String,
) -> SyscallResult {
    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!(
            "[syscall] symlink(\"{}\" -> \"{}\")",
            resolved_linkpath,
            raw_target
        );
    }

    // Check if the destination linkpath already exists
    if crate::fs::vfs::lookup(&resolved_linkpath).is_some() {
        return Errno::EEXIST.into();
    }

    // Split resolved_linkpath into parent directory and base name
    let (parent_path, name) = crate::fs::path::split_path(&resolved_linkpath);

    // Lookup parent directory
    let parent_inode = match crate::fs::vfs::lookup(parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    // Make sure parent is a directory
    if parent_inode.inode().file_type != FileType::Directory {
        return Errno::ENOTDIR.into();
    }

    // Create the symlink inode
    let symlink_inode = match parent_inode.create(name, FileType::Symlink) {
        Some(i) => i,
        None => return Errno::ENOSPC.into(),
    };

    // Write the target path into the symlink file
    let target_bytes = raw_target.as_bytes();
    match symlink_inode.write(0, target_bytes) {
        Ok(n) if n == target_bytes.len() => 0,
        Ok(_) => Errno::ENOSPC.into(),
        Err(e) => e as SyscallResult,
    }
}

/// `symlinkat(target, newdirfd, linkpath)` — Create a symbolic link relative to a directory fd.
pub fn sys_symlinkat(target: *const u8, newdirfd: i32, linkpath: *const u8) -> SyscallResult {
    let raw_target = match unsafe { copy_string_from_user(target) } {
        Some(t) => t,
        None => return Errno::EFAULT.into(),
    };
    let raw_linkpath = match unsafe { copy_string_from_user(linkpath) } {
        Some(l) => l,
        None => return Errno::EFAULT.into(),
    };

    let resolved_linkpath = match crate::fs::vfs::resolve_relative_path_at(newdirfd, &raw_linkpath)
    {
        Ok(path) => path,
        Err(e) => return e.into(),
    };
    sys_symlink_with_resolved_linkpath(raw_target, resolved_linkpath)
}

/// `poll` fd event struct.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

/// `poll(fds, nfds, timeout)` — Wait for events on file descriptors.
struct PollWaitGuard {
    wq: alloc::sync::Arc<crate::sync::wait_queue::WaitQueue>,
}

impl PollWaitGuard {
    fn new() -> Self {
        let wq = alloc::sync::Arc::new(crate::sync::wait_queue::WaitQueue::new());
        x86_64::instructions::interrupts::without_interrupts(|| {
            crate::fs::epoll::EPOLL_WAIT_QUEUES.lock().push(wq.clone());
        });
        Self { wq }
    }
}

impl Drop for PollWaitGuard {
    fn drop(&mut self) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut wqs = crate::fs::epoll::EPOLL_WAIT_QUEUES.lock();
            wqs.retain(|w| !alloc::sync::Arc::ptr_eq(w, &self.wq));
        });
    }
}

/// `poll(fds, nfds, timeout)` — Wait for events on file descriptors.
///
/// Marks ready fds and blocks until events occur, timeout expires, or a signal is caught.
pub fn sys_poll(fds: *mut u8, nfds: u64, timeout: i32) -> SyscallResult {
    if fds.is_null() || nfds == 0 {
        return 0;
    }
    let total_size = match (nfds as usize).checked_mul(core::mem::size_of::<PollFd>()) {
        Some(s) => s,
        None => return Errno::EINVAL.into(),
    };
    if validate_user_ptr_write(fds, total_size).is_err() {
        return Errno::EFAULT.into();
    }

    let mut local_fds = alloc::vec![PollFd { fd: 0, events: 0, revents: 0 }; nfds as usize];
    unsafe {
        core::ptr::copy_nonoverlapping(fds as *const PollFd, local_fds.as_mut_ptr(), nfds as usize);
    }

    let start_ticks = crate::arch::x86_64::interrupts::timer_ticks();
    // timeout < 0  → wait forever (no deadline)
    // timeout = 0  → return immediately (poll, no block)
    // timeout > 0  → wait up to `timeout` ms
    let timeout_ticks = if timeout > 0 {
        Some((timeout as u64 + 9) / 10) // convert ms → 10ms ticks, round up
    } else {
        None
    };
    let wait_forever = timeout < 0;

    // ── Register the wait-queue BEFORE the first poll check ──────────────────
    // This closes the missed-wakeup race: if an event (e.g. TCP data arriving)
    // fires wake_all_epolls() between the empty-buffer check and wq.wait(), the
    // wakeup is NOT lost because the guard is already in EPOLL_WAIT_QUEUES.
    // For timeout=0 (immediate) we still skip the wait, but registration is
    // harmless and avoids a special-case branch.
    let poll_guard = if timeout != 0 {
        Some(PollWaitGuard::new())
    } else {
        None
    };

    loop {
        let mut ready = 0i64;
        for pfd in local_fds.iter_mut() {
            if pfd.fd >= 0 {
                if let Some(inode) = proc_fd::current_task_read_fd(pfd.fd) {
                    let revents = inode.poll(pfd.events as u32);
                    pfd.revents = revents as i16;
                    if revents != 0 {
                        ready += 1;
                    }
                } else {
                    pfd.revents = 0x0008; // POLLERR — fd not open
                    ready += 1;
                }
            } else {
                pfd.revents = 0;
            }
        }

        if ready > 0 || timeout == 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    local_fds.as_ptr(),
                    fds as *mut PollFd,
                    nfds as usize,
                );
            }
            return ready as SyscallResult;
        }

        if let Some(limit) = timeout_ticks {
            let current = crate::arch::x86_64::interrupts::timer_ticks();
            if current >= start_ticks + limit {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        local_fds.as_ptr(),
                        fds as *mut PollFd,
                        nfds as usize,
                    );
                }
                return 0; // timeout expired
            }
        }

        // Handle pending signals — return EINTR if any unblocked signal is pending.
        let current_pid = match crate::process::scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        };
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
            let task = task_arc.lock();
            let unblocked = task.pending_signals & !task.blocked_signals;
            if unblocked != 0 {
                return Errno::EINTR.into();
            }
        }

        // Sleep until an event fires or a deadline expires.
        // The guard is already registered in EPOLL_WAIT_QUEUES, so any
        // wake_all_epolls() call (from TCP receive, pipe write, etc.) will
        // unblock us. We then loop back to re-check all fds.
        if let Some(ref guard) = poll_guard {
            if !wait_forever {
                if let Some(limit) = timeout_ticks {
                    crate::fs::epoll::add_sleep_timeout(current_pid, start_ticks + limit);
                }
            }
            guard.wq.wait();
            if !wait_forever {
                if timeout_ticks.is_some() {
                    crate::fs::epoll::remove_sleep_timeout(current_pid);
                }
            }
        }
    }
}

/// `pselect6(nfds, readfds, writefds, exceptfds, timeout, sigmask)` — Synchronous I/O multiplexing.
pub fn sys_pselect6(
    nfds: i32,
    readfds: *mut u64,
    writefds: *mut u64,
    exceptfds: *mut u64,
    timeout: *const TimeSpec,
    _sigmask: *const u8,
) -> SyscallResult {
    if nfds < 0 || nfds > 1024 {
        return Errno::EINVAL.into();
    }
    if nfds == 0 && readfds.is_null() && writefds.is_null() && exceptfds.is_null() {
        if !timeout.is_null() {
            if !validate_user_ptr(timeout as *const u8, core::mem::size_of::<TimeSpec>()) {
                return Errno::EFAULT.into();
            }
            let ts = unsafe { *timeout };
            let ms = (ts.tv_sec * 1000).saturating_add(ts.tv_nsec / 1_000_000);
            if ms > 0 {
                let ticks = ((ms as u64) + 9) / 10;
                let start = crate::arch::x86_64::interrupts::timer_ticks();
                while crate::arch::x86_64::interrupts::timer_ticks() < start + ticks {
                    crate::process::scheduler::yield_now();
                }
            }
        }
        return 0;
    }

    let num_words = ((nfds as usize) + 63) / 64;
    let byte_size = num_words * 8;

    let mut in_read = [0u64; 16];
    let mut in_write = [0u64; 16];
    let mut in_except = [0u64; 16];

    if !readfds.is_null() {
        if validate_user_ptr_write(readfds as *mut u8, byte_size).is_err() {
            return Errno::EFAULT.into();
        }
        unsafe {
            core::ptr::copy_nonoverlapping(readfds, in_read.as_mut_ptr(), num_words);
        }
    }
    if !writefds.is_null() {
        if validate_user_ptr_write(writefds as *mut u8, byte_size).is_err() {
            return Errno::EFAULT.into();
        }
        unsafe {
            core::ptr::copy_nonoverlapping(writefds, in_write.as_mut_ptr(), num_words);
        }
    }
    if !exceptfds.is_null() {
        if validate_user_ptr_write(exceptfds as *mut u8, byte_size).is_err() {
            return Errno::EFAULT.into();
        }
        unsafe {
            core::ptr::copy_nonoverlapping(exceptfds, in_except.as_mut_ptr(), num_words);
        }
    }

    let timeout_ms = if !timeout.is_null() {
        if !validate_user_ptr(timeout as *const u8, core::mem::size_of::<TimeSpec>()) {
            return Errno::EFAULT.into();
        }
        let ts = unsafe { *timeout };
        Some((ts.tv_sec * 1000).saturating_add(ts.tv_nsec / 1_000_000))
    } else {
        None
    };

    let start_ticks = crate::arch::x86_64::interrupts::timer_ticks();
    let timeout_ticks = timeout_ms.map(|ms| if ms > 0 { ((ms as u64) + 9) / 10 } else { 0 });
    let wait_forever = timeout.is_null(); // NULL timeout = block until event

    // Pre-register the poll wait-queue before the first fd check to close the
    // missed-wakeup race (same fix as sys_poll).
    let pselect_guard = if timeout_ticks != Some(0) {
        Some(PollWaitGuard::new())
    } else {
        None
    };

    loop {
        let mut out_read = [0u64; 16];
        let mut out_write = [0u64; 16];
        let mut out_except = [0u64; 16];
        let mut total_ready = 0i64;

        for fd in 0..nfds {
            let word_idx = (fd as usize) / 64;
            let bit_idx = (fd as usize) % 64;
            let mask = 1u64 << bit_idx;

            let check_read = (in_read[word_idx] & mask) != 0;
            let check_write = (in_write[word_idx] & mask) != 0;
            let check_except = (in_except[word_idx] & mask) != 0;

            if !check_read && !check_write && !check_except {
                continue;
            }

            if let Some(inode) = proc_fd::current_task_read_fd(fd) {
                let mut events = 0u32;
                if check_read {
                    events |= 0x0001;
                } // POLLIN
                if check_write {
                    events |= 0x0004;
                } // POLLOUT
                if check_except {
                    events |= 0x0002;
                } // POLLPRI

                let revents = inode.poll(events);
                if check_read
                    && (revents & 0x0001 != 0 || revents & 0x0010 != 0 || revents & 0x0008 != 0)
                {
                    out_read[word_idx] |= mask;
                    total_ready += 1;
                }
                if check_write && (revents & 0x0004 != 0) {
                    out_write[word_idx] |= mask;
                    total_ready += 1;
                }
                if check_except && (revents & 0x0002 != 0) {
                    out_except[word_idx] |= mask;
                    total_ready += 1;
                }
            } else {
                return Errno::EBADF.into();
            }
        }

        if total_ready > 0 || timeout_ticks == Some(0) {
            unsafe {
                if !readfds.is_null() {
                    core::ptr::copy_nonoverlapping(out_read.as_ptr(), readfds, num_words);
                }
                if !writefds.is_null() {
                    core::ptr::copy_nonoverlapping(out_write.as_ptr(), writefds, num_words);
                }
                if !exceptfds.is_null() {
                    core::ptr::copy_nonoverlapping(out_except.as_ptr(), exceptfds, num_words);
                }
            }
            return total_ready;
        }

        if let Some(limit) = timeout_ticks {
            let current = crate::arch::x86_64::interrupts::timer_ticks();
            if current >= start_ticks + limit {
                unsafe {
                    if !readfds.is_null() {
                        core::ptr::write_bytes(readfds, 0, num_words);
                    }
                    if !writefds.is_null() {
                        core::ptr::write_bytes(writefds, 0, num_words);
                    }
                    if !exceptfds.is_null() {
                        core::ptr::write_bytes(exceptfds, 0, num_words);
                    }
                }
                return 0;
            }
        }

        let current_pid = match crate::process::scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        };
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
            let task = task_arc.lock();
            let unblocked = task.pending_signals & !task.blocked_signals;
            if unblocked != 0 {
                return Errno::EINTR.into();
            }
        }

        if let Some(ref guard) = pselect_guard {
            if !wait_forever {
                if let Some(limit) = timeout_ticks {
                    crate::fs::epoll::add_sleep_timeout(current_pid, start_ticks + limit);
                }
            }
            guard.wq.wait();
            if !wait_forever {
                if timeout_ticks.is_some() {
                    crate::fs::epoll::remove_sleep_timeout(current_pid);
                }
            }
        }
    }
}

/// `chmod(pathname, mode)` — Change file permissions.
pub fn sys_chmod(pathname: *const u8, mode: u32) -> SyscallResult {
    if pathname.is_null() {
        return Errno::EFAULT.into();
    }
    // SAFETY: copy_string_from_user validates bounds and reads until null terminator.
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    if raw_path.is_empty() {
        return Errno::ENOENT.into();
    }

    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    sys_chmod_with_resolved_path_follow(resolved_path, mode, true)
}

pub fn sys_chmod_with_resolved_path_follow(
    resolved_path: String,
    mode: u32,
    follow_last: bool,
) -> SyscallResult {
    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!(
            "[syscall] chmod(\"{}\", mode={:#o}, follow={})",
            resolved_path,
            mode,
            follow_last
        );
    }

    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, follow_last) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    // Check ownership before changing permissions (only owner or root can change)
    let current_uid = if let Some(pid) = crate::process::scheduler::current_pid() {
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
            let task = task_arc.lock();
            (task.euid, task.uid)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    let inode_uid = inode_ops.inode().uid;
    if current_uid.0 != 0 && current_uid.0 != inode_uid && current_uid.1 != inode_uid {
        return Errno::EPERM.into();
    }

    match inode_ops.set_permissions(mode as u16) {
        Ok(_) => 0,
        Err(e) => {
            if e < 0 {
                e as SyscallResult
            } else {
                -e as SyscallResult
            }
        }
    }
}

/// `fchmod(fd, mode)` — Change permissions of an open file descriptor.
pub fn sys_fchmod(fd: i32, mode: u32) -> SyscallResult {
    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!("[syscall] fchmod(fd={}, mode={:#o})", fd, mode);
    }

    let inode_ops = match proc_fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };

    // Check ownership
    let current_uid = if let Some(pid) = crate::process::scheduler::current_pid() {
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
            let task = task_arc.lock();
            (task.euid, task.uid)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    let inode_uid = inode_ops.inode().uid;
    if current_uid.0 != 0 && current_uid.0 != inode_uid && current_uid.1 != inode_uid {
        return Errno::EPERM.into();
    }

    match inode_ops.set_permissions(mode as u16) {
        Ok(_) => 0,
        Err(e) => {
            if e < 0 {
                e as SyscallResult
            } else {
                -e as SyscallResult
            }
        }
    }
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxStatfs {
    pub f_type: i64,       /* Type of filesystem */
    pub f_bsize: i64,      /* Optimal transfer block size */
    pub f_blocks: u64,     /* Total data blocks in filesystem */
    pub f_bfree: u64,      /* Free blocks in filesystem */
    pub f_bavail: u64,     /* Free blocks available to unprivileged user */
    pub f_files: u64,      /* Total file nodes in filesystem */
    pub f_ffree: u64,      /* Free file nodes in filesystem */
    pub f_fsid: [i32; 2],  /* Filesystem ID */
    pub f_namelen: i64,    /* Maximum length of filenames */
    pub f_frsize: i64,     /* Fragment size */
    pub f_flags: i64,      /* Mount flags of filesystem */
    pub f_spare: [i64; 4], /* Padding bytes reserved for future use */
}

fn fs_stats_to_linux_statfs(stats: &crate::fs::vfs::FsStats, fs_name: &str) -> LinuxStatfs {
    let f_type = match fs_name {
        "ext" | "ext2" => 0xEF53,
        "tmpfs" => 0x01021994,
        "procfs" => 0x9fa0,
        "devfs" => 0x1373,
        _ => 0,
    };
    LinuxStatfs {
        f_type,
        f_bsize: stats.block_size as i64,
        f_blocks: stats.total_blocks,
        f_bfree: stats.free_blocks,
        f_bavail: stats.free_blocks,
        f_files: stats.total_inodes,
        f_ffree: stats.free_inodes,
        f_fsid: [0, 0],
        f_namelen: stats.max_name_len as i64,
        f_frsize: stats.block_size as i64,
        f_flags: 0,
        f_spare: [0; 4],
    }
}

/// `statfs(path, buf)` — Get filesystem statistics.
pub fn sys_statfs(path_ptr: *const u8, buf: *mut LinuxStatfs) -> SyscallResult {
    if path_ptr.is_null() || buf.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(path_ptr, 1)
        || !validate_user_ptr(buf as *const u8, core::mem::size_of::<LinuxStatfs>())
    {
        return Errno::EFAULT.into();
    }

    let path_str = unsafe {
        match crate::syscall::validation::copy_string_from_user_pub(path_ptr) {
            Some(s) => s,
            None => return Errno::EFAULT.into(),
        }
    };

    let abs_path = crate::fs::vfs::resolve_relative_path(&path_str);
    let (fs, _) = match crate::fs::vfs::resolve_mount(&abs_path) {
        Some(res) => res,
        None => return Errno::ENOENT.into(),
    };

    let stats = fs.statfs();
    let linux_stats = fs_stats_to_linux_statfs(&stats, fs.name());

    unsafe {
        buf.write(linux_stats);
    }
    0
}

/// `fstatfs(fd, buf)` — Get filesystem statistics.
pub fn sys_fstatfs(fd: i32, buf: *mut LinuxStatfs) -> SyscallResult {
    if buf.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(buf as *const u8, core::mem::size_of::<LinuxStatfs>()) {
        return Errno::EFAULT.into();
    }

    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    // If there is a path associated, resolve its filesystem
    let (stats, fs_name) = if let Some(ref path_str) = file_desc.path {
        let abs_path = crate::fs::vfs::resolve_relative_path(path_str);
        if let Some((fs, _)) = crate::fs::vfs::resolve_mount(&abs_path) {
            (fs.statfs(), alloc::string::String::from(fs.name()))
        } else {
            // Fallback to synthetic
            (
                crate::fs::vfs::FsStats {
                    total_blocks: 1024 * 1024,
                    free_blocks: 512 * 1024,
                    total_inodes: 1024 * 1024,
                    free_inodes: 512 * 1024,
                    block_size: 4096,
                    max_name_len: 255,
                },
                alloc::string::String::from("virtual"),
            )
        }
    } else {
        // Fallback to synthetic
        (
            crate::fs::vfs::FsStats {
                total_blocks: 1024 * 1024,
                free_blocks: 512 * 1024,
                total_inodes: 1024 * 1024,
                free_inodes: 512 * 1024,
                block_size: 4096,
                max_name_len: 255,
            },
            alloc::string::String::from("virtual"),
        )
    };

    let linux_stats = fs_stats_to_linux_statfs(&stats, &fs_name);
    unsafe {
        buf.write(linux_stats);
    }
    0
}

/// `umask(mask)` — Set file mode creation mask.
pub fn sys_umask(mask: u32) -> SyscallResult {
    if crate::syscall::DEBUG_SYSCALLS {
        kprintln!("[syscall] umask(mask={:#o})", mask);
    }

    let current_pid = match crate::process::scheduler::current_pid() {
        Some(pid) => pid,
        None => return 0o022,
    };

    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        let mut task = task_arc.lock();
        let old_mask = task.umask;
        task.umask = mask & 0o777;
        old_mask as SyscallResult
    } else {
        0o022
    }
}

/// Helper function to perform permission checks and call set_owner.
fn change_inode_owner(
    inode_ops: &dyn crate::fs::inode::InodeOps,
    uid: u32,
    gid: u32,
) -> SyscallResult {
    let (euid, is_root) = if let Some(pid) = crate::process::scheduler::current_pid() {
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
            let task = task_arc.lock();
            (task.euid, task.euid == 0)
        } else {
            (0, true)
        }
    } else {
        (0, true)
    };

    let metadata = inode_ops.inode();
    if !is_root && metadata.uid != euid {
        return Errno::EPERM.into();
    }

    let target_uid = if uid == 0xffffffff { metadata.uid } else { uid };
    let target_gid = if gid == 0xffffffff { metadata.gid } else { gid };

    match inode_ops.set_owner(target_uid, target_gid) {
        Ok(_) => 0,
        Err(e) => e as SyscallResult,
    }
}

/// `chown(pathname, uid, gid)` — Change ownership of a file.
pub fn sys_chown(pathname: *const u8, uid: u32, gid: u32) -> SyscallResult {
    if pathname.is_null() {
        return Errno::EFAULT.into();
    }
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, true) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };
    change_inode_owner(inode_ops.as_ref(), uid, gid)
}

/// `lchown(pathname, uid, gid)` — Change ownership of a file, don't follow symlinks.
pub fn sys_lchown(pathname: *const u8, uid: u32, gid: u32) -> SyscallResult {
    if pathname.is_null() {
        return Errno::EFAULT.into();
    }
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, false) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };
    change_inode_owner(inode_ops.as_ref(), uid, gid)
}

/// `fchown(fd, uid, gid)` — Change ownership of an open file descriptor.
pub fn sys_fchown(fd: i32, uid: u32, gid: u32) -> SyscallResult {
    let inode_ops = match proc_fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };
    change_inode_owner(inode_ops.as_ref(), uid, gid)
}

/// `fchownat(dfd, pathname, uid, gid, flags)` — Change ownership relative to a directory fd.
pub fn sys_fchownat(
    dfd: i32,
    pathname: *const u8,
    uid: u32,
    gid: u32,
    flags: i32,
) -> SyscallResult {
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

    let follow_last = (flags & 0x100) == 0; // AT_SYMLINK_NOFOLLOW = 0x100
    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, follow_last) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };
    change_inode_owner(inode_ops.as_ref(), uid, gid)
}

/// `mount(source, target, filesystemtype, mountflags, data)` — Mount a filesystem.
pub fn sys_mount(
    _source: *const u8,
    target: *const u8,
    filesystemtype: *const u8,
    _mountflags: u64,
    _data: *const u8,
) -> SyscallResult {
    if target.is_null() || filesystemtype.is_null() {
        return Errno::EFAULT.into();
    }
    let target_raw = match unsafe { copy_string_from_user(target) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let fs_type = match unsafe { copy_string_from_user(filesystemtype) } {
        Some(t) => t,
        None => return Errno::EFAULT.into(),
    };

    let target_path = crate::fs::vfs::resolve_relative_path(&target_raw);

    let fs_instance: alloc::sync::Arc<dyn crate::fs::vfs::FileSystem> = match fs_type.as_str() {
        "proc" | "procfs" => crate::fs::procfs::create_procfs(),
        "sysfs" => crate::fs::sysfs::create_sysfs(),
        "tmpfs" => crate::fs::tmpfs::create_tmpfs(),
        "devtmpfs" | "devfs" => crate::fs::devfs::create_devfs(),
        _ => return Errno::EINVAL.into(),
    };

    // If the calling task has a private mount namespace, insert into it.
    // Otherwise, fall through to the global VFS.
    let has_private_ns = if let Some(pid) = crate::process::scheduler::current_pid() {
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
            let task = task_arc.lock();
            let ns_id = task.fs_ctx.read().mount_ns.read().id;
            let initial_id = crate::fs::namespace::INITIAL_MOUNT_NS.read().id;
            ns_id != initial_id
        } else {
            false
        }
    } else {
        false
    };

    if has_private_ns {
        // Mount into the task's private namespace only.
        if let Some(pid) = crate::process::scheduler::current_pid() {
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
                let task = task_arc.lock();
                task.fs_ctx
                    .read()
                    .mount_ns
                    .write()
                    .mounts
                    .insert(target_path, fs_instance);
            }
        }
    } else {
        // Global mount (boot-time or non-containerised process).
        crate::fs::vfs::mount(target_path, fs_instance);
    }
    0
}

/// `umount2(target, flags)` — Unmount a filesystem.
pub fn sys_umount2(target: *const u8, _flags: i32) -> SyscallResult {
    if target.is_null() {
        return Errno::EFAULT.into();
    }
    let target_raw = match unsafe { copy_string_from_user(target) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let target_path = crate::fs::vfs::resolve_relative_path(&target_raw);

    // Operate on private namespace if the task has one.
    let has_private_ns = if let Some(pid) = crate::process::scheduler::current_pid() {
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
            let task = task_arc.lock();
            let ns_id = task.fs_ctx.read().mount_ns.read().id;
            let initial_id = crate::fs::namespace::INITIAL_MOUNT_NS.read().id;
            ns_id != initial_id
        } else {
            false
        }
    } else {
        false
    };

    if has_private_ns {
        if let Some(pid) = crate::process::scheduler::current_pid() {
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
                let task = task_arc.lock();
                let removed = task
                    .fs_ctx
                    .read()
                    .mount_ns
                    .write()
                    .mounts
                    .remove(&target_path)
                    .is_some();
                if removed {
                    return 0;
                }
            }
        }
        return Errno::EINVAL.into();
    }

    if crate::fs::vfs::unmount(&target_path) {
        0
    } else {
        Errno::EINVAL.into()
    }
}

/// `unlinkat(dfd, pathname, flags)` — Remove a file/directory relative to a directory fd.
pub fn sys_unlinkat(dfd: i32, pathname: *const u8, flags: i32) -> SyscallResult {
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

    if (flags & 0x200) != 0 {
        // AT_REMOVEDIR
        sys_rmdir_with_resolved_path(resolved_path)
    } else {
        sys_unlink_with_resolved_path(resolved_path)
    }
}

/// `mkdirat(dfd, pathname, mode)` — Create a directory relative to a directory fd.
pub fn sys_mkdirat(dfd: i32, pathname: *const u8, mode: u32) -> SyscallResult {
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
    sys_mkdir_with_resolved_path(resolved_path, mode)
}

/// `renameat(olddirfd, oldpath, newdirfd, newpath)` — Rename relative to directory fds.
pub fn sys_renameat(
    olddirfd: i32,
    oldpath: *const u8,
    newdirfd: i32,
    newpath: *const u8,
) -> SyscallResult {
    if oldpath.is_null() || newpath.is_null() {
        return Errno::EFAULT.into();
    }
    let raw_old = match unsafe { copy_string_from_user(oldpath) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let raw_new = match unsafe { copy_string_from_user(newpath) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_old = match crate::fs::vfs::resolve_relative_path_at(olddirfd, &raw_old) {
        Ok(path) => path,
        Err(e) => return e.into(),
    };
    let resolved_new = match crate::fs::vfs::resolve_relative_path_at(newdirfd, &raw_new) {
        Ok(path) => path,
        Err(e) => return e.into(),
    };

    sys_rename_with_resolved_paths(resolved_old, resolved_new)
}

/// `fchmodat(dfd, pathname, mode, flags)` — Change permissions relative to a directory fd.
pub fn sys_fchmodat(dfd: i32, pathname: *const u8, mode: u32, flags: i32) -> SyscallResult {
    if pathname.is_null() {
        return Errno::EFAULT.into();
    }
    // Linux supports AT_SYMLINK_NOFOLLOW (0x100) and AT_EMPTY_PATH (0x1000)
    if (flags & !(0x100 | 0x1000)) != 0 {
        return Errno::EINVAL.into();
    }
    // SAFETY: copy_string_from_user validates bounds and reads until null terminator.
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    if raw_path.is_empty() && (flags & 0x1000 == 0) {
        return Errno::ENOENT.into();
    }
    let resolved_path = match crate::fs::vfs::resolve_relative_path_at(dfd, &raw_path) {
        Ok(path) => path,
        Err(e) => return e.into(),
    };

    let follow_last = (flags & 0x100) == 0; // AT_SYMLINK_NOFOLLOW = 0x100
    sys_chmod_with_resolved_path_follow(resolved_path, mode, follow_last)
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TimeSpec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TimeVal {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UTimeBuf {
    pub actime: i64,
    pub modtime: i64,
}

const UTIME_NOW: i64 = 0x3fffffff;
const UTIME_OMIT: i64 = 0x3ffffffe;

/// `utimensat(dirfd, pathname, times, flags)` — Change file timestamps with nanosecond precision.
pub fn sys_utimensat(
    dirfd: i32,
    pathname: *const u8,
    times: *const TimeSpec,
    flags: i32,
) -> SyscallResult {
    let now = crate::fs::vfs::current_time_sec() as u64;

    let (mut atime, mut mtime) = if !times.is_null() {
        if !validate_user_ptr(times as *const u8, core::mem::size_of::<[TimeSpec; 2]>()) {
            return Errno::EFAULT.into();
        }
        let ts = unsafe { *(times as *const [TimeSpec; 2]) };
        let a = match ts[0].tv_nsec {
            UTIME_NOW => now,
            UTIME_OMIT => u64::MAX,
            _ => ts[0].tv_sec as u64,
        };
        let m = match ts[1].tv_nsec {
            UTIME_NOW => now,
            UTIME_OMIT => u64::MAX,
            _ => ts[1].tv_sec as u64,
        };
        (a, m)
    } else {
        (now, now)
    };

    let inode_ops = if pathname.is_null() || (flags & 0x1000) != 0 {
        // Operates directly on dirfd (or AT_EMPTY_PATH)
        if dirfd == -100 {
            // AT_FDCWD
            let cwd = if let Some(pid) = crate::process::scheduler::current_pid() {
                if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
                    task_arc.lock().cwd.clone()
                } else {
                    alloc::string::String::from("/")
                }
            } else {
                alloc::string::String::from("/")
            };
            match crate::fs::vfs::lookup(&cwd) {
                Some(i) => i,
                None => return Errno::ENOENT.into(),
            }
        } else {
            let desc = match proc_fd::current_task_get_file_desc(dirfd) {
                Some(d) => d,
                None => return Errno::EBADF.into(),
            };
            desc.inode.clone()
        }
    } else {
        let raw_path = match unsafe { copy_string_from_user(pathname) } {
            Some(p) => p,
            None => return Errno::EFAULT.into(),
        };
        let resolved_path = match crate::fs::vfs::resolve_relative_path_at(dirfd, &raw_path) {
            Ok(path) => path,
            Err(e) => return e.into(),
        };
        let follow_symlinks = (flags & 0x100) == 0; // AT_SYMLINK_NOFOLLOW = 0x100
        match crate::fs::vfs::lookup_follow(&resolved_path, follow_symlinks) {
            Some(i) => i,
            None => return Errno::ENOENT.into(),
        }
    };

    let cur_inode = inode_ops.inode();
    if atime == u64::MAX {
        atime = cur_inode.atime;
    }
    if mtime == u64::MAX {
        mtime = cur_inode.mtime;
    }

    match inode_ops.set_times(atime, mtime) {
        Ok(_) => 0,
        Err(e) => e as SyscallResult,
    }
}

/// `utimes(filename, times)` — Change file timestamps.
pub fn sys_utimes(filename: *const u8, times: *const TimeVal) -> SyscallResult {
    let now = crate::fs::vfs::current_time_sec() as u64;
    let (atime, mtime) = if !times.is_null() {
        if !validate_user_ptr(times as *const u8, core::mem::size_of::<[TimeVal; 2]>()) {
            return Errno::EFAULT.into();
        }
        let tv = unsafe { *(times as *const [TimeVal; 2]) };
        (tv[0].tv_sec as u64, tv[1].tv_sec as u64)
    } else {
        (now, now)
    };

    let raw_path = match unsafe { copy_string_from_user(filename) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, true) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    match inode_ops.set_times(atime, mtime) {
        Ok(_) => 0,
        Err(e) => e as SyscallResult,
    }
}

/// `utime(filename, times)` — Change file timestamps.
pub fn sys_utime(filename: *const u8, times: *const UTimeBuf) -> SyscallResult {
    let now = crate::fs::vfs::current_time_sec() as u64;
    let (atime, mtime) = if !times.is_null() {
        if !validate_user_ptr(times as *const u8, core::mem::size_of::<UTimeBuf>()) {
            return Errno::EFAULT.into();
        }
        let utb = unsafe { *times };
        (utb.actime as u64, utb.modtime as u64)
    } else {
        (now, now)
    };

    let raw_path = match unsafe { copy_string_from_user(filename) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, true) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    match inode_ops.set_times(atime, mtime) {
        Ok(_) => 0,
        Err(e) => e as SyscallResult,
    }
}

/// `renameat2(olddirfd, oldpath, newdirfd, newpath, flags)` — Rename relative to directory fds with flags.
pub fn sys_renameat2(
    olddirfd: i32,
    oldpath: *const u8,
    newdirfd: i32,
    newpath: *const u8,
    flags: u32,
) -> SyscallResult {
    const RENAME_NOREPLACE: u32 = 1;
    const RENAME_EXCHANGE: u32 = 2;
    const RENAME_WHITEOUT: u32 = 4;

    if (flags & !(RENAME_NOREPLACE | RENAME_EXCHANGE | RENAME_WHITEOUT)) != 0 {
        return Errno::EINVAL.into();
    }
    if (flags & (RENAME_NOREPLACE | RENAME_EXCHANGE)) == (RENAME_NOREPLACE | RENAME_EXCHANGE) {
        return Errno::EINVAL.into();
    }

    if oldpath.is_null() || newpath.is_null() {
        return Errno::EFAULT.into();
    }
    let raw_old = match unsafe { copy_string_from_user(oldpath) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let raw_new = match unsafe { copy_string_from_user(newpath) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_old = match crate::fs::vfs::resolve_relative_path_at(olddirfd, &raw_old) {
        Ok(path) => path,
        Err(e) => return e.into(),
    };
    let resolved_new = match crate::fs::vfs::resolve_relative_path_at(newdirfd, &raw_new) {
        Ok(path) => path,
        Err(e) => return e.into(),
    };

    if (flags & RENAME_NOREPLACE) != 0
        && crate::fs::vfs::lookup_follow(&resolved_new, false).is_some()
    {
        return Errno::EEXIST.into();
    }

    sys_rename_with_resolved_paths(resolved_old, resolved_new)
}

/// Linux statx timestamp layout
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct StatxTimestamp {
    pub tv_sec: i64,
    pub tv_nsec: u32,
    pub __reserved: i32,
}

/// Linux statx structure layout (x86_64 ABI compatible)
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct StatX {
    pub stx_mask: u32,
    pub stx_blksize: u32,
    pub stx_attributes: u64,
    pub stx_nlink: u32,
    pub stx_uid: u32,
    pub stx_gid: u32,
    pub stx_mode: u16,
    pub __spare0: [u16; 1],
    pub stx_ino: u64,
    pub stx_size: u64,
    pub stx_blocks: u64,
    pub stx_attributes_mask: u64,
    pub stx_atime: StatxTimestamp,
    pub stx_btime: StatxTimestamp,
    pub stx_ctime: StatxTimestamp,
    pub stx_mtime: StatxTimestamp,
    pub stx_rdev_major: u32,
    pub stx_rdev_minor: u32,
    pub stx_dev_major: u32,
    pub stx_dev_minor: u32,
    pub stx_mnt_id: u64,
    pub stx_dio_mem_align: u32,
    pub stx_dio_offset_align: u32,
    pub __spare2: [u64; 12],
}

fn populate_statx(inode_ops: &dyn crate::fs::inode::InodeOps) -> StatX {
    let stat = populate_stat(inode_ops);
    let mut sx = StatX::default();
    // STATX_BASIC_STATS (0x07ff) | STATX_BTIME (0x0800) | STATX_MNT_ID (0x01000)
    sx.stx_mask = 0x07ff | 0x0800 | 0x1000;
    sx.stx_blksize = stat.st_blksize as u32;
    sx.stx_nlink = stat.st_nlink as u32;
    sx.stx_uid = stat.st_uid;
    sx.stx_gid = stat.st_gid;
    sx.stx_mode = stat.st_mode as u16;
    sx.stx_ino = stat.st_ino;
    sx.stx_size = stat.st_size as u64;
    sx.stx_blocks = stat.st_blocks as u64;
    sx.stx_atime = StatxTimestamp {
        tv_sec: stat.st_atime,
        tv_nsec: stat.st_atime_nsec as u32,
        __reserved: 0,
    };
    sx.stx_mtime = StatxTimestamp {
        tv_sec: stat.st_mtime,
        tv_nsec: stat.st_mtime_nsec as u32,
        __reserved: 0,
    };
    sx.stx_ctime = StatxTimestamp {
        tv_sec: stat.st_ctime,
        tv_nsec: stat.st_ctime_nsec as u32,
        __reserved: 0,
    };
    sx.stx_btime = sx.stx_ctime;
    sx.stx_rdev_major = ((stat.st_rdev >> 8) & 0xfff) as u32;
    sx.stx_rdev_minor = (stat.st_rdev & 0xff) as u32;
    sx.stx_dev_major = ((stat.st_dev >> 8) & 0xfff) as u32;
    sx.stx_dev_minor = (stat.st_dev & 0xff) as u32;
    sx.stx_mnt_id = if inode_ops.inode().dev != 0 {
        inode_ops.inode().dev
    } else {
        1
    };
    sx
}

/// `statx(dfd, pathname, flags, mask, statxbuf)` — Extended file status.
pub fn sys_statx(
    dfd: i32,
    pathname: *const u8,
    flags: i32,
    _mask: u32,
    statxbuf: *mut StatX,
) -> SyscallResult {
    if statxbuf.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(statxbuf as *mut u8, core::mem::size_of::<StatX>()).is_err() {
        return Errno::EFAULT.into();
    }

    let is_empty_path = (flags & 0x1000) != 0; // AT_EMPTY_PATH = 0x1000
    let raw_path = if !pathname.is_null() {
        unsafe { copy_string_from_user(pathname) }
    } else {
        None
    };

    let inode_ops = if is_empty_path && (raw_path.as_deref() == Some("") || raw_path.is_none()) {
        if dfd < 0 {
            return Errno::EBADF.into();
        }
        match proc_fd::current_task_read_fd(dfd) {
            Some(i) => i,
            None => return Errno::EBADF.into(),
        }
    } else {
        let raw = match raw_path {
            Some(p) => p,
            None => return Errno::EFAULT.into(),
        };
        let resolved_path = match crate::fs::vfs::resolve_relative_path_at(dfd, &raw) {
            Ok(path) => path,
            Err(e) => return e.into(),
        };
        let follow_last = (flags & 0x100) == 0; // AT_SYMLINK_NOFOLLOW = 0x100
        match crate::fs::vfs::lookup_follow(&resolved_path, follow_last) {
            Some(i) => i,
            None => return Errno::ENOENT.into(),
        }
    };

    let sx = populate_statx(inode_ops.as_ref());
    unsafe {
        statxbuf.write(sx);
    }
    0
}

/// `fdatasync(fd)` — Synchronize a file's in-core state with storage device.
pub fn sys_fdatasync(fd: i32) -> SyscallResult {
    crate::syscall::fs::sys_fsync(fd)
}

/// `select(nfds, readfds, writefds, exceptfds, timeout)` — Synchronous I/O multiplexing.
pub fn sys_select(
    nfds: i32,
    readfds: *mut u64,
    writefds: *mut u64,
    exceptfds: *mut u64,
    timeout: *const TimeVal,
) -> SyscallResult {
    let ts = if !timeout.is_null() {
        if !validate_user_ptr(timeout as *const u8, core::mem::size_of::<TimeVal>()) {
            return Errno::EFAULT.into();
        }
        let tv = unsafe { core::ptr::read(timeout) };
        if tv.tv_sec < 0 || tv.tv_usec < 0 || tv.tv_usec >= 1_000_000 {
            return Errno::EINVAL.into();
        }
        Some(TimeSpec {
            tv_sec: tv.tv_sec,
            tv_nsec: tv.tv_usec * 1000,
        })
    } else {
        None
    };

    let ts_ptr = match ts.as_ref() {
        Some(t) => t as *const TimeSpec,
        None => core::ptr::null(),
    };

    sys_pselect6(
        nfds,
        readfds,
        writefds,
        exceptfds,
        ts_ptr,
        core::ptr::null(),
    )
}

/// `ppoll(fds, nfds, tmo_p, sigmask, sigsetsize)` — Wait for some event on a file descriptor.
pub fn sys_ppoll(
    fds: *mut u8,
    nfds: u64,
    tmo_p: *const TimeSpec,
    _sigmask: *const u8,
    _sigsetsize: usize,
) -> SyscallResult {
    let timeout_ms = if !tmo_p.is_null() {
        if !validate_user_ptr(tmo_p as *const u8, core::mem::size_of::<TimeSpec>()) {
            return Errno::EFAULT.into();
        }
        let ts = unsafe { core::ptr::read(tmo_p) };
        if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
            return Errno::EINVAL.into();
        }
        (ts.tv_sec * 1000).saturating_add((ts.tv_nsec + 999_999) / 1_000_000) as i32
    } else {
        -1
    };

    sys_poll(fds, nfds, timeout_ms)
}

/// `epoll_create(size)` — Open an epoll file descriptor.
pub fn sys_epoll_create(size: i32) -> SyscallResult {
    if size <= 0 {
        return Errno::EINVAL.into();
    }
    crate::fs::epoll::sys_epoll_create1(0)
}

/// `epoll_pwait(epfd, events, maxevents, timeout, sigmask, sigsetsize)` — Wait for an I/O event on an epoll file descriptor.
pub fn sys_epoll_pwait(
    epfd: i32,
    events: *mut crate::fs::epoll::EpollEvent,
    maxevents: i32,
    timeout: i32,
    _sigmask: *const u8,
    _sigsetsize: usize,
) -> SyscallResult {
    crate::fs::epoll::sys_epoll_wait(epfd, events, maxevents, timeout)
}

/// `epoll_pwait2(epfd, events, maxevents, timeout_ts, sigmask, sigsetsize)` — Wait for an I/O event on an epoll file descriptor with nanosecond resolution.
pub fn sys_epoll_pwait2(
    epfd: i32,
    events: *mut crate::fs::epoll::EpollEvent,
    maxevents: i32,
    timeout_ts: *const TimeSpec,
    _sigmask: *const u8,
    _sigsetsize: usize,
) -> SyscallResult {
    let timeout_ms = if !timeout_ts.is_null() {
        if !validate_user_ptr(timeout_ts as *const u8, core::mem::size_of::<TimeSpec>()) {
            return Errno::EFAULT.into();
        }
        let ts = unsafe { core::ptr::read(timeout_ts) };
        if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
            return Errno::EINVAL.into();
        }
        (ts.tv_sec * 1000).saturating_add((ts.tv_nsec + 999_999) / 1_000_000) as i32
    } else {
        -1
    };
    crate::fs::epoll::sys_epoll_wait(epfd, events, maxevents, timeout_ms)
}

// ── Extended Attributes (xattrs) ─────────────────────────────────────

fn resolve_xattr_path(
    path: *const u8,
    follow: bool,
) -> Result<Arc<dyn crate::fs::inode::InodeOps>, i64> {
    let raw = match unsafe { crate::syscall::validation::copy_string_from_user(path) } {
        Some(p) => p,
        None => return Err(-14), // EFAULT
    };
    let resolved = crate::fs::vfs::resolve_relative_path(&raw);
    match crate::fs::vfs::lookup_follow(&resolved, follow) {
        Some(i) => Ok(i),
        None => Err(-2), // ENOENT
    }
}

pub fn sys_getxattr(
    path: *const u8,
    name: *const u8,
    value: *mut u8,
    size: usize,
) -> SyscallResult {
    let inode = match resolve_xattr_path(path, true) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let name_str = match unsafe { crate::syscall::validation::copy_string_from_user(name) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };
    let ino = inode.inode().ino;
    let data = match crate::fs::xattr::get_xattr(ino, &name_str) {
        Some(d) => d,
        None => return -61, // ENODATA
    };

    if size == 0 {
        return data.len() as SyscallResult;
    }
    if size < data.len() {
        return -34; // ERANGE
    }
    if value.is_null() || validate_user_ptr_write(value, data.len()).is_err() {
        return Errno::EFAULT.into();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), value, data.len());
    }
    data.len() as SyscallResult
}

pub fn sys_lgetxattr(
    path: *const u8,
    name: *const u8,
    value: *mut u8,
    size: usize,
) -> SyscallResult {
    let inode = match resolve_xattr_path(path, false) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let name_str = match unsafe { crate::syscall::validation::copy_string_from_user(name) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };
    let ino = inode.inode().ino;
    let data = match crate::fs::xattr::get_xattr(ino, &name_str) {
        Some(d) => d,
        None => return -61, // ENODATA
    };

    if size == 0 {
        return data.len() as SyscallResult;
    }
    if size < data.len() {
        return -34; // ERANGE
    }
    if value.is_null() || validate_user_ptr_write(value, data.len()).is_err() {
        return Errno::EFAULT.into();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), value, data.len());
    }
    data.len() as SyscallResult
}

pub fn sys_fgetxattr(fd: i32, name: *const u8, value: *mut u8, size: usize) -> SyscallResult {
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };
    let name_str = match unsafe { crate::syscall::validation::copy_string_from_user(name) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };
    let ino = file_desc.inode.inode().ino;
    let data = match crate::fs::xattr::get_xattr(ino, &name_str) {
        Some(d) => d,
        None => return -61, // ENODATA
    };

    if size == 0 {
        return data.len() as SyscallResult;
    }
    if size < data.len() {
        return -34; // ERANGE
    }
    if value.is_null() || validate_user_ptr_write(value, data.len()).is_err() {
        return Errno::EFAULT.into();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), value, data.len());
    }
    data.len() as SyscallResult
}

pub fn sys_setxattr(
    path: *const u8,
    name: *const u8,
    value: *const u8,
    size: usize,
    flags: i32,
) -> SyscallResult {
    let inode = match resolve_xattr_path(path, true) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let name_str = match unsafe { crate::syscall::validation::copy_string_from_user(name) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };
    if size > 65536 {
        return Errno::E2BIG.into();
    }
    let val_bytes = if size > 0 {
        if value.is_null() || !validate_user_ptr(value, size) {
            return Errno::EFAULT.into();
        }
        let mut buf = alloc::vec![0u8; size];
        unsafe {
            core::ptr::copy_nonoverlapping(value, buf.as_mut_ptr(), size);
        }
        buf
    } else {
        alloc::vec::Vec::new()
    };

    let ino = inode.inode().ino;
    match crate::fs::xattr::set_xattr(ino, &name_str, &val_bytes, flags) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

pub fn sys_lsetxattr(
    path: *const u8,
    name: *const u8,
    value: *const u8,
    size: usize,
    flags: i32,
) -> SyscallResult {
    let inode = match resolve_xattr_path(path, false) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let name_str = match unsafe { crate::syscall::validation::copy_string_from_user(name) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };
    if size > 65536 {
        return Errno::E2BIG.into();
    }
    let val_bytes = if size > 0 {
        if value.is_null() || !validate_user_ptr(value, size) {
            return Errno::EFAULT.into();
        }
        let mut buf = alloc::vec![0u8; size];
        unsafe {
            core::ptr::copy_nonoverlapping(value, buf.as_mut_ptr(), size);
        }
        buf
    } else {
        alloc::vec::Vec::new()
    };

    let ino = inode.inode().ino;
    match crate::fs::xattr::set_xattr(ino, &name_str, &val_bytes, flags) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

pub fn sys_fsetxattr(
    fd: i32,
    name: *const u8,
    value: *const u8,
    size: usize,
    flags: i32,
) -> SyscallResult {
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };
    let name_str = match unsafe { crate::syscall::validation::copy_string_from_user(name) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };
    if size > 65536 {
        return Errno::E2BIG.into();
    }
    let val_bytes = if size > 0 {
        if value.is_null() || !validate_user_ptr(value, size) {
            return Errno::EFAULT.into();
        }
        let mut buf = alloc::vec![0u8; size];
        unsafe {
            core::ptr::copy_nonoverlapping(value, buf.as_mut_ptr(), size);
        }
        buf
    } else {
        alloc::vec::Vec::new()
    };

    let ino = file_desc.inode.inode().ino;
    match crate::fs::xattr::set_xattr(ino, &name_str, &val_bytes, flags) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

pub fn sys_listxattr(path: *const u8, list: *mut u8, size: usize) -> SyscallResult {
    let inode = match resolve_xattr_path(path, true) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let ino = inode.inode().ino;
    let names = crate::fs::xattr::list_xattr(ino);
    if size == 0 {
        return names.len() as SyscallResult;
    }
    if size < names.len() {
        return -34; // ERANGE
    }
    if list.is_null() || validate_user_ptr_write(list, names.len()).is_err() {
        return Errno::EFAULT.into();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(names.as_ptr(), list, names.len());
    }
    names.len() as SyscallResult
}

pub fn sys_llistxattr(path: *const u8, list: *mut u8, size: usize) -> SyscallResult {
    let inode = match resolve_xattr_path(path, false) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let ino = inode.inode().ino;
    let names = crate::fs::xattr::list_xattr(ino);
    if size == 0 {
        return names.len() as SyscallResult;
    }
    if size < names.len() {
        return -34; // ERANGE
    }
    if list.is_null() || validate_user_ptr_write(list, names.len()).is_err() {
        return Errno::EFAULT.into();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(names.as_ptr(), list, names.len());
    }
    names.len() as SyscallResult
}

pub fn sys_flistxattr(fd: i32, list: *mut u8, size: usize) -> SyscallResult {
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };
    let ino = file_desc.inode.inode().ino;
    let names = crate::fs::xattr::list_xattr(ino);
    if size == 0 {
        return names.len() as SyscallResult;
    }
    if size < names.len() {
        return -34; // ERANGE
    }
    if list.is_null() || validate_user_ptr_write(list, names.len()).is_err() {
        return Errno::EFAULT.into();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(names.as_ptr(), list, names.len());
    }
    names.len() as SyscallResult
}

pub fn sys_removexattr(path: *const u8, name: *const u8) -> SyscallResult {
    let inode = match resolve_xattr_path(path, true) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let name_str = match unsafe { crate::syscall::validation::copy_string_from_user(name) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };
    let ino = inode.inode().ino;
    match crate::fs::xattr::remove_xattr(ino, &name_str) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

pub fn sys_lremovexattr(path: *const u8, name: *const u8) -> SyscallResult {
    let inode = match resolve_xattr_path(path, false) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let name_str = match unsafe { crate::syscall::validation::copy_string_from_user(name) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };
    let ino = inode.inode().ino;
    match crate::fs::xattr::remove_xattr(ino, &name_str) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

pub fn sys_fremovexattr(fd: i32, name: *const u8) -> SyscallResult {
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };
    let name_str = match unsafe { crate::syscall::validation::copy_string_from_user(name) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };
    let ino = file_desc.inode.inode().ino;
    match crate::fs::xattr::remove_xattr(ino, &name_str) {
        Ok(()) => 0,
        Err(e) => e,
    }
}
