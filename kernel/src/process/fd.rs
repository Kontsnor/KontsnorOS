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

//! File descriptor helpers for the current process.

use crate::fs::file::{FileDescription, OpenFlags};
use crate::fs::inode::InodeOps;
use crate::process::scheduler;
use alloc::sync::Arc;

/// Retrieve the file seek offset for descriptor `fd`.
pub fn get_fd_offset(fd: i32) -> Option<u64> {
    if fd < 0 {
        return None;
    }
    let fd_idx = fd as usize;
    let current_pid = scheduler::current_pid()?;
    let task_arc = scheduler::get_task_arc(current_pid)?;
    let task = task_arc.lock();
    let fd_table = task.fd_table.lock();
    let file_desc = fd_table.entries.get(fd_idx)?.as_ref()?;
    let offset = *file_desc.offset.lock();
    Some(offset)
}

/// Set the file seek offset for descriptor `fd`.
pub fn set_fd_offset(fd: i32, offset: u64) -> Option<()> {
    if fd < 0 {
        return None;
    }
    let fd_idx = fd as usize;
    let current_pid = scheduler::current_pid()?;
    let task_arc = scheduler::get_task_arc(current_pid)?;
    let task = task_arc.lock();
    let fd_table = task.fd_table.lock();
    let file_desc = fd_table.entries.get(fd_idx)?.as_ref()?;
    *file_desc.offset.lock() = offset;
    Some(())
}

/// Retrieve a clone of the inode backing file descriptor `fd` in the
/// currently running task.
pub fn current_task_read_fd(fd: i32) -> Option<Arc<dyn InodeOps>> {
    if fd < 0 {
        return None;
    }
    let fd_idx = fd as usize;
    let current_pid = scheduler::current_pid()?;
    let task_arc = scheduler::get_task_arc(current_pid)?;
    let task = task_arc.lock();
    let fd_table = task.fd_table.lock();
    fd_table
        .entries
        .get(fd_idx)?
        .as_ref()
        .map(|desc| desc.inode.clone())
}

/// Retrieve a clone of the FileDescription backing file descriptor `fd`
/// in the currently running task.
pub fn current_task_get_file_desc(fd: i32) -> Option<Arc<FileDescription>> {
    if fd < 0 {
        return None;
    }
    let fd_idx = fd as usize;
    let current_pid = scheduler::current_pid()?;
    let task_arc = scheduler::get_task_arc(current_pid)?;
    let task = task_arc.lock();
    let fd_table = task.fd_table.lock();
    fd_table.entries.get(fd_idx)?.as_ref().cloned()
}

/// Allocate the next free file descriptor slot in the current task's fd_table
/// and store the given inode with default read-write flags.
pub fn current_task_alloc_fd(inode: Arc<dyn InodeOps>) -> Option<i32> {
    current_task_alloc_fd_with_flags(inode, OpenFlags(OpenFlags::O_RDWR))
}

/// Allocate the next free file descriptor slot with specified flags.
pub fn current_task_alloc_fd_with_flags(inode: Arc<dyn InodeOps>, flags: OpenFlags) -> Option<i32> {
    current_task_alloc_fd_with_flags_and_path(inode, flags, None)
}

/// Allocate the next free file descriptor slot with specified flags and open path.
pub fn current_task_alloc_fd_with_flags_and_path(
    inode: Arc<dyn InodeOps>,
    flags: OpenFlags,
    path: Option<alloc::string::String>,
) -> Option<i32> {
    let current_pid = scheduler::current_pid()?;
    let task_arc = scheduler::get_task_arc(current_pid)?;
    let task = task_arc.lock();
    let mut fd_table = task.fd_table.lock();
    let file_desc = Arc::new(FileDescription::new(inode, flags, path));

    let max_fds = task.rlimit_nofile_cur as usize;
    let start_fd = fd_table.next_free_fd;

    // Fast-path: search for first free slot starting at next_free_fd
    for i in start_fd..fd_table.entries.len() {
        if i >= max_fds {
            return None;
        }
        if fd_table.entries[i].is_none() {
            fd_table.entries[i] = Some(file_desc);
            if i >= fd_table.cloexec.len() {
                fd_table.cloexec.resize(i + 1, false);
            }
            fd_table.cloexec[i] = (flags.0 & OpenFlags::O_CLOEXEC) != 0;
            fd_table.next_free_fd = i + 1;
            return Some(i as i32);
        }
    }

    // No free slot found in existing range — extend table up to rlimit_nofile_cur
    let next_idx = fd_table.entries.len();
    if next_idx < max_fds {
        fd_table.entries.push(Some(file_desc));
        fd_table.cloexec.resize(next_idx + 1, false);
        fd_table.cloexec[next_idx] = (flags.0 & OpenFlags::O_CLOEXEC) != 0;
        fd_table.next_free_fd = next_idx + 1;
        Some(next_idx as i32)
    } else {
        None // EMFILE
    }
}

/// Close file descriptor `fd` in the current task's fd_table.
pub fn current_task_close_fd(fd: i32) -> bool {
    if fd < 0 {
        return false;
    }
    let fd_idx = fd as usize;
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return false,
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return false,
    };
    let task = task_arc.lock();
    let mut fd_table = task.fd_table.lock();

    let desc = if fd_idx < fd_table.entries.len() && fd_table.entries[fd_idx].is_some() {
        if fd_idx < fd_table.cloexec.len() {
            fd_table.cloexec[fd_idx] = false;
        }
        if fd_idx < fd_table.next_free_fd {
            fd_table.next_free_fd = fd_idx;
        }
        fd_table.entries[fd_idx].take()
    } else {
        None
    };

    drop(fd_table);
    drop(task); // Drop the task lock before dropping the desc (which might trigger Drop calling flush_all_for_inode)

    if let Some(desc) = desc {
        let mut rc = desc.ref_count.lock();
        if *rc > 0 {
            *rc -= 1;
        }
        true
    } else {
        false
    }
}

/// Duplicate an existing file descriptor `fd` in the current task's fd_table.
pub fn current_task_dup_fd(fd: i32) -> Option<i32> {
    if fd < 0 {
        return None;
    }
    let fd_idx = fd as usize;
    let current_pid = scheduler::current_pid()?;
    let task_arc = scheduler::get_task_arc(current_pid)?;
    let task = task_arc.lock();
    let mut fd_table = task.fd_table.lock();

    let file_desc = fd_table.entries.get(fd_idx)?.as_ref().cloned()?;
    *file_desc.ref_count.lock() += 1;

    let max_fds = task.rlimit_nofile_cur as usize;
    let start_fd = fd_table.next_free_fd;

    // Fast-path: search for first free slot starting at next_free_fd
    for i in start_fd..fd_table.entries.len() {
        if i >= max_fds {
            return None;
        }
        if fd_table.entries[i].is_none() {
            fd_table.entries[i] = Some(file_desc);
            if i >= fd_table.cloexec.len() {
                fd_table.cloexec.resize(i + 1, false);
            }
            fd_table.cloexec[i] = false; // dup clears close-on-exec
            fd_table.next_free_fd = i + 1;
            return Some(i as i32);
        }
    }

    // No free slot found in existing range — extend table up to rlimit_nofile_cur
    let next_idx = fd_table.entries.len();
    if next_idx < max_fds {
        fd_table.entries.push(Some(file_desc));
        fd_table.cloexec.resize(next_idx + 1, false);
        fd_table.cloexec[next_idx] = false; // dup clears close-on-exec
        fd_table.next_free_fd = next_idx + 1;
        Some(next_idx as i32)
    } else {
        None
    }
}

/// Duplicate an existing file descriptor `oldfd` onto `newfd` in the current task's fd_table.
pub fn current_task_dup2_fd(oldfd: i32, newfd: i32) -> Option<i32> {
    if oldfd < 0 || newfd < 0 {
        return None;
    }
    let current_pid = scheduler::current_pid()?;
    let task_arc = scheduler::get_task_arc(current_pid)?;

    if oldfd == newfd {
        let task = task_arc.lock();
        if newfd as u64 >= task.rlimit_nofile_cur {
            return None;
        }
        let fd_table = task.fd_table.lock();
        if fd_table.entries.get(oldfd as usize)?.as_ref().is_some() {
            return Some(newfd);
        } else {
            return None;
        }
    }

    let task = task_arc.lock();
    if newfd as u64 >= task.rlimit_nofile_cur {
        return None;
    }
    let mut fd_table = task.fd_table.lock();
    let file_desc = fd_table.entries.get(oldfd as usize)?.as_ref().cloned()?;
    *file_desc.ref_count.lock() += 1;

    // Ensure fd_table is large enough to contain newfd
    let newfd_idx = newfd as usize;
    if newfd_idx >= fd_table.entries.len() {
        fd_table.entries.resize(newfd_idx + 1, None);
    }
    if newfd_idx >= fd_table.cloexec.len() {
        fd_table.cloexec.resize(newfd_idx + 1, false);
    }

    let old_desc = if newfd_idx < fd_table.entries.len() {
        fd_table.entries[newfd_idx].take()
    } else {
        None
    };

    fd_table.entries[newfd_idx] = Some(file_desc);
    fd_table.cloexec[newfd_idx] = false; // dup2 clears close-on-exec

    if newfd_idx == fd_table.next_free_fd {
        let mut next = newfd_idx + 1;
        while next < fd_table.entries.len() && fd_table.entries[next].is_some() {
            next += 1;
        }
        fd_table.next_free_fd = next;
    }

    drop(fd_table);
    drop(task);

    if let Some(old_desc) = old_desc {
        let mut rc = old_desc.ref_count.lock();
        if *rc > 0 {
            *rc -= 1;
        }
    }

    Some(newfd)
}

/// Duplicate an existing file descriptor `oldfd` onto `newfd` with cloexec flag (dup3).
pub fn current_task_dup3_fd(oldfd: i32, newfd: i32, cloexec: bool) -> Option<i32> {
    if oldfd < 0 || newfd < 0 || oldfd == newfd {
        return None;
    }
    let current_pid = scheduler::current_pid()?;
    let task_arc = scheduler::get_task_arc(current_pid)?;

    let task = task_arc.lock();
    if newfd as u64 >= task.rlimit_nofile_cur {
        return None;
    }
    let mut fd_table = task.fd_table.lock();
    let file_desc = fd_table.entries.get(oldfd as usize)?.as_ref().cloned()?;
    *file_desc.ref_count.lock() += 1;

    let newfd_idx = newfd as usize;
    if newfd_idx >= fd_table.entries.len() {
        fd_table.entries.resize(newfd_idx + 1, None);
    }
    if newfd_idx >= fd_table.cloexec.len() {
        fd_table.cloexec.resize(newfd_idx + 1, false);
    }

    let old_desc = if newfd_idx < fd_table.entries.len() {
        fd_table.entries[newfd_idx].take()
    } else {
        None
    };

    fd_table.entries[newfd_idx] = Some(file_desc);
    fd_table.cloexec[newfd_idx] = cloexec;

    if newfd_idx == fd_table.next_free_fd {
        let mut next = newfd_idx + 1;
        while next < fd_table.entries.len() && fd_table.entries[next].is_some() {
            next += 1;
        }
        fd_table.next_free_fd = next;
    }

    drop(fd_table);
    drop(task);

    if let Some(old_desc) = old_desc {
        let mut rc = old_desc.ref_count.lock();
        if *rc > 0 {
            *rc -= 1;
        }
    }

    Some(newfd)
}
