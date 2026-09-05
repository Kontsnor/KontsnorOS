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

//! pidfd — process file descriptors.
//!
//! Provides a file descriptor referring to a process, supporting epoll polling
//! for process termination and signal delivery.

use crate::fs::file::OpenFlags;
use crate::fs::inode::{FileType, Inode, InodeOps, POLLIN};
use crate::process::fd as proc_fd;
use crate::process::pid::Pid;
use crate::process::scheduler;
use crate::process::task::TaskState;
use crate::syscall::{Errno, SyscallResult};
use alloc::sync::Arc;

pub const PIDFD_NONBLOCK: u32 = 0o4000;

pub struct PidFd {
    inode: Inode,
    pub target_pid: u64,
}

impl PidFd {
    pub fn new(target_pid: u64) -> Self {
        Self {
            inode: Inode::new(0, FileType::Regular),
            target_pid,
        }
    }
}

impl InodeOps for PidFd {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn as_pidfd(&self) -> Option<&PidFd> {
        Some(self)
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        if (events & POLLIN) != 0 {
            let exited = match scheduler::get_task_arc(Pid::from_raw(self.target_pid)) {
                Some(task_arc) => task_arc.lock().state == TaskState::Zombie,
                None => true,
            };
            if exited {
                revents |= POLLIN;
            }
        }
        revents
    }
}

/// `pidfd_open(pid, flags)` — obtain a file descriptor that refers to a process.
pub fn sys_pidfd_open(pid: i32, flags: u32) -> SyscallResult {
    if pid <= 0 {
        return Errno::EINVAL.into();
    }
    if (flags & !PIDFD_NONBLOCK) != 0 {
        return Errno::EINVAL.into();
    }

    // Verify process exists
    if scheduler::get_task_arc(Pid::from_raw(pid as u64)).is_none() {
        return Errno::ESRCH.into();
    }

    let pidfd = Arc::new(PidFd::new(pid as u64));
    let mut open_flags = OpenFlags::O_RDWR;
    if (flags & PIDFD_NONBLOCK) != 0 {
        open_flags |= OpenFlags::O_NONBLOCK;
    }

    match proc_fd::current_task_alloc_fd_with_flags(pidfd, OpenFlags(open_flags)) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}

/// `pidfd_send_signal(pidfd, sig, info, flags)` — send a signal to a process specified by a file descriptor.
pub fn sys_pidfd_send_signal(pidfd: i32, sig: i32, _info: *const u8, flags: u32) -> SyscallResult {
    if flags != 0 {
        return Errno::EINVAL.into();
    }
    let file_desc = match proc_fd::current_task_get_file_desc(pidfd) {
        Some(f) => f,
        None => return Errno::EBADF.into(),
    };

    let target_pid = match file_desc.inode.as_pidfd() {
        Some(p) => p.target_pid,
        None => return Errno::EBADF.into(),
    };

    if sig == 0 {
        if scheduler::get_task_arc(Pid::from_raw(target_pid)).is_some() {
            return 0;
        } else {
            return Errno::ESRCH.into();
        }
    }

    crate::syscall::signal::sys_kill(target_pid as i32, sig)
}

/// `pidfd_getfd(pidfd, targetfd, flags)` — obtain a duplicate of another process's file descriptor.
pub fn sys_pidfd_getfd(pidfd: i32, targetfd: i32, flags: u32) -> SyscallResult {
    if (flags & !(OpenFlags::O_CLOEXEC as u32)) != 0 {
        return Errno::EINVAL.into();
    }
    let file_desc = match proc_fd::current_task_get_file_desc(pidfd) {
        Some(f) => f,
        None => return Errno::EBADF.into(),
    };

    let target_pid = match file_desc.inode.as_pidfd() {
        Some(p) => p.target_pid,
        None => return Errno::EBADF.into(),
    };

    let target_task_arc = match scheduler::get_task_arc(Pid::from_raw(target_pid)) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };

    let target_file_desc = {
        let task = target_task_arc.lock();
        let fd_table = task.fd_table.lock();
        if targetfd < 0 || (targetfd as usize) >= fd_table.entries.len() {
            return Errno::EBADF.into();
        }
        match fd_table.entries[targetfd as usize].clone() {
            Some(desc) => desc,
            None => return Errno::EBADF.into(),
        }
    };

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let current_task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };

    let task = current_task_arc.lock();
    let mut fd_table = task.fd_table.lock();
    let limit = task.rlimit_nofile_cur as usize;

    let mut new_fd = None;
    for (fd, slot) in fd_table.entries.iter_mut().enumerate() {
        if fd >= limit {
            break;
        }
        if slot.is_none() {
            *slot = Some(target_file_desc.clone());
            *target_file_desc.ref_count.lock() += 1;
            if fd < fd_table.cloexec.len() {
                fd_table.cloexec[fd] = (flags & (OpenFlags::O_CLOEXEC as u32)) != 0;
            }
            new_fd = Some(fd);
            break;
        }
    }

    if new_fd.is_none() && fd_table.entries.len() < limit {
        let fd = fd_table.entries.len();
        fd_table.entries.push(Some(target_file_desc.clone()));
        *target_file_desc.ref_count.lock() += 1;
        fd_table
            .cloexec
            .push((flags & (OpenFlags::O_CLOEXEC as u32)) != 0);
        new_fd = Some(fd);
    }

    match new_fd {
        Some(fd) => fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}
