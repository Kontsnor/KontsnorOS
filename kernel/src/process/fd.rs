//! File descriptor helpers for the current process.
//!
//! These functions safely access the current task's file descriptor table
//! through the scheduler lock, returning `Arc<dyn InodeOps>` clones so that
//! callers can hold onto the inode across `schedule()` calls without keeping
//! the scheduler lock held.

use alloc::sync::Arc;
use crate::fs::inode::InodeOps;

/// Retrieve the file seek offset for descriptor `fd`.
pub fn get_fd_offset(fd: i32) -> Option<u64> {
    if fd < 0 {
        return None;
    }
    let fd_idx = fd as usize;
    let current_pid = crate::process::scheduler::current_pid()?;
    let sched_lock = crate::process::scheduler::SCHEDULER.lock();
    let scheduler = sched_lock.as_ref()?;
    let task = scheduler.get_task(current_pid)?;
    task.fd_offsets.get(fd_idx).copied()
}

/// Set the file seek offset for descriptor `fd`.
pub fn set_fd_offset(fd: i32, offset: u64) -> Option<()> {
    if fd < 0 {
        return None;
    }
    let fd_idx = fd as usize;
    let current_pid = crate::process::scheduler::current_pid()?;
    let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
    let scheduler = sched_lock.as_mut()?;
    let task = scheduler.get_task_mut(current_pid)?;
    let slot = task.fd_offsets.get_mut(fd_idx)?;
    *slot = offset;
    Some(())
}

/// Retrieve a clone of the inode backing file descriptor `fd` in the
/// currently running task.
///
/// Returns `None` if `fd` is out of range or not open.
pub fn current_task_read_fd(fd: i32) -> Option<Arc<dyn InodeOps>> {
    if fd < 0 {
        return None;
    }
    let fd_idx = fd as usize;

    // We need the current PID, then lock the scheduler to read its fd_table.
    let current_pid = crate::process::scheduler::current_pid()?;

    let sched_lock = crate::process::scheduler::SCHEDULER.lock();
    let scheduler = sched_lock.as_ref()?;
    let task = scheduler.get_task(current_pid)?;

    task.fd_table.get(fd_idx)?.as_ref().cloned()
}

/// Allocate the next free file descriptor slot in the current task's fd_table
/// and store the given inode. Returns the allocated fd number, or `None` on
/// failure (no current task, fd_table full, etc.).
pub fn current_task_alloc_fd(inode: Arc<dyn InodeOps>) -> Option<i32> {
    let current_pid = crate::process::scheduler::current_pid()?;

    let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
    let scheduler = sched_lock.as_mut()?;
    let task = scheduler.get_task_mut(current_pid)?;

    // Find first free slot (first None entry)
    for (i, slot) in task.fd_table.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(inode);
            if i < task.fd_offsets.len() {
                task.fd_offsets[i] = 0;
            } else {
                task.fd_offsets.resize(i + 1, 0);
            }
            return Some(i as i32);
        }
    }

    // No free slot found — extend the table (up to a hard limit of 1024)
    if task.fd_table.len() < 1024 {
        task.fd_table.push(Some(inode));
        task.fd_offsets.push(0);
        Some((task.fd_table.len() - 1) as i32)
    } else {
        None // EMFILE
    }
}

/// Close file descriptor `fd` in the current task's fd_table.
///
/// Returns `true` if the fd was open and has been closed.
pub fn current_task_close_fd(fd: i32) -> bool {
    if fd < 0 {
        return false;
    }
    let fd_idx = fd as usize;

    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return false,
    };

    let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
    let scheduler = match sched_lock.as_mut() {
        Some(s) => s,
        None => return false,
    };
    let task = match scheduler.get_task_mut(current_pid) {
        Some(t) => t,
        None => return false,
    };

    if let Some(slot) = task.fd_table.get_mut(fd_idx) {
        if slot.is_some() {
            *slot = None;
            if fd_idx < task.fd_offsets.len() {
                task.fd_offsets[fd_idx] = 0;
            }
            return true;
        }
    }
    false
}

/// Duplicate an existing file descriptor `fd` in the current task's fd_table.
///
/// Returns the new file descriptor number, or `None` on failure.
pub fn current_task_dup_fd(fd: i32) -> Option<i32> {
    if fd < 0 {
        return None;
    }
    let fd_idx = fd as usize;
    let current_pid = crate::process::scheduler::current_pid()?;

    let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
    let scheduler = sched_lock.as_mut()?;
    let task = scheduler.get_task_mut(current_pid)?;

    let inode = task.fd_table.get(fd_idx)?.as_ref().cloned()?;
    let offset = task.fd_offsets.get(fd_idx).copied().unwrap_or(0);

    // Find first free slot (first None entry)
    for (i, slot) in task.fd_table.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(inode);
            if i < task.fd_offsets.len() {
                task.fd_offsets[i] = offset;
            } else {
                task.fd_offsets.resize(i + 1, offset);
            }
            return Some(i as i32);
        }
    }

    // No free slot found — extend the table (up to a hard limit of 1024)
    if task.fd_table.len() < 1024 {
        task.fd_table.push(Some(inode));
        task.fd_offsets.push(offset);
        Some((task.fd_table.len() - 1) as i32)
    } else {
        None
    }
}

/// Duplicate an existing file descriptor `oldfd` onto `newfd` in the current task's fd_table.
///
/// If `newfd` was already open, it is closed first. If `oldfd == newfd`, it just returns `newfd`
/// without closing.
/// Returns the new file descriptor number, or `None` on failure.
pub fn current_task_dup2_fd(oldfd: i32, newfd: i32) -> Option<i32> {
    if oldfd < 0 || newfd < 0 || newfd >= 1024 {
        return None;
    }
    if oldfd == newfd {
        let current_pid = crate::process::scheduler::current_pid()?;
        let sched_lock = crate::process::scheduler::SCHEDULER.lock();
        let scheduler = sched_lock.as_ref()?;
        let task = scheduler.get_task(current_pid)?;
        if task.fd_table.get(oldfd as usize)?.as_ref().is_some() {
            return Some(newfd);
        } else {
            return None;
        }
    }

    let current_pid = crate::process::scheduler::current_pid()?;

    let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
    let scheduler = sched_lock.as_mut()?;
    let task = scheduler.get_task_mut(current_pid)?;

    let inode = task.fd_table.get(oldfd as usize)?.as_ref().cloned()?;
    let offset = task.fd_offsets.get(oldfd as usize).copied().unwrap_or(0);

    // Ensure fd_table and offsets are large enough to contain newfd
    let newfd_idx = newfd as usize;
    if newfd_idx >= task.fd_table.len() {
        task.fd_table.resize(newfd_idx + 1, None);
    }
    if newfd_idx >= task.fd_offsets.len() {
        task.fd_offsets.resize(newfd_idx + 1, 0);
    }

    task.fd_table[newfd_idx] = Some(inode);
    task.fd_offsets[newfd_idx] = offset;
    Some(newfd)
}
