//! File I/O system calls: read, write, lseek, dup, pipe, fcntl, pread64, writev.

use super::super::{Errno, SyscallResult};
use crate::fs::file::{FileDescription, OpenFlags};
use crate::kprintln;
use crate::process::fd as proc_fd;
use crate::syscall::validation::{validate_user_ptr, validate_user_ptr_write};
use alloc::sync::Arc;

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
    if validate_user_ptr_write(buf, count).is_err() {
        return Errno::EFAULT.into();
    }

    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    let is_pipe = file_desc.inode.inode().file_type == crate::fs::inode::FileType::Pipe;
    if is_pipe {
        let pid_str = crate::process::scheduler::current_pid()
            .map(|p| p.as_u64())
            .unwrap_or(0);
        crate::kprintln!("[syscall pid={}] sys_read on pipe fd {}", pid_str, fd);
    }

    let mut total_read = 0;
    let mut temp_buf = [0u8; 4096];

    while total_read < count {
        let chunk_size = core::cmp::min(count - total_read, 4096);
        match file_desc.read(&mut temp_buf[..chunk_size]) {
            Ok(0) => break,
            Ok(n) => {
                unsafe {
                    core::ptr::copy_nonoverlapping(temp_buf.as_ptr(), buf.add(total_read), n);
                }
                total_read += n;
                if is_pipe {
                    crate::kprintln!(
                        "[syscall] sys_read on pipe fd {} chunk returned {} bytes",
                        fd,
                        n
                    );
                }
                if n < chunk_size {
                    break;
                }
            }
            Err(e) => {
                if is_pipe {
                    crate::kprintln!(
                        "[syscall] sys_read on pipe fd {} failed with error {}",
                        fd,
                        e
                    );
                }
                if total_read > 0 {
                    break;
                }
                return e as SyscallResult;
            }
        }
    }

    total_read as SyscallResult
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
        let pid_str = crate::process::scheduler::current_pid()
            .map(|p| p.as_u64())
            .unwrap_or(0);
        crate::kprintln!(
            "[syscall pid={}] sys_write on pipe fd {} count {}",
            pid_str,
            fd,
            count
        );
    }

    let mut total_written = 0;
    let mut temp_buf = [0u8; 4096];

    while total_written < count {
        let chunk_size = core::cmp::min(count - total_written, 4096);
        unsafe {
            core::ptr::copy_nonoverlapping(
                buf.add(total_written),
                temp_buf.as_mut_ptr(),
                chunk_size,
            );
        }
        match file_desc.write(&temp_buf[..chunk_size]) {
            Ok(0) => break,
            Ok(n) => {
                total_written += n;
                if is_pipe {
                    crate::kprintln!(
                        "[syscall] sys_write on pipe fd {} returned {} bytes written",
                        fd,
                        n
                    );
                }
                if n < chunk_size {
                    break;
                }
            }
            Err(e) => {
                if is_pipe {
                    crate::kprintln!(
                        "[syscall] sys_write on pipe fd {} failed with error {}",
                        fd,
                        e
                    );
                }
                if total_written > 0 {
                    break;
                }
                return e as SyscallResult;
            }
        }
    }

    total_written as SyscallResult
}

/// `fsync(fd)` — Commit file buffer cache/page cache changes to disk.
pub fn sys_fsync(fd: i32) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    match crate::memory::page_cache::flush_all_for_inode(&file_desc.inode) {
        Ok(_) => 0,
        Err(e) => e as SyscallResult,
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
        let pid_str = crate::process::scheduler::current_pid()
            .map(|p| p.as_u64())
            .unwrap_or(0);
        crate::kprintln!(
            "[syscall pid={}] sys_dup2(oldfd={}, newfd={}) on pipe",
            pid_str,
            oldfd,
            newfd
        );
    }
    match proc_fd::current_task_dup2_fd(oldfd, newfd) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EBADF.into(),
    }
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

/// `fcntl(fd, cmd, arg)` — File control.
pub fn sys_fcntl(fd: i32, cmd: i32, arg: u64) -> SyscallResult {
    match cmd {
        0 => {
            // F_DUPFD
            let start_fd = arg as i32;
            if start_fd < 0 {
                return Errno::EINVAL.into();
            }

            let current_pid = match crate::process::scheduler::current_pid() {
                Some(p) => p,
                None => return Errno::ESRCH.into(),
            };
            let task_arc = match crate::process::scheduler::get_task_arc(current_pid) {
                Some(t) => t,
                None => return Errno::ESRCH.into(),
            };
            let mut task = task_arc.lock();
            let mut fd_table = task.fd_table.lock();

            let file_desc = match fd_table.entries.get(fd as usize) {
                Some(Some(desc)) => desc.clone(),
                _ => return Errno::EBADF.into(),
            };

            *file_desc.ref_count.lock() += 1;

            let mut new_fd = start_fd;
            while (new_fd as usize) < fd_table.entries.len()
                && fd_table.entries[new_fd as usize].is_some()
            {
                new_fd += 1;
            }

            if (new_fd as usize) >= fd_table.entries.len() {
                fd_table.entries.resize(new_fd as usize + 1, None);
            }
            fd_table.entries[new_fd as usize] = Some(file_desc);

            kprintln!(
                "[syscall] fcntl(fd={}, F_DUPFD, arg={}) -> {}",
                fd,
                arg,
                new_fd
            );
            new_fd as i64
        }
        1 => {
            // F_GETFD
            0
        }
        2 => {
            // F_SETFD
            0
        }
        3 => {
            // F_GETFL
            let current_pid = match crate::process::scheduler::current_pid() {
                Some(p) => p,
                None => return Errno::ESRCH.into(),
            };
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
                let task = task_arc.lock();
                let fd_table = task.fd_table.lock();
                if let Some(Some(desc)) = fd_table.entries.get(fd as usize) {
                    return desc.flags.lock().0 as i64;
                }
            }
            Errno::EBADF.into()
        }
        4 => {
            // F_SETFL
            let current_pid = match crate::process::scheduler::current_pid() {
                Some(p) => p,
                None => return Errno::ESRCH.into(),
            };
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
                let mut task = task_arc.lock();
                let mut fd_table = task.fd_table.lock();
                if let Some(Some(desc)) = fd_table.entries.get_mut(fd as usize) {
                    let allowed_flags = OpenFlags::O_APPEND | OpenFlags::O_NONBLOCK;
                    let mut flags = desc.flags.lock();
                    let old_val = flags.0;
                    flags.0 = (old_val & !allowed_flags) | (arg as u32 & allowed_flags);
                    return 0;
                }
            }
            Errno::EBADF.into()
        }
        5 | 6 | 7 | 36 | 37 | 38 => {
            // F_GETLK (5), F_SETLK (6), F_SETLKW (7), F_OFD_GETLK (36), F_OFD_SETLK (37), F_OFD_SETLKW (38)
            // Mock successful POSIX file lock acquisition to satisfy Cargo database locking.
            kprintln!(
                "[syscall] fcntl(fd={}, cmd={} (lock command), arg={}) -> stub success",
                fd,
                cmd,
                arg
            );
            0 // Success
        }
        _ => {
            kprintln!(
                "[syscall] fcntl(fd={}, cmd={}, arg={}) -> ENOSYS",
                fd,
                cmd,
                arg
            );
            Errno::ENOSYS.into()
        }
    }
}

/// `pread64(fd, buf, count, offset)` — Read from a file descriptor at an offset.
///
/// Unlike `read`, this does not change the file's seek position.
pub fn sys_pread64(fd: i32, buf: *mut u8, count: usize, offset: i64) -> SyscallResult {
    if fd < 0 {
        return Errno::EBADF.into();
    }
    if buf.is_null() || count == 0 {
        return 0;
    }
    if validate_user_ptr_write(buf, count).is_err() {
        return Errno::EFAULT.into();
    }

    let file = match proc_fd::current_task_get_file_desc(fd) {
        Some(f) => f,
        None => return Errno::EBADF.into(),
    };

    let mut total_read = 0;
    let mut temp_buf = [0u8; 4096];

    while total_read < count {
        let chunk_size = core::cmp::min(count - total_read, 4096);
        let chunk_offset = offset + total_read as i64;
        match file
            .inode
            .read(chunk_offset as u64, &mut temp_buf[..chunk_size])
        {
            Ok(0) => break,
            Ok(n) => {
                unsafe {
                    core::ptr::copy_nonoverlapping(temp_buf.as_ptr(), buf.add(total_read), n);
                }
                total_read += n;
                if n < chunk_size {
                    break;
                }
            }
            Err(e) => {
                if total_read > 0 {
                    break;
                }
                return e as SyscallResult;
            }
        }
    }

    total_read as SyscallResult
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
    if !validate_user_ptr(
        iov as *const u8,
        iovcnt as usize * core::mem::size_of::<IoVec>(),
    ) {
        return Errno::EFAULT.into();
    }
    let mut local_iov =
        alloc::vec![IoVec { iov_base: core::ptr::null(), iov_len: 0 }; iovcnt as usize];
    unsafe {
        core::ptr::copy_nonoverlapping(iov, local_iov.as_mut_ptr(), iovcnt as usize);
    }
    let mut total_written = 0;
    for io in local_iov {
        if io.iov_len == 0 {
            continue;
        }
        let ret = sys_write(fd, io.iov_base, io.iov_len);
        if ret < 0 {
            if total_written > 0 {
                break;
            }
            return ret;
        }
        total_written += ret;
    }
    total_written
}

/// `flock(fd, operation)` — Apply or remove an advisory lock on an open file.
pub fn sys_flock(fd: i32, operation: i32) -> SyscallResult {
    kprintln!(
        "[syscall] flock(fd={}, operation={}) -> stub success",
        fd,
        operation
    );
    0
}
