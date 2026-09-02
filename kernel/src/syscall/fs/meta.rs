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
    let cwd = task_arc.lock().cwd.clone();

    let cwd_bytes = cwd.as_bytes();
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
        st_dev: 0,
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
    kprintln!(
        "[syscall] rename(\"{}\" -> \"{}\")",
        resolved_old,
        resolved_new
    );

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

    sys_link_with_resolved_paths(resolved_old, resolved_new, flags)
}

/// Core hardlink logic with already resolved paths.
pub fn sys_link_with_resolved_paths(
    resolved_old: String,
    resolved_new: String,
    flags: i32,
) -> SyscallResult {
    kprintln!(
        "[syscall] link(\"{}\" -> \"{}\")",
        resolved_old,
        resolved_new
    );

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
///
/// Stub: marks all fds as having POLLIN|POLLOUT ready and returns immediately.
/// A real implementation would block in the scheduler until events fire.
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
    let timeout_ticks = if timeout > 0 {
        Some((timeout as u64 + 9) / 10)
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
                    pfd.revents = 0x0008; // POLLERR
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

        // Handle signals checking (break with EINTR if a signal is pending)
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

        // Sleep on wait queue with timeout instead of burning CPU in yield_now()
        if let Some(limit) = timeout_ticks {
            crate::fs::epoll::add_sleep_timeout(current_pid, start_ticks + limit);
        }
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
            let wait_queue = {
                let mut task = task_arc.lock();
                task.state = crate::process::task::TaskState::Blocked;
                task.child_wait_queue.clone()
            };
            wait_queue.register(current_pid);
            crate::process::scheduler::schedule();
            wait_queue.remove(current_pid);
        }
        if timeout_ticks.is_some() {
            crate::fs::epoll::remove_sleep_timeout(current_pid);
        }
    }
}

/// `chmod(pathname, mode)` — Change file permissions.
pub fn sys_chmod(pathname: *const u8, mode: u32) -> SyscallResult {
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

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

    crate::fs::vfs::mount(target_path, fs_instance);
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
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
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
