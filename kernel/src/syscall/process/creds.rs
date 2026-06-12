//! Process credentials and session system calls.

use super::super::{Errno, SyscallResult};
use crate::process::pid::Pid;
use crate::process::scheduler;

/// `getpid()` — Get the process ID of the calling process.
pub fn sys_getpid() -> SyscallResult {
    match scheduler::current_pid() {
        Some(pid) => pid.as_u64() as SyscallResult,
        None => 0,
    }
}

/// `getuid()` — Get real user ID.
pub fn sys_getuid() -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return 0,
    };
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        return task_arc.lock().uid as SyscallResult;
    }
    0
}

/// `getgid()` — Get real group ID.
pub fn sys_getgid() -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return 0,
    };
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        return task_arc.lock().gid as SyscallResult;
    }
    0
}

/// `geteuid()` — Get effective user ID.
pub fn sys_geteuid() -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return 0,
    };
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        return task_arc.lock().euid as SyscallResult;
    }
    0
}

/// `getegid()` — Get effective group ID.
pub fn sys_getegid() -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return 0,
    };
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        return task_arc.lock().egid as SyscallResult;
    }
    0
}

/// Helper to calculate new EUID/EGID for execve.
pub fn calculate_exec_creds(
    mode: u16,
    file_uid: u32,
    file_gid: u32,
    real_uid: u32,
    real_gid: u32,
) -> (u32, u32) {
    let euid = if mode & 0o4000 != 0 {
        file_uid
    } else {
        real_uid
    };
    let egid = if mode & 0o2000 != 0 {
        file_gid
    } else {
        real_gid
    };
    (euid, egid)
}

/// `setuid(uid)` — Set user ID.
pub fn sys_setuid(uid: u32) -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        let mut task = task_arc.lock();
        if task.euid == 0 {
            task.uid = uid;
            task.euid = uid;
            return 0;
        } else {
            if uid == task.uid {
                task.euid = uid;
                return 0;
            } else {
                return Errno::EPERM.into();
            }
        }
    }
    Errno::ESRCH.into()
}

/// `setgid(gid)` — Set group ID.
pub fn sys_setgid(gid: u32) -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        let mut task = task_arc.lock();
        if task.egid == 0 {
            task.gid = gid;
            task.egid = gid;
            return 0;
        } else {
            if gid == task.gid {
                task.egid = gid;
                return 0;
            } else {
                return Errno::EPERM.into();
            }
        }
    }
    Errno::ESRCH.into()
}

/// `getppid()` — Return the parent PID of the calling process.
pub fn sys_getppid() -> SyscallResult {
    if let Some(pid) = scheduler::current_pid() {
        if let Some(task_arc) = scheduler::get_task_arc(pid) {
            return task_arc.lock().parent_pid.as_u64() as SyscallResult;
        }
    }
    0
}

/// `setpgid(pid, pgid)` — Set the process group ID of a process.
pub fn sys_setpgid(pid: i32, pgid: i32) -> SyscallResult {
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        Pid::from_raw(pid as u64)
    };

    let new_pgid = if pgid == 0 {
        target_pid.as_u64()
    } else {
        pgid as u64
    };

    if let Some(task_arc) = scheduler::get_task_arc(target_pid) {
        task_arc.lock().pgid = new_pgid;
        return 0;
    }
    Errno::ESRCH.into()
}

/// `getpgid(pid)` — Get the process group ID of a process.
pub fn sys_getpgid(pid: i32) -> SyscallResult {
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        Pid::from_raw(pid as u64)
    };

    if let Some(task_arc) = scheduler::get_task_arc(target_pid) {
        return task_arc.lock().pgid as SyscallResult;
    }
    Errno::ESRCH.into()
}

/// `setsid()` — Create a new session and set the process group ID.
pub fn sys_setsid() -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        let mut task = task_arc.lock();
        task.pgid = current_pid.as_u64();
        return current_pid.as_u64() as SyscallResult;
    }
    Errno::ESRCH.into()
}

/// `gettid()` — Get thread ID (alias to getpid).
pub fn sys_gettid() -> SyscallResult {
    sys_getpid()
}
