//! File system syscalls — open, read, write, close, stat.
//!
//! These implement the core POSIX file I/O operations that form the
//! foundation of Unix's "everything is a file" philosophy.
//!
//! All file descriptors are looked up in the current task's `fd_table`
//! via `process::fd` helpers. VFS path resolution is used for `open`.

use super::{Errno, SyscallResult};
use crate::kprintln;
use crate::process::fd as proc_fd;

// ── Helper: copy a null-terminated C string from user virtual memory ──────────

/// Enforce that a user-space pointer range [ptr, ptr + size) is valid.
///
/// 1. Must lie strictly below 0x0000_7FFF_FFFF_FFFF.
/// 2. Must not wrap around.
/// 3. Every page in the range must be mapped in the active page directory.
pub fn validate_user_ptr(ptr: *const u8, size: usize) -> bool {
    if ptr.is_null() {
        return false;
    }
    let start = ptr as u64;
    let end = match start.checked_add(size as u64) {
        Some(e) => e,
        None => return false,
    };
    if end > 0x0000_7FFF_FFFF_FFFF {
        return false;
    }
    if size == 0 {
        return true;
    }
    let page_size = 4096;
    let start_page = start & !(page_size - 1);
    let end_page = (end + page_size - 1) & !(page_size - 1);

    let mut curr = start_page;
    while curr < end_page {
        if crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(curr)).is_none() {
            return false;
        }
        curr += page_size;
    }
    true
}

/// Copy a null-terminated string from user-space virtual address `ptr`.
///
/// Validates that each byte's page pointer resides in user memory and is mapped
/// in the active page table before dereferencing it, preventing unmapped page faults.
unsafe fn copy_string_from_user(ptr: *const u8) -> Option<alloc::string::String> {
    if ptr.is_null() || (ptr as u64) > 0x0000_7FFF_FFFF_FFFF {
        return None;
    }
    let mut result = alloc::string::String::new();
    let mut p = ptr;
    loop {
        let addr = p as u64;
        if addr > 0x0000_7FFF_FFFF_FFFF {
            return None;
        }
        let page_base = addr & !4095;
        if crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(page_base)).is_none() {
            return None;
        }
        let byte = unsafe { p.read_volatile() };
        if byte == 0 {
            break;
        }
        result.push(byte as char);
        p = unsafe { p.add(1) };
        if result.len() > 4096 {
            return None;
        }
    }
    Some(result)
}

/// Public wrapper used by `syscall::process` for execve path resolution.
pub(crate) unsafe fn copy_string_from_user_pub(ptr: *const u8) -> Option<alloc::string::String> {
    unsafe { copy_string_from_user(ptr) }
}


// ── Syscall implementations ───────────────────────────────────────────────────

/// `read(fd, buf, count)` — Read from a file descriptor.
///
/// Reads up to `count` bytes from file descriptor `fd` into user buffer `buf`.
/// Returns the number of bytes read, or a negative errno on error.
pub fn sys_read(fd: i32, buf: *mut u8, count: usize) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }
    if buf.is_null() || count == 0 {
        return 0;
    }
    if !validate_user_ptr(buf as *const u8, count) {
        return Errno::EFAULT.into();
    }

    let inode = match proc_fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };

    let is_pipe = inode.inode().file_type == crate::fs::inode::FileType::Pipe;
    if is_pipe {
        crate::kprintln!("[syscall] sys_read on pipe fd {}", fd);
    }

    let offset = proc_fd::get_fd_offset(fd).unwrap_or(0);
    let slice = unsafe { core::slice::from_raw_parts_mut(buf, count) };

    match inode.read(offset, slice) {
        Ok(n) => {
            if is_pipe {
                crate::kprintln!("[syscall] sys_read on pipe fd {} returned {} bytes", fd, n);
            }
            proc_fd::set_fd_offset(fd, offset + n as u64);
            n as SyscallResult
        }
        Err(e) => {
            if is_pipe {
                crate::kprintln!("[syscall] sys_read on pipe fd {} failed with error {}", fd, e);
            }
            e as SyscallResult
        }
    }
}

/// `write(fd, buf, count)` — Write to a file descriptor.
///
/// Writes up to `count` bytes from `buf` to file descriptor `fd`.
/// Returns the number of bytes written, or a negative errno on error.
pub fn sys_write(fd: i32, buf: *const u8, count: usize) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }
    if buf.is_null() || count == 0 {
        return 0;
    }
    if !validate_user_ptr(buf, count) {
        return Errno::EFAULT.into();
    }

    let inode = match proc_fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };

    let is_pipe = inode.inode().file_type == crate::fs::inode::FileType::Pipe;
    if is_pipe {
        crate::kprintln!("[syscall] sys_write on pipe fd {} count {}", fd, count);
    }

    let offset = proc_fd::get_fd_offset(fd).unwrap_or(0);
    let slice = unsafe { core::slice::from_raw_parts(buf, count) };

    match inode.write(offset, slice) {
        Ok(n) => {
            if is_pipe {
                crate::kprintln!("[syscall] sys_write on pipe fd {} returned {} bytes written", fd, n);
            }
            proc_fd::set_fd_offset(fd, offset + n as u64);
            n as SyscallResult
        }
        Err(e) => {
            if is_pipe {
                crate::kprintln!("[syscall] sys_write on pipe fd {} failed with error {}", fd, e);
            }
            e as SyscallResult
        }
    }
}

/// `open(pathname, flags, mode)` — Open a file.
///
/// Resolves `pathname` through the VFS, allocates a file descriptor in the
/// current task's `fd_table`, and returns the new fd number.
pub fn sys_open(pathname: *const u8, flags: i32, _mode: u32) -> SyscallResult {
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    kprintln!("[syscall] open(\"{}\", flags={:#x})", resolved_path, flags);

    let flags_u32 = flags as u32;
    let exists = crate::fs::vfs::lookup(&resolved_path);

    let inode = match exists {
        Some(i) => {
            // If O_CREAT and O_EXCL are both set, return EEXIST
            if (flags_u32 & crate::fs::file::OpenFlags::O_CREAT != 0)
                && (flags_u32 & crate::fs::file::OpenFlags::O_EXCL != 0)
            {
                return Errno::EEXIST.into();
            }
            // If O_DIRECTORY is set and it is not a directory, return ENOTDIR
            if (flags_u32 & crate::fs::file::OpenFlags::O_DIRECTORY != 0)
                && !i.inode().is_dir()
            {
                return Errno::ENOTDIR.into();
            }
            // If opened for writing and the inode is a directory, return EISDIR
            if i.inode().is_dir() && crate::fs::file::OpenFlags(flags_u32).is_writable() {
                return Errno::EISDIR.into();
            }
            // If O_TRUNC is set and it is a regular file, truncate it to 0 size
            if (flags_u32 & crate::fs::file::OpenFlags::O_TRUNC != 0)
                && i.inode().is_file()
            {
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
                match parent_inode.create(name, crate::fs::inode::FileType::Regular) {
                    Some(new_i) => new_i,
                    None => return Errno::EACCES.into(),
                }
            } else {
                return Errno::ENOENT.into();
            }
        }
    };

    match proc_fd::current_task_alloc_fd(inode) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}

/// `close(fd)` — Close a file descriptor.
pub fn sys_close(fd: i32) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }
    let is_pipe = proc_fd::current_task_read_fd(fd)
        .map(|i| i.inode().file_type == crate::fs::inode::FileType::Pipe)
        .unwrap_or(false);
    if is_pipe {
        crate::kprintln!("[syscall] sys_close on pipe fd {}", fd);
    }
    if proc_fd::current_task_close_fd(fd) {
        0
    } else {
        Errno::EBADF.into()
    }
}

/// `stat(pathname, statbuf)` — Get file status.
pub fn sys_stat(_pathname: *const u8, _statbuf: *mut u8) -> SyscallResult {
    // TODO: fill in stat buffer with inode metadata
    Errno::ENOSYS.into()
}

/// `lseek(fd, offset, whence)` — Reposition file offset.
pub fn sys_lseek(fd: i32, offset: i64, whence: i32) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }
    let inode = match proc_fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };

    let current_offset = proc_fd::get_fd_offset(fd).unwrap_or(0) as i64;
    let size = inode.inode().size as i64;

    let new_offset = match whence {
        0 => offset, // SEEK_SET
        1 => current_offset + offset, // SEEK_CUR
        2 => size + offset, // SEEK_END
        _ => return Errno::EINVAL.into(),
    };

    if new_offset < 0 {
        return Errno::EINVAL.into();
    }

    proc_fd::set_fd_offset(fd, new_offset as u64);
    new_offset as SyscallResult
}

/// `dup(fd)` — Duplicate a file descriptor.
pub fn sys_dup(fd: i32) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }
    match proc_fd::current_task_dup_fd(fd) {
        Some(newfd) => newfd as SyscallResult,
        None => Errno::EBADF.into(),
    }
}

/// `dup2(oldfd, newfd)` — Duplicate a file descriptor onto a specific index.
pub fn sys_dup2(oldfd: i32, newfd: i32) -> SyscallResult {
    if oldfd < 0 || newfd < 0 || newfd >= 1024 {
        return Errno::EBADF.into();
    }
    let is_pipe = proc_fd::current_task_read_fd(oldfd)
        .map(|i| i.inode().file_type == crate::fs::inode::FileType::Pipe)
        .unwrap_or(false);
    if is_pipe {
        crate::kprintln!("[syscall] sys_dup2(oldfd={}, newfd={}) on pipe", oldfd, newfd);
    }
    match proc_fd::current_task_dup2_fd(oldfd, newfd) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EBADF.into(),
    }
}

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
            crate::fs::inode::FileType::Directory => 4,
            crate::fs::inode::FileType::Regular => 8,
            crate::fs::inode::FileType::CharDevice => 2,
            crate::fs::inode::FileType::BlockDevice => 6,
            crate::fs::inode::FileType::Pipe => 1,
            crate::fs::inode::FileType::Socket => 12,
            crate::fs::inode::FileType::Symlink => 10,
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

/// `pipe(pipefds)` — Create a unidirectional pipe.
pub fn sys_pipe(pipefds: *mut i32) -> SyscallResult {
    if pipefds.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(pipefds as *const u8, 8) {
        return Errno::EFAULT.into();
    }

    // Create the pipe VFS endpoints
    let (reader, writer) = crate::fs::pipe::make_pipe();

    // Allocate file descriptors
    let fd0 = match proc_fd::current_task_alloc_fd(reader) {
        Some(fd) => fd,
        None => return Errno::EMFILE.into(),
    };

    let fd1 = match proc_fd::current_task_alloc_fd(writer) {
        Some(fd) => fd,
        None => {
            // Roll back fd0
            proc_fd::current_task_close_fd(fd0);
            return Errno::EMFILE.into();
        }
    };

    // Write to user space
    unsafe {
        pipefds.write(fd0);
        pipefds.add(1).write(fd1);
    }

    kprintln!("[syscall] pipe() -> fds: [{}, {}]", fd0, fd1);
    0 // Success
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

    let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
    let scheduler = match sched_lock.as_mut() {
        Some(s) => s,
        None => return Errno::ESRCH.into(),
    };
    let task = match scheduler.get_task_mut(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };

    task.cwd = resolved_path;
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

    let cwd = {
        let sched_lock = crate::process::scheduler::SCHEDULER.lock();
        let scheduler = match sched_lock.as_ref() {
            Some(s) => s,
            None => return 0,
        };
        let task = match scheduler.get_task(current_pid) {
            Some(t) => t,
            None => return 0,
        };
        task.cwd.clone()
    };

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

fn file_type_to_st_mode(file_type: crate::fs::inode::FileType) -> u32 {
    match file_type {
        crate::fs::inode::FileType::Regular => 0o100000,
        crate::fs::inode::FileType::Directory => 0o040000,
        crate::fs::inode::FileType::CharDevice => 0o020000,
        crate::fs::inode::FileType::BlockDevice => 0o060000,
        crate::fs::inode::FileType::Pipe => 0o010000,
        crate::fs::inode::FileType::Symlink => 0o120000,
        crate::fs::inode::FileType::Socket => 0o140000,
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

    let resolved_path = if raw_path.starts_with('/') {
        raw_path
    } else if dfd == -100 { // AT_FDCWD
        crate::fs::vfs::resolve_relative_path(&raw_path)
    } else {
        crate::fs::vfs::resolve_relative_path(&raw_path)
    };

    let inode_ops = match crate::fs::vfs::lookup(&resolved_path) {
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
pub fn sys_faccessat(
    dfd: i32,
    pathname: *const u8,
    mode: i32,
    _flags: i32,
) -> SyscallResult {
    if pathname.is_null() {
        return Errno::EFAULT.into();
    }
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_path = if raw_path.starts_with('/') {
        raw_path
    } else if dfd == -100 { // AT_FDCWD
        crate::fs::vfs::resolve_relative_path(&raw_path)
    } else {
        crate::fs::vfs::resolve_relative_path(&raw_path)
    };

    let inode_ops = match crate::fs::vfs::lookup(&resolved_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    let inode = inode_ops.inode();
    if mode != 0 {
        if (mode & 4) != 0 && !inode.permissions.owner_read() {
            return Errno::EACCES.into();
        }
        if (mode & 2) != 0 && !inode.permissions.owner_write() {
            return Errno::EACCES.into();
        }
        if (mode & 1) != 0 && !inode.permissions.owner_exec() {
            return Errno::EACCES.into();
        }
    }

    0
}

/// `fcntl(fd, cmd, arg)` — File control.
pub fn sys_fcntl(fd: i32, cmd: i32, arg: u64) -> SyscallResult {
    match cmd {
        0 => { // F_DUPFD
            let inode_ops = match proc_fd::current_task_read_fd(fd) {
                Some(i) => i,
                None => return Errno::EBADF.into(),
            };
            let start_fd = arg as i32;
            if start_fd < 0 {
                return Errno::EINVAL.into();
            }
            
            let current_pid = match crate::process::scheduler::current_pid() {
                Some(p) => p,
                None => return Errno::ESRCH.into(),
            };
            let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
            let scheduler = match sched_lock.as_mut() {
                Some(s) => s,
                None => return Errno::ESRCH.into(),
            };
            let task = match scheduler.get_task_mut(current_pid) {
                Some(t) => t,
                None => return Errno::ESRCH.into(),
            };
            
            let mut new_fd = start_fd;
            while (new_fd as usize) < task.fd_table.len() && task.fd_table[new_fd as usize].is_some() {
                new_fd += 1;
            }
            
            if (new_fd as usize) >= task.fd_table.len() {
                task.fd_table.resize(new_fd as usize + 1, None);
            }
            task.fd_table[new_fd as usize] = Some(inode_ops);
            
            if (new_fd as usize) >= task.fd_offsets.len() {
                task.fd_offsets.resize(new_fd as usize + 1, 0);
            }
            let old_offset = task.fd_offsets[fd as usize];
            task.fd_offsets[new_fd as usize] = old_offset;
            
            kprintln!("[syscall] fcntl(fd={}, F_DUPFD, arg={}) -> {}", fd, arg, new_fd);
            new_fd as i64
        }
        1 => { // F_GETFD
            0
        }
        2 => { // F_SETFD
            0
        }
        3 => { // F_GETFL
            2 // O_RDWR
        }
        4 => { // F_SETFL
            0
        }
        _ => {
            kprintln!("[syscall] fcntl(fd={}, cmd={}, arg={}) -> ENOSYS", fd, cmd, arg);
            Errno::ENOSYS.into()
        }
    }
}

/// `mkdir(pathname, mode)` — Create a directory.
pub fn sys_mkdir(pathname: *const u8, _mode: u32) -> SyscallResult {
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    kprintln!("[syscall] mkdir(\"{}\")", resolved_path);

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
    if parent_inode.inode().file_type != crate::fs::inode::FileType::Directory {
        return Errno::ENOTDIR.into();
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
    kprintln!("[syscall] rmdir(\"{}\")", resolved_path);

    // Split resolved_path into parent directory and base name
    let (parent_path, name) = crate::fs::path::split_path(&resolved_path);

    // Lookup parent directory
    let parent_inode = match crate::fs::vfs::lookup(parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    // Make sure parent is a directory
    if parent_inode.inode().file_type != crate::fs::inode::FileType::Directory {
        return Errno::ENOTDIR.into();
    }

    match parent_inode.rmdir(name) {
        Ok(_) => 0,
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
    kprintln!("[syscall] unlink(\"{}\")", resolved_path);

    // Split resolved_path into parent directory and base name
    let (parent_path, name) = crate::fs::path::split_path(&resolved_path);

    // Lookup parent directory
    let parent_inode = match crate::fs::vfs::lookup(parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    // Make sure parent is a directory
    if parent_inode.inode().file_type != crate::fs::inode::FileType::Directory {
        return Errno::ENOTDIR.into();
    }

    match parent_inode.unlink(name) {
        Ok(_) => 0,
        Err(e) => e as SyscallResult,
    }
}




