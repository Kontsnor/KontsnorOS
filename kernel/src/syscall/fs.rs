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

    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    let is_pipe = file_desc.inode.inode().file_type == crate::fs::inode::FileType::Pipe;
    if is_pipe {
        let pid_str = crate::process::scheduler::current_pid().map(|p| p.as_u64()).unwrap_or(0);
        crate::kprintln!("[syscall pid={}] sys_read on pipe fd {}", pid_str, fd);
    }

    let mut kernel_buf = alloc::vec![0u8; count];

    match file_desc.read(&mut kernel_buf) {
        Ok(n) => {
            unsafe {
                core::ptr::copy_nonoverlapping(kernel_buf.as_ptr(), buf, n);
            }
            if is_pipe {
                crate::kprintln!("[syscall] sys_read on pipe fd {} returned {} bytes", fd, n);
            }
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

    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    let is_pipe = file_desc.inode.inode().file_type == crate::fs::inode::FileType::Pipe;
    if is_pipe {
        let pid_str = crate::process::scheduler::current_pid().map(|p| p.as_u64()).unwrap_or(0);
        crate::kprintln!("[syscall pid={}] sys_write on pipe fd {} count {}", pid_str, fd, count);
    }

    let mut kernel_buf = alloc::vec![0u8; count];
    unsafe {
        core::ptr::copy_nonoverlapping(buf, kernel_buf.as_mut_ptr(), count);
    }

    match file_desc.write(&kernel_buf) {
        Ok(n) => {
            if is_pipe {
                crate::kprintln!("[syscall] sys_write on pipe fd {} returned {} bytes written", fd, n);
            }
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
        }
    };

    match proc_fd::current_task_alloc_fd_with_flags(inode, crate::fs::file::OpenFlags(flags_u32)) {
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
        let pid_str = crate::process::scheduler::current_pid().map(|p| p.as_u64()).unwrap_or(0);
        crate::kprintln!("[syscall pid={}] sys_close on pipe fd {}", pid_str, fd);
    }
    if proc_fd::current_task_close_fd(fd) {
        0
    } else {
        Errno::EBADF.into()
    }
}

/// `lseek(fd, offset, whence)` — Reposition file offset.
pub fn sys_lseek(fd: i32, offset: i64, whence: i32) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    match file_desc.seek(offset, whence) {
        Ok(new_offset) => new_offset as SyscallResult,
        Err(e) => e as SyscallResult,
    }
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
        let pid_str = crate::process::scheduler::current_pid().map(|p| p.as_u64()).unwrap_or(0);
        crate::kprintln!("[syscall pid={}] sys_dup2(oldfd={}, newfd={}) on pipe", pid_str, oldfd, newfd);
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

    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, true) {
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
            
            let file_desc = match task.fd_table.get(fd as usize) {
                Some(Some(desc)) => desc.clone(),
                _ => return Errno::EBADF.into(),
            };
            
            *file_desc.ref_count.lock() += 1;
            
            let mut new_fd = start_fd;
            while (new_fd as usize) < task.fd_table.len() && task.fd_table[new_fd as usize].is_some() {
                new_fd += 1;
            }
            
            if (new_fd as usize) >= task.fd_table.len() {
                task.fd_table.resize(new_fd as usize + 1, None);
            }
            task.fd_table[new_fd as usize] = Some(file_desc);
            
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
            let current_pid = match crate::process::scheduler::current_pid() {
                Some(p) => p,
                None => return Errno::ESRCH.into(),
            };
            let sched_lock = crate::process::scheduler::SCHEDULER.lock();
            if let Some(ref scheduler) = *sched_lock {
                if let Some(task) = scheduler.get_task(current_pid) {
                    if let Some(Some(desc)) = task.fd_table.get(fd as usize) {
                        return desc.flags.lock().0 as i64;
                    }
                }
            }
            Errno::EBADF.into()
        }
        4 => { // F_SETFL
            let current_pid = match crate::process::scheduler::current_pid() {
                Some(p) => p,
                None => return Errno::ESRCH.into(),
            };
            let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
            if let Some(ref mut scheduler) = *sched_lock {
                if let Some(task) = scheduler.get_task_mut(current_pid) {
                    if let Some(Some(desc)) = task.fd_table.get_mut(fd as usize) {
                        let allowed_flags = crate::fs::file::OpenFlags::O_APPEND | crate::fs::file::OpenFlags::O_NONBLOCK;
                        let mut flags = desc.flags.lock();
                        let old_val = flags.0;
                        flags.0 = (old_val & !allowed_flags) | (arg as u32 & allowed_flags);
                        return 0;
                    }
                }
            }
            Errno::EBADF.into()
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

// ─────────────────────────────────────────────────────────────────────────────
// Additional POSIX FS syscalls required by bash + glibc/musl
// ─────────────────────────────────────────────────────────────────────────────

/// Validate that a user-space write target at `[ptr, ptr+size)` is safe.
///
/// This is the write-variant of `validate_user_ptr`: it must also be mapped
/// and writable (we allow any user-space address below the canonical hole).
pub fn validate_user_ptr_write(ptr: *mut u8, size: usize) -> Result<(), ()> {
    if ptr.is_null() {
        return Err(());
    }
    let start = ptr as u64;
    let end = match start.checked_add(size as u64) {
        Some(e) => e,
        None => return Err(()),
    };
    if end > 0x0000_7FFF_FFFF_FFFF {
        return Err(());
    }
    if size == 0 {
        return Ok(());
    }
    let page_size: u64 = 4096;
    let start_page = start & !(page_size - 1);
    let end_page = (end + page_size - 1) & !(page_size - 1);
    let mut curr = start_page;
    while curr < end_page {
        if crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(curr)).is_none() {
            return Err(());
        }
        curr += page_size;
    }
    Ok(())
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
    if super::DEBUG_SYSCALLS {
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
    if super::DEBUG_SYSCALLS {
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
    kprintln!("[syscall] rename(\"{}\" -> \"{}\")", resolved_old, resolved_new);

    // Split paths into parent + name
    let (old_parent_path, old_name) = crate::fs::path::split_path(&resolved_old);
    let (new_parent_path, new_name) = crate::fs::path::split_path(&resolved_new);

    let old_parent = match crate::fs::vfs::lookup(old_parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    // For now: read the file data, create at new location, remove at old location.
    // This works for regular files in tmpfs; directory rename is not supported.
    let src_inode_ops = match crate::fs::vfs::lookup(&resolved_old) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    let file_size = src_inode_ops.inode().size as usize;
    let mut buf = alloc::vec![0u8; file_size];
    if file_size > 0 {
        let _ = src_inode_ops.read(0, &mut buf);
    }

    // Create file at new location
    let new_parent = match crate::fs::vfs::lookup(new_parent_path) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };
    let new_inode = match new_parent.create(new_name, crate::fs::inode::FileType::Regular) {
        Some(i) => i,
        None => return Errno::ENOSPC.into(),
    };
    if file_size > 0 {
        let _ = new_inode.write(0, &buf);
    }

    // Remove old file
    let _ = old_parent.unlink(old_name);
    0
}

/// `link(oldpath, newpath)` — Create a hard link.
///
/// KontsnorOS does not support hard links; return EPERM.
pub fn sys_link(_oldpath: *const u8, _newpath: *const u8) -> SyscallResult {
    Errno::EPERM.into()
}

/// `readlink(pathname, buf, bufsize)` — Read the value of a symbolic link.
pub fn sys_readlink(pathname: *const u8, buf: *mut u8, bufsize: usize) -> SyscallResult {
    let raw_path = match unsafe { copy_string_from_user(pathname) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };
    let resolved_path = crate::fs::vfs::resolve_relative_path(&raw_path);
    if super::DEBUG_SYSCALLS {
        kprintln!("[syscall] readlink(\"{}\")", resolved_path);
    }

    let inode_ops = match crate::fs::vfs::lookup_follow(&resolved_path, false) {
        Some(i) => i,
        None => return Errno::ENOENT.into(),
    };

    if inode_ops.inode().file_type != crate::fs::inode::FileType::Symlink {
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
    if dirfd == -100 { // AT_FDCWD
        sys_readlink(pathname, buf, bufsize)
    } else {
        if super::DEBUG_SYSCALLS {
            kprintln!("[syscall] sys_readlinkat: only AT_FDCWD is supported currently");
        }
        Errno::ENOSYS.into()
    }
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
    if super::DEBUG_SYSCALLS {
        kprintln!("[syscall] symlink(\"{}\" -> \"{}\")", resolved_linkpath, raw_target);
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
    if parent_inode.inode().file_type != crate::fs::inode::FileType::Directory {
        return Errno::ENOTDIR.into();
    }

    // Create the symlink inode
    let symlink_inode = match parent_inode.create(name, crate::fs::inode::FileType::Symlink) {
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
    if newdirfd == -100 { // AT_FDCWD
        sys_symlink(target, linkpath)
    } else {
        if super::DEBUG_SYSCALLS {
            kprintln!("[syscall] sys_symlinkat: only AT_FDCWD is supported currently");
        }
        Errno::ENOSYS.into()
    }
}

/// `poll` fd event struct.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct PollFd {
    fd:      i32,
    events:  i16,
    revents: i16,
}

/// `poll(fds, nfds, timeout)` — Wait for events on file descriptors.
///
/// Stub: marks all fds as having POLLIN|POLLOUT ready and returns immediately.
/// A real implementation would block in the scheduler until events fire.
pub fn sys_poll(fds: *mut u8, nfds: u64, _timeout: i32) -> SyscallResult {
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

    let mut ready = 0i64;
    for pfd in local_fds.iter_mut() {
        if pfd.fd >= 0 {
            pfd.revents = pfd.events;
            ready += 1;
        } else {
            pfd.revents = 0;
        }
    }

    unsafe {
        core::ptr::copy_nonoverlapping(local_fds.as_ptr(), fds as *mut PollFd, nfds as usize);
    }

    ready as SyscallResult
}

/// `pread64(fd, buf, count, offset)` — Read from a file descriptor at an offset.
///
/// Unlike `read`, this does not change the file's seek position.
pub fn sys_pread64(fd: i32, buf: *mut u8, count: usize, offset: i64) -> SyscallResult {
    if !validate_user_ptr(buf, count) {
        return Errno::EFAULT.into();
    }

    let file = match proc_fd::current_task_get_file_desc(fd) {
        Some(f) => f,
        None => return Errno::EBADF.into(),
    };

    let mut kernel_buf = alloc::vec![0u8; count];
    match file.inode.read(offset as u64, &mut kernel_buf) {
        Ok(n)  => {
            unsafe {
                core::ptr::copy_nonoverlapping(kernel_buf.as_ptr(), buf, n);
            }
            n as SyscallResult
        }
        Err(e) => e as SyscallResult,
    }
}

/// `IoVec` structure for `writev`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IoVec {
    pub iov_base: *const u8,
    pub iov_len: usize,
}

/// `writev(fd, iov, iovcnt)` — Write vector.
pub fn sys_writev(fd: i32, iov: *const IoVec, iovcnt: i32) -> SyscallResult {
    if iov.is_null() || iovcnt <= 0 || iovcnt > 1024 {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(iov as *const u8, iovcnt as usize * core::mem::size_of::<IoVec>()) {
        return Errno::EFAULT.into();
    }
    let mut local_iov = alloc::vec![IoVec { iov_base: core::ptr::null(), iov_len: 0 }; iovcnt as usize];
    unsafe {
        core::ptr::copy_nonoverlapping(iov, local_iov.as_mut_ptr(), iovcnt as usize);
    }
    let mut total_written = 0;
    for io in local_iov {
        if io.iov_len == 0 { continue; }
        let ret = sys_write(fd, io.iov_base, io.iov_len);
        if ret < 0 {
            if total_written > 0 { break; }
            return ret;
        }
        total_written += ret;
    }
    total_written
}

/// `openat(dfd, pathname, flags, mode)` — Open file relative to directory file descriptor.
pub fn sys_openat(dfd: i32, pathname: *const u8, flags: i32, mode: u32) -> SyscallResult {
    if dfd == -100 { // AT_FDCWD
        sys_open(pathname, flags, mode)
    } else {
        if pathname.is_null() {
            return Errno::EFAULT.into();
        }
        // If the path starts with '/', it is absolute, so dfd is ignored.
        let first_byte = unsafe { pathname.read() };
        if first_byte == b'/' {
            sys_open(pathname, flags, mode)
        } else {
            // Relative to directory fd is not supported yet
            Errno::ENOSYS.into()
        }
    }
}

