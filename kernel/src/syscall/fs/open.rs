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
                    let mode_val = if _mode == 0 { 0o644 } else { _mode };
                    let file_mode = ((mode_val & 0x0FFF) & !umask) as u16;

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

    if proc_fd::current_task_close_fd(fd) {
        if let Some((pid, ino)) = lock_cleanup_info {
            crate::syscall::fs::io::release_fcntl_locks_for_pid_and_ino(pid, ino);
        }
        0
    } else {
        Errno::EBADF.into()
    }
}

/// `close_range(first, last, flags)` — Close a range of file descriptors.
pub fn sys_close_range(first: u32, last: u32, _flags: u32) -> SyscallResult {
    if first > last {
        return Errno::EINVAL.into();
    }
    let end = last.min(1024);
    for fd in first..=end {
        let _ = sys_close(fd as i32);
    }
    0
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

/// `creat(pathname, mode)` — Create a new file or rewrite an existing one.
pub fn sys_creat(pathname: *const u8, mode: u32) -> SyscallResult {
    // O_CREAT (0x40) | O_WRONLY (0x01) | O_TRUNC (0x200)
    sys_open(pathname, 0x01 | 0x40 | 0x200, mode)
}

/// `fchdir(fd)` — Change working directory using file descriptor.
pub fn sys_fchdir(fd: i32) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };
    if !file_desc.inode.inode().is_dir() {
        return Errno::ENOTDIR.into();
    }
    let path = match file_desc.path {
        Some(ref p) => p.clone(),
        None => return 0,
    };
    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        task_arc.lock().cwd = path;
    }
    0
}

/// Linux `open_how` structure for `openat2`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OpenHow {
    pub flags: u64,
    pub mode: u64,
    pub resolve: u64,
}

/// `openat2(dfd, filename, how, usize)` — Open a file relative to a directory fd.
pub fn sys_openat2(
    dfd: i32,
    filename: *const u8,
    how: *const OpenHow,
    usize_: usize,
) -> SyscallResult {
    if how.is_null() || usize_ < core::mem::size_of::<OpenHow>() {
        return Errno::EINVAL.into();
    }
    if !crate::syscall::validation::validate_user_ptr(
        how as *const u8,
        core::mem::size_of::<OpenHow>(),
    ) {
        return Errno::EFAULT.into();
    }

    let how_val = unsafe { core::ptr::read(how) };
    sys_openat(dfd, filename, how_val.flags as i32, how_val.mode as u32)
}

/// `chroot(path)` — change root directory.
///
/// Sets the task's filesystem jail root so that all subsequent absolute path
/// lookups are interpreted relative to `path` and `..` traversal is clamped
/// at this boundary.
///
/// Requires `euid == 0`.
pub fn sys_chroot(path_ptr: *const u8) -> SyscallResult {
    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        if task_arc.lock().euid != 0 {
            return Errno::EPERM.into();
        }
    }
    let path = match unsafe { copy_string_from_user(path_ptr) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let resolved = crate::fs::vfs::resolve_relative_path(&path);
    let inode = match crate::fs::vfs::lookup(&resolved) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };
    if !inode.inode().is_dir() {
        return Errno::ENOTDIR.into();
    }
    // Update the jail root so that future path resolution is clamped here.
    // Also update cwd to be "/" (relative to the new root).
    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        let mut task = task_arc.lock();
        task.fs_ctx.write().root = resolved.clone();
        // After chroot, cwd is "/" inside the new root (= resolved on the host).
        task.cwd = resolved;
    }
    0
}

/// `pivot_root(new_root, put_old)` — change the root mount.
///
/// Atomically swaps the mount namespace entries so that `new_root` becomes
/// the new root mount (`/`) and the old root is moved to `put_old`.
///
/// The task must have a **private** mount namespace (obtained by calling
/// `unshare(CLONE_NEWNS)` first); otherwise `EINVAL` is returned.
///
/// After `pivot_root`, the caller should `umount2(put_old, MNT_DETACH)` to
/// fully unhook the host root from the container's view.
pub fn sys_pivot_root(new_root_ptr: *const u8, put_old_ptr: *const u8) -> SyscallResult {
    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        if task_arc.lock().euid != 0 {
            return Errno::EPERM.into();
        }
    }
    let new_root = match unsafe { copy_string_from_user(new_root_ptr) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let put_old = match unsafe { copy_string_from_user(put_old_ptr) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let new_res = crate::fs::vfs::resolve_relative_path(&new_root);
    let old_res = crate::fs::vfs::resolve_relative_path(&put_old);

    // Validate that both paths exist.
    if crate::fs::vfs::lookup(&new_res).is_none() || crate::fs::vfs::lookup(&old_res).is_none() {
        return Errno::ENOENT.into();
    }

    // Retrieve the task's mount namespace Arc.
    let (ns_arc, initial_ns_id) = match crate::process::scheduler::get_task_arc(current_pid) {
        Some(task_arc) => {
            let task = task_arc.lock();
            let ns_arc = task.fs_ctx.read().mount_ns.clone();
            let initial_id = crate::fs::namespace::INITIAL_MOUNT_NS.read().id;
            (ns_arc, initial_id)
        }
        None => return Errno::ESRCH.into(),
    };

    // Refuse to operate on the shared global namespace — the task must have
    // already called unshare(CLONE_NEWNS) to get a private namespace.
    if ns_arc.read().id == initial_ns_id {
        return Errno::EINVAL.into();
    }

    {
        let mut ns = ns_arc.write();

        // Retrieve the filesystem that is currently mounted at new_root.
        let new_root_fs = match ns.mounts.get(&new_res).cloned() {
            Some(fs) => fs,
            None => {
                // new_root is a directory but not a distinct mount point.
                // Bind it to itself (mount the underlying FS at put_old).
                // Fall through with the root FS.
                match ns.mounts.get("/").cloned() {
                    Some(fs) => fs,
                    None => return Errno::EINVAL.into(),
                }
            }
        };

        // The current root FS.
        let old_root_fs = match ns.mounts.get("/").cloned() {
            Some(fs) => fs,
            None => return Errno::EINVAL.into(),
        };

        // Move the old root to put_old, install new_root as the new root.
        ns.mounts.remove("/");
        ns.mounts.remove(&new_res);
        ns.mounts.insert(old_res.clone(), old_root_fs);
        ns.mounts.insert(String::from("/"), new_root_fs);
    }

    // Update the task's jail root and cwd to the new root.
    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        let mut task = task_arc.lock();
        task.fs_ctx.write().root = String::from("/");
        task.cwd = String::from("/");
    }

    crate::kprintln!(
        "[namespace] pivot_root: {} -> /, old root at {}",
        new_res,
        old_res
    );
    0
}

/// Linux magic numbers for reboot(2).
pub const LINUX_REBOOT_MAGIC1: u32 = 0xfee1dead;
pub const LINUX_REBOOT_MAGIC2: u32 = 672274793;
pub const LINUX_REBOOT_MAGIC2A: u32 = 85072278;
pub const LINUX_REBOOT_MAGIC2B: u32 = 369367448;
pub const LINUX_REBOOT_MAGIC2C: u32 = 537993216;

pub const LINUX_REBOOT_CMD_RESTART: u32 = 0x01234567;
pub const LINUX_REBOOT_CMD_HALT: u32 = 0xcdef0123;
pub const LINUX_REBOOT_CMD_CAD_ON: u32 = 0x89abcdef;
pub const LINUX_REBOOT_CMD_CAD_OFF: u32 = 0x00000000;
pub const LINUX_REBOOT_CMD_POWER_OFF: u32 = 0x4321fedc;
pub const LINUX_REBOOT_CMD_RESTART2: u32 = 0xa1b2c3d4;

/// `reboot(magic1, magic2, cmd, arg)` — reboot or enable/disable Ctrl-Alt-Del.
pub fn sys_reboot(magic1: u32, magic2: u32, cmd: u32, _arg: *const u8) -> SyscallResult {
    if magic1 != LINUX_REBOOT_MAGIC1
        || (magic2 != LINUX_REBOOT_MAGIC2
            && magic2 != LINUX_REBOOT_MAGIC2A
            && magic2 != LINUX_REBOOT_MAGIC2B
            && magic2 != LINUX_REBOOT_MAGIC2C)
    {
        return Errno::EINVAL.into();
    }
    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        if task_arc.lock().euid != 0 {
            return Errno::EPERM.into();
        }
    }

    match cmd {
        LINUX_REBOOT_CMD_CAD_ON | LINUX_REBOOT_CMD_CAD_OFF => 0,
        LINUX_REBOOT_CMD_RESTART | LINUX_REBOOT_CMD_RESTART2 => {
            crate::kprintln!("[kernel] System restart requested via reboot()");
            // Triple fault / 8042 keyboard controller reset
            // SAFETY: Standard x86 8042 reset port access
            unsafe {
                use x86_64::instructions::port::Port;
                let mut port = Port::<u8>::new(0x64);
                port.write(0xFE);
            }
            0
        }
        LINUX_REBOOT_CMD_HALT | LINUX_REBOOT_CMD_POWER_OFF => {
            crate::kprintln!("[kernel] System power off requested via reboot()");
            // Allow pending PTY router and serial queues to drain to console
            for _ in 0..100 {
                crate::process::scheduler::yield_now();
            }
            // QEMU / ACPI poweroff & ISA debug exit
            // SAFETY: Standard QEMU poweroff and debug-exit port write
            unsafe {
                use x86_64::instructions::port::Port;
                let mut p = Port::<u16>::new(0x604);
                p.write(0x2000);

                let mut debug_exit = Port::<u32>::new(0xf4);
                debug_exit.write(0x10); // QEMU exit code 33 ((0x10 << 1) | 1)
            }
            0
        }
        _ => Errno::EINVAL.into(),
    }
}
