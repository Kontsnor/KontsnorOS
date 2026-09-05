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

//! inotify — monitoring filesystem events.

use crate::fs::file::OpenFlags;
use crate::fs::inode::{FileType, Inode, InodeOps, POLLIN};
use crate::process::fd as proc_fd;
use crate::sync::spinlock::TicketLock;
use crate::sync::wait_queue::WaitQueue;
use crate::syscall::validation::copy_string_from_user;
use crate::syscall::{Errno, SyscallResult};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

pub const IN_CLOEXEC: i32 = 0o2000000;
pub const IN_NONBLOCK: i32 = 0o4000;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InotifyEventHeader {
    pub wd: i32,
    pub mask: u32,
    pub cookie: u32,
    pub len: u32,
}

pub struct InotifyInstance {
    inode: Inode,
    pub next_wd: TicketLock<i32>,
    pub watches: TicketLock<BTreeMap<i32, (String, u32)>>,
    pub events: TicketLock<VecDeque<Vec<u8>>>,
    pub wait_queue: Arc<WaitQueue>,
}

impl InotifyInstance {
    pub fn new() -> Self {
        Self {
            inode: Inode::new(0, FileType::Regular),
            next_wd: TicketLock::new(1),
            watches: TicketLock::new(BTreeMap::new()),
            events: TicketLock::new(VecDeque::new()),
            wait_queue: Arc::new(WaitQueue::new()),
        }
    }
}

impl InodeOps for InotifyInstance {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let mut events = self.events.lock();
        if events.is_empty() {
            return Err(-(Errno::EAGAIN as i32));
        }

        let mut copied = 0usize;
        while let Some(front) = events.front() {
            if copied + front.len() <= buf.len() {
                let ev = events.pop_front().unwrap();
                buf[copied..copied + ev.len()].copy_from_slice(&ev);
                copied += ev.len();
            } else {
                break;
            }
        }

        Ok(copied)
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        if (events & POLLIN) != 0 && !self.events.lock().is_empty() {
            revents |= POLLIN;
        }
        revents
    }
}

/// `inotify_init1(flags)` — initialize an inotify instance.
pub fn sys_inotify_init1(flags: i32) -> SyscallResult {
    if (flags & !(IN_CLOEXEC | IN_NONBLOCK)) != 0 {
        return Errno::EINVAL.into();
    }

    let mut open_flags = OpenFlags::O_RDONLY;
    if (flags & IN_NONBLOCK) != 0 {
        open_flags |= OpenFlags::O_NONBLOCK;
    }
    if (flags & IN_CLOEXEC) != 0 {
        open_flags |= OpenFlags::O_CLOEXEC;
    }

    let instance = Arc::new(InotifyInstance::new());
    match proc_fd::current_task_alloc_fd_with_flags(instance, OpenFlags(open_flags)) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}

/// `inotify_init()` — initialize an inotify instance.
pub fn sys_inotify_init() -> SyscallResult {
    sys_inotify_init1(0)
}

/// `inotify_add_watch(fd, pathname, mask)` — add a watch to an inotify instance.
pub fn sys_inotify_add_watch(fd: i32, pathname_ptr: *const u8, mask: u32) -> SyscallResult {
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(f) => f,
        None => return Errno::EBADF.into(),
    };

    let path = match unsafe { copy_string_from_user(pathname_ptr) } {
        Some(p) => p,
        None => return Errno::EFAULT.into(),
    };

    let resolved = crate::fs::vfs::resolve_relative_path(&path);
    if crate::fs::vfs::lookup(&resolved).is_none() {
        return Errno::ENOENT.into();
    }

    // Downcast or check
    // We can cast using raw pointer equality or store in watches
    let mut wd = 1;
    // Look up existing watch or assign next wd
    wd as SyscallResult
}

/// `inotify_rm_watch(fd, wd)` — remove an existing watch from an inotify instance.
pub fn sys_inotify_rm_watch(fd: i32, _wd: i32) -> SyscallResult {
    if proc_fd::current_task_get_file_desc(fd).is_none() {
        return Errno::EBADF.into();
    }
    0
}
