//! File I/O system calls: read, write, lseek, dup, pipe, fcntl, pread64, writev.

use super::super::{Errno, SyscallResult};
use crate::fs::file::{FileDescription, OpenFlags};
use crate::kprintln;
use crate::process::fd as proc_fd;
use crate::sync::spinlock::TicketLock;
use crate::syscall::validation::{validate_user_ptr, validate_user_ptr_write};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockType {
    Shared,
    Exclusive,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockOwner {
    Flock { fd_desc_ptr: usize },
    Fcntl { pid: u64 },
}

#[derive(Clone, Debug)]
pub struct FileRangeLock {
    pub owner: LockOwner,
    pub typ: LockType,
    pub start: u64,
    pub len: u64, // 0 means extends to EOF (infinity)
}

#[derive(Default, Clone, Debug)]
pub struct FileLockState {
    pub range_locks: alloc::vec::Vec<FileRangeLock>,
}

pub static FILE_LOCKS: TicketLock<BTreeMap<(u32, u64), FileLockState>> =
    TicketLock::new(BTreeMap::new());

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Flock {
    pub l_type: i16,   // F_RDLCK (0), F_WRLCK (1), F_UNLCK (2)
    pub l_whence: i16, // SEEK_SET (0), SEEK_CUR (1), SEEK_END (2)
    pub l_start: i64,
    pub l_len: i64,
    pub l_pid: i32,
}

fn resolve_lock_range(
    fl: &Flock,
    current_offset: u64,
    file_size: u64,
) -> Result<(u64, u64), Errno> {
    let start = match fl.l_whence {
        0 => {
            // SEEK_SET
            if fl.l_start < 0 {
                return Err(Errno::EINVAL);
            }
            fl.l_start as u64
        }
        1 => {
            // SEEK_CUR
            let val = current_offset as i64 + fl.l_start;
            if val < 0 {
                return Err(Errno::EINVAL);
            }
            val as u64
        }
        2 => {
            // SEEK_END
            let val = file_size as i64 + fl.l_start;
            if val < 0 {
                return Err(Errno::EINVAL);
            }
            val as u64
        }
        _ => return Err(Errno::EINVAL),
    };

    if fl.l_len > 0 {
        Ok((start, fl.l_len as u64))
    } else if fl.l_len < 0 {
        let new_start = start as i64 + fl.l_len;
        if new_start < 0 {
            return Err(Errno::EINVAL);
        }
        Ok((new_start as u64, (-fl.l_len) as u64))
    } else {
        // l_len == 0 means till EOF
        Ok((start, 0))
    }
}

fn ranges_overlap(start1: u64, len1: u64, start2: u64, len2: u64) -> bool {
    let end1 = if len1 == 0 {
        u64::MAX
    } else {
        start1.saturating_add(len1)
    };
    let end2 = if len2 == 0 {
        u64::MAX
    } else {
        start2.saturating_add(len2)
    };
    start1 < end2 && start2 < end1
}

fn conflicts(
    existing: &[FileRangeLock],
    req_owner: LockOwner,
    req_typ: LockType,
    req_start: u64,
    req_len: u64,
) -> bool {
    for lock in existing {
        if lock.owner != req_owner && ranges_overlap(req_start, req_len, lock.start, lock.len) {
            if req_typ == LockType::Exclusive || lock.typ == LockType::Exclusive {
                return true;
            }
        }
    }
    false
}

fn subtract_range(
    lock: &FileRangeLock,
    u_start: u64,
    u_len: u64,
) -> alloc::vec::Vec<FileRangeLock> {
    if !ranges_overlap(lock.start, lock.len, u_start, u_len) {
        return alloc::vec![lock.clone()];
    }
    let l_end = if lock.len == 0 {
        u64::MAX
    } else {
        lock.start + lock.len
    };
    let u_end = if u_len == 0 {
        u64::MAX
    } else {
        u_start + u_len
    };

    let mut res = alloc::vec::Vec::new();
    if lock.start < u_start {
        res.push(FileRangeLock {
            owner: lock.owner,
            typ: lock.typ,
            start: lock.start,
            len: u_start - lock.start,
        });
    }
    if l_end > u_end {
        res.push(FileRangeLock {
            owner: lock.owner,
            typ: lock.typ,
            start: u_end,
            len: if lock.len == 0 { 0 } else { l_end - u_end },
        });
    }
    res
}

pub fn release_flock_locks(fd_desc_ptr: usize, ino: u64) {
    let mut locks = FILE_LOCKS.lock();
    if let Some(state) = locks.get_mut(&(0, ino)) {
        state.range_locks.retain(|lock| match lock.owner {
            LockOwner::Flock { fd_desc_ptr: ptr } => ptr != fd_desc_ptr,
            _ => true,
        });
    }
}

pub fn release_fcntl_locks(pid: u64) {
    let mut locks = FILE_LOCKS.lock();
    for state in locks.values_mut() {
        state.range_locks.retain(|lock| match lock.owner {
            LockOwner::Fcntl { pid: p } => p != pid,
            _ => true,
        });
    }
}

pub fn release_fcntl_locks_for_pid_and_ino(pid: u64, ino: u64) {
    let mut locks = FILE_LOCKS.lock();
    if let Some(state) = locks.get_mut(&(0, ino)) {
        state.range_locks.retain(|lock| match lock.owner {
            LockOwner::Fcntl { pid: p } => p != pid,
            _ => true,
        });
    }
}

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

/// `pipe2(pipefds, flags)` — Create a unidirectional pipe with flags.
pub fn sys_pipe2(pipefds: *mut i32, flags: i32) -> SyscallResult {
    if pipefds.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(pipefds as *const u8, 8) {
        return Errno::EFAULT.into();
    }

    const O_CLOEXEC: i32 = 0x80000;
    const O_NONBLOCK: i32 = 0o4000;

    if (flags & !(O_CLOEXEC | O_NONBLOCK)) != 0 {
        return Errno::EINVAL.into();
    }

    use crate::fs::file::OpenFlags;
    let mut raw_flags = OpenFlags::O_RDWR;
    if (flags & O_CLOEXEC) != 0 {
        raw_flags |= OpenFlags::O_CLOEXEC;
    }
    if (flags & O_NONBLOCK) != 0 {
        raw_flags |= OpenFlags::O_NONBLOCK;
    }
    let open_flags = OpenFlags(raw_flags);

    // Create the pipe VFS endpoints
    let (reader, writer) = crate::fs::pipe::make_pipe();

    // Allocate file descriptors
    let fd0 = match proc_fd::current_task_alloc_fd_with_flags_and_path(
        reader,
        open_flags,
        Some(alloc::string::String::from("pipe:[reader]")),
    ) {
        Some(fd) => fd,
        None => return Errno::EMFILE.into(),
    };

    let fd1 = match proc_fd::current_task_alloc_fd_with_flags_and_path(
        writer,
        open_flags,
        Some(alloc::string::String::from("pipe:[writer]")),
    ) {
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

    kprintln!(
        "[syscall] pipe2(flags={:#x}) -> fds: [{}, {}]",
        flags,
        fd0,
        fd1
    );
    0 // Success
}

/// `memfd_create(name, flags)` — Create an anonymous RAM-backed file descriptor.
pub fn sys_memfd_create(name_ptr: *const u8, flags: u32) -> SyscallResult {
    let name = unsafe {
        match crate::syscall::validation::copy_string_from_user_pub(name_ptr) {
            Some(s) => s,
            None => return Errno::EFAULT.into(),
        }
    };

    kprintln!(
        "[syscall] memfd_create(name=\"{}\", flags={:#x})",
        name,
        flags
    );

    const MFD_CLOEXEC: u32 = 0x0001;
    const MFD_ALLOW_SEALING: u32 = 0x0002;

    if (flags & !(MFD_CLOEXEC | MFD_ALLOW_SEALING)) != 0 {
        return Errno::EINVAL.into();
    }

    let mut raw_flags = OpenFlags::O_RDWR;
    if (flags & MFD_CLOEXEC) != 0 {
        raw_flags |= OpenFlags::O_CLOEXEC;
    }
    let open_flags = OpenFlags(raw_flags);

    let inode = crate::fs::tmpfs::create_memfd_inode();
    let path = format!("memfd:{}", name);
    let fd = match proc_fd::current_task_alloc_fd_with_flags_and_path(inode, open_flags, Some(path))
    {
        Some(fd) => fd,
        None => return Errno::EMFILE.into(),
    };

    fd as SyscallResult
}

/// `fcntl(fd, cmd, arg)` — File control.
pub fn sys_fcntl(fd: i32, cmd: i32, arg: u64) -> SyscallResult {
    match cmd {
        0 | 1030 => {
            // F_DUPFD or F_DUPFD_CLOEXEC
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

            if cmd == 1030 {
                file_desc.flags.lock().0 |= crate::fs::file::OpenFlags::O_CLOEXEC;
            }

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
                "[syscall] fcntl(fd={}, cmd={}, arg={}) -> {}",
                fd,
                cmd,
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
            let file_desc = match proc_fd::current_task_get_file_desc(fd) {
                Some(d) => d,
                None => return Errno::EBADF.into(),
            };

            if arg == 0 {
                return Errno::EFAULT.into();
            }
            if validate_user_ptr_write(arg as *mut u8, core::mem::size_of::<Flock>()).is_err() {
                return Errno::EFAULT.into();
            }

            // SAFETY: The pointer was validated with validate_user_ptr_write and is safe to read.
            let mut fl = unsafe { *(arg as *const Flock) };

            let current_pid = match crate::process::scheduler::current_pid() {
                Some(p) => p.as_u64(),
                None => return Errno::ESRCH.into(),
            };

            let owner = if cmd == 36 || cmd == 37 || cmd == 38 {
                LockOwner::Flock {
                    fd_desc_ptr: Arc::as_ptr(&file_desc) as usize,
                }
            } else {
                LockOwner::Fcntl { pid: current_pid }
            };

            let ino = file_desc.inode.inode().ino;
            let dev = 0;

            let current_offset = *file_desc.offset.lock();
            let file_size = file_desc.inode.inode().size;

            let (start, len) = match resolve_lock_range(&fl, current_offset, file_size) {
                Ok(r) => r,
                Err(e) => return e.into(),
            };

            if cmd == 5 || cmd == 36 {
                // F_GETLK / F_OFD_GETLK
                let req_typ = if fl.l_type == 0 {
                    // F_RDLCK
                    LockType::Shared
                } else if fl.l_type == 1 {
                    // F_WRLCK
                    LockType::Exclusive
                } else {
                    return Errno::EINVAL.into();
                };

                let locks = FILE_LOCKS.lock();
                if let Some(state) = locks.get(&(dev, ino)) {
                    let mut conflicting_lock = None;
                    for lock in &state.range_locks {
                        if lock.owner != owner && ranges_overlap(start, len, lock.start, lock.len) {
                            if req_typ == LockType::Exclusive || lock.typ == LockType::Exclusive {
                                conflicting_lock = Some(lock.clone());
                                break;
                            }
                        }
                    }

                    if let Some(c) = conflicting_lock {
                        fl.l_type = match c.typ {
                            LockType::Shared => 0,    // F_RDLCK
                            LockType::Exclusive => 1, // F_WRLCK
                        };
                        fl.l_whence = 0; // SEEK_SET
                        fl.l_start = c.start as i64;
                        fl.l_len = c.len as i64;
                        fl.l_pid = match c.owner {
                            LockOwner::Fcntl { pid } => pid as i32,
                            LockOwner::Flock { .. } => -1,
                        };
                    } else {
                        fl.l_type = 2; // F_UNLCK
                    }
                } else {
                    fl.l_type = 2; // F_UNLCK
                }

                // SAFETY: We validated the pointer as writable and within user limits.
                unsafe {
                    *(arg as *mut Flock) = fl;
                }
                0
            } else {
                // F_SETLK / F_SETLKW / F_OFD_SETLK / F_OFD_SETLKW
                if fl.l_type == 2 {
                    // F_UNLCK
                    let mut locks = FILE_LOCKS.lock();
                    if let Some(state) = locks.get_mut(&(dev, ino)) {
                        let mut new_locks = alloc::vec::Vec::new();
                        for lock in &state.range_locks {
                            if lock.owner == owner {
                                new_locks.extend(subtract_range(lock, start, len));
                            } else {
                                new_locks.push(lock.clone());
                            }
                        }
                        state.range_locks = new_locks;
                    }
                    0
                } else {
                    let req_typ = if fl.l_type == 0 {
                        // F_RDLCK
                        LockType::Shared
                    } else if fl.l_type == 1 {
                        // F_WRLCK
                        LockType::Exclusive
                    } else {
                        return Errno::EINVAL.into();
                    };

                    let blocking = cmd == 7 || cmd == 38;

                    loop {
                        let mut locks = FILE_LOCKS.lock();
                        let state = locks
                            .entry((dev, ino))
                            .or_insert_with(FileLockState::default);

                        if conflicts(&state.range_locks, owner, req_typ, start, len) {
                            if !blocking {
                                return Errno::EAGAIN.into();
                            }
                            drop(locks);
                            crate::process::scheduler::yield_now();
                        } else {
                            // Subtract range from our own locks first to update/overwrite
                            let mut new_locks = alloc::vec::Vec::new();
                            for lock in &state.range_locks {
                                if lock.owner == owner {
                                    new_locks.extend(subtract_range(lock, start, len));
                                } else {
                                    new_locks.push(lock.clone());
                                }
                            }
                            // Add the new lock
                            new_locks.push(FileRangeLock {
                                owner,
                                typ: req_typ,
                                start,
                                len,
                            });
                            state.range_locks = new_locks;
                            return 0;
                        }
                    }
                }
            }
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
    if fd < 0 {
        return Errno::EBADF.into();
    }
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    let ino = file_desc.inode.inode().ino;
    let dev = 0; // Device ID consistent with stat
    let fd_desc_ptr = Arc::as_ptr(&file_desc) as usize;
    let owner = LockOwner::Flock { fd_desc_ptr };

    // Standard flock operations
    let lock_sh = 1;
    let lock_ex = 2;
    let lock_nb = 4;
    let lock_un = 8;

    if (operation & lock_un) != 0 {
        // Unlock
        let mut locks = FILE_LOCKS.lock();
        if let Some(state) = locks.get_mut(&(dev, ino)) {
            state.range_locks.retain(|lock| match lock.owner {
                LockOwner::Flock { fd_desc_ptr: ptr } => ptr != fd_desc_ptr,
                _ => true,
            });
        }
        return 0;
    }

    let req_typ = if (operation & lock_ex) != 0 {
        LockType::Exclusive
    } else if (operation & lock_sh) != 0 {
        LockType::Shared
    } else {
        return Errno::EINVAL.into();
    };

    let non_block = (operation & lock_nb) != 0;

    loop {
        let mut locks = FILE_LOCKS.lock();
        let state = locks
            .entry((dev, ino))
            .or_insert_with(FileLockState::default);

        if conflicts(&state.range_locks, owner, req_typ, 0, 0) {
            if non_block {
                return Errno::EAGAIN.into();
            }
            drop(locks);
            crate::process::scheduler::yield_now();
        } else {
            // Remove any existing flock lock for this description
            state.range_locks.retain(|lock| match lock.owner {
                LockOwner::Flock { fd_desc_ptr: ptr } => ptr != fd_desc_ptr,
                _ => true,
            });
            // Add the new lock
            state.range_locks.push(FileRangeLock {
                owner,
                typ: req_typ,
                start: 0,
                len: 0,
            });
            return 0;
        }
    }
}
