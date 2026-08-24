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

    // Find first free slot (first None entry)
    for (i, slot) in fd_table.entries.iter_mut().enumerate() {
        if i >= task.rlimit_nofile_cur as usize {
            return None;
        }
        if slot.is_none() {
            *slot = Some(file_desc);
            if i >= fd_table.cloexec.len() {
                fd_table.cloexec.resize(i + 1, false);
            }
            fd_table.cloexec[i] = (flags.0 & OpenFlags::O_CLOEXEC) != 0;
            return Some(i as i32);
        }
    }

    // No free slot found — extend the table up to rlimit_nofile_cur
    let next_idx = fd_table.entries.len();
    if next_idx < task.rlimit_nofile_cur as usize {
        fd_table.entries.push(Some(file_desc));
        fd_table.cloexec.resize(next_idx + 1, false);
        fd_table.cloexec[next_idx] = (flags.0 & OpenFlags::O_CLOEXEC) != 0;
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

    // Find first free slot (first None entry)
    for (i, slot) in fd_table.entries.iter_mut().enumerate() {
        if i >= task.rlimit_nofile_cur as usize {
            return None;
        }
        if slot.is_none() {
            *slot = Some(file_desc);
            if i >= fd_table.cloexec.len() {
                fd_table.cloexec.resize(i + 1, false);
            }
            fd_table.cloexec[i] = false; // dup clears close-on-exec
            return Some(i as i32);
        }
    }

    // No free slot found — extend the table up to rlimit_nofile_cur
    let next_idx = fd_table.entries.len();
    if next_idx < task.rlimit_nofile_cur as usize {
        fd_table.entries.push(Some(file_desc));
        fd_table.cloexec.resize(next_idx + 1, false);
        fd_table.cloexec[next_idx] = false; // dup clears close-on-exec
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

    // If newfd was already open, decrement its ref count
    if let Some(ref old_desc) = fd_table.entries[newfd_idx] {
        let mut rc = old_desc.ref_count.lock();
        if *rc > 0 {
            *rc -= 1;
        }
    }

    fd_table.entries[newfd_idx] = Some(file_desc);
    fd_table.cloexec[newfd_idx] = false; // dup2 clears close-on-exec
    Some(newfd)
}
