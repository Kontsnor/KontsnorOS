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

//! Process credentials and session system calls.

use super::super::{Errno, SyscallResult};
use crate::process::pid::Pid;
use crate::process::scheduler;
use crate::syscall::validation::{
    copy_from_user, copy_from_user_slice, copy_to_user, copy_to_user_slice,
};

/// `getpid()` — Get the process ID of the calling process.
pub fn sys_getpid() -> SyscallResult {
    match scheduler::current_pid() {
        Some(pid) => {
            if let Some(task_arc) = scheduler::get_task_arc(pid) {
                task_arc.lock().tgid.as_u64() as SyscallResult
            } else {
                pid.as_u64() as SyscallResult
            }
        }
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
        task.sid = current_pid.as_u64();
        return current_pid.as_u64() as SyscallResult;
    }
    Errno::ESRCH.into()
}

/// `gettid()` — Get thread ID (alias to getpid).
pub fn sys_gettid() -> SyscallResult {
    match scheduler::current_pid() {
        Some(pid) => pid.as_u64() as SyscallResult,
        None => 0,
    }
}

/// `getpgrp()` — Get process group ID of calling process.
pub fn sys_getpgrp() -> SyscallResult {
    sys_getpgid(0)
}

/// `getgroups(size, list)` — Get list of supplementary group IDs.
pub fn sys_getgroups(size: i32, list: *mut u32) -> SyscallResult {
    if size < 0 {
        return Errno::EINVAL.into();
    }
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let task = task_arc.lock();
    let count = if task.groups.is_empty() {
        1
    } else {
        task.groups.len()
    };

    if size == 0 {
        return count as SyscallResult;
    }

    if (size as usize) < count {
        return Errno::EINVAL.into();
    }

    if task.groups.is_empty() {
        if let Err(err) = copy_to_user(list, &task.gid) {
            return err.into();
        }
    } else if let Err(err) = copy_to_user_slice(list, &task.groups) {
        return err.into();
    }

    count as SyscallResult
}

/// `setgroups(size, list)` — Set list of supplementary group IDs.
pub fn sys_setgroups(size: usize, list: *const u32) -> SyscallResult {
    if size > 32 {
        return Errno::EINVAL.into();
    }
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let mut task = task_arc.lock();
    if task.euid != 0 {
        return Errno::EPERM.into();
    }

    if size == 0 {
        task.groups.clear();
        return 0;
    }

    let mut new_groups = alloc::vec![0u32; size];
    if let Err(err) = copy_from_user_slice(list, &mut new_groups) {
        return err.into();
    }
    task.groups = new_groups;
    0
}

/// `getresuid(ruid, euid, suid)` — Get real, effective, and saved user IDs.
pub fn sys_getresuid(ruid: *mut u32, euid: *mut u32, suid: *mut u32) -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let task = task_arc.lock();

    if !ruid.is_null() {
        if let Err(err) = copy_to_user(ruid, &task.uid) {
            return err.into();
        }
    }
    if !euid.is_null() {
        if let Err(err) = copy_to_user(euid, &task.euid) {
            return err.into();
        }
    }
    if !suid.is_null() {
        if let Err(err) = copy_to_user(suid, &task.suid) {
            return err.into();
        }
    }
    0
}

/// `setresuid(ruid, euid, suid)` — Set real, effective, and saved user IDs.
pub fn sys_setresuid(ruid: u32, euid: u32, suid: u32) -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let mut task = task_arc.lock();

    // Check permissions
    if task.euid != 0 {
        if ruid != u32::MAX && ruid != task.uid && ruid != task.euid && ruid != task.suid {
            return Errno::EPERM.into();
        }
        if euid != u32::MAX && euid != task.uid && euid != task.euid && euid != task.suid {
            return Errno::EPERM.into();
        }
        if suid != u32::MAX && suid != task.uid && suid != task.euid && suid != task.suid {
            return Errno::EPERM.into();
        }
    }

    if ruid != u32::MAX {
        task.uid = ruid;
    }
    if euid != u32::MAX {
        task.euid = euid;
    }
    if suid != u32::MAX {
        task.suid = suid;
    }
    0
}

/// `getresgid(rgid, egid, sgid)` — Get real, effective, and saved group IDs.
pub fn sys_getresgid(rgid: *mut u32, egid: *mut u32, sgid: *mut u32) -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let task = task_arc.lock();

    if !rgid.is_null() {
        if let Err(err) = copy_to_user(rgid, &task.gid) {
            return err.into();
        }
    }
    if !egid.is_null() {
        if let Err(err) = copy_to_user(egid, &task.egid) {
            return err.into();
        }
    }
    if !sgid.is_null() {
        if let Err(err) = copy_to_user(sgid, &task.sgid) {
            return err.into();
        }
    }
    0
}

/// `setresgid(rgid, egid, sgid)` — Set real, effective, and saved group IDs.
pub fn sys_setresgid(rgid: u32, egid: u32, sgid: u32) -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let mut task = task_arc.lock();

    // Check permissions
    if task.euid != 0 {
        if rgid != u32::MAX && rgid != task.gid && rgid != task.egid && rgid != task.sgid {
            return Errno::EPERM.into();
        }
        if egid != u32::MAX && egid != task.gid && egid != task.egid && egid != task.sgid {
            return Errno::EPERM.into();
        }
        if sgid != u32::MAX && sgid != task.gid && sgid != task.egid && sgid != task.sgid {
            return Errno::EPERM.into();
        }
    }

    if rgid != u32::MAX {
        task.gid = rgid;
    }
    if egid != u32::MAX {
        task.egid = egid;
    }
    if sgid != u32::MAX {
        task.sgid = sgid;
    }
    0
}

/// `setreuid(ruid, euid)` — Set real and/or effective user ID.
pub fn sys_setreuid(ruid: u32, euid: u32) -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let mut task = task_arc.lock();

    if task.euid != 0 {
        if ruid != u32::MAX && ruid != task.uid && ruid != task.euid {
            return Errno::EPERM.into();
        }
        if euid != u32::MAX && euid != task.uid && euid != task.euid && euid != task.suid {
            return Errno::EPERM.into();
        }
    }

    if ruid != u32::MAX {
        task.uid = ruid;
    }
    if euid != u32::MAX {
        task.euid = euid;
    }
    0
}

/// `setregid(rgid, egid)` — Set real and/or effective group ID.
pub fn sys_setregid(rgid: u32, egid: u32) -> SyscallResult {
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let mut task = task_arc.lock();

    if task.euid != 0 {
        if rgid != u32::MAX && rgid != task.gid && rgid != task.egid {
            return Errno::EPERM.into();
        }
        if egid != u32::MAX && egid != task.gid && egid != task.egid && egid != task.sgid {
            return Errno::EPERM.into();
        }
    }

    if rgid != u32::MAX {
        task.gid = rgid;
    }
    if egid != u32::MAX {
        task.egid = egid;
    }
    0
}

/// `getsid(pid)` — Get process session ID.
pub fn sys_getsid(pid: i32) -> SyscallResult {
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        Pid::from_raw(pid as u64)
    };

    if let Some(task_arc) = scheduler::get_task_arc(target_pid) {
        let task = task_arc.lock();
        task.sid as SyscallResult
    } else {
        Errno::ESRCH.into()
    }
}

/// Linux capability structures
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CapUserHeader {
    pub version: u32,
    pub pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CapUserData {
    pub effective: u32,
    pub permitted: u32,
    pub inheritable: u32,
}

pub const LINUX_CAPABILITY_VERSION_1: u32 = 0x19980330;
pub const LINUX_CAPABILITY_VERSION_2: u32 = 0x20071026;
pub const LINUX_CAPABILITY_VERSION_3: u32 = 0x20080522;

/// `capget(hdrp, datap)` — Get process capabilities.
pub fn sys_capget(hdrp: *mut CapUserHeader, datap: *mut CapUserData) -> SyscallResult {
    let mut header = match copy_from_user(hdrp as *const CapUserHeader) {
        Ok(hdr) => hdr,
        Err(err) => return err.into(),
    };

    if header.version != LINUX_CAPABILITY_VERSION_1
        && header.version != LINUX_CAPABILITY_VERSION_2
        && header.version != LINUX_CAPABILITY_VERSION_3
    {
        // Indicate preferred version in header
        header.version = LINUX_CAPABILITY_VERSION_3;
        let _ = copy_to_user(hdrp, &header);
        return Errno::EINVAL.into();
    }

    if datap.is_null() {
        return 0;
    }

    let entries = if header.version == LINUX_CAPABILITY_VERSION_1 {
        1
    } else {
        2
    };

    let is_root = sys_geteuid() == 0;
    let cap_val = if is_root { 0xFFFF_FFFF } else { 0 };

    let cap_data = [
        CapUserData {
            effective: cap_val,
            permitted: cap_val,
            inheritable: 0,
        },
        CapUserData {
            effective: cap_val,
            permitted: cap_val,
            inheritable: 0,
        },
    ];

    if let Err(err) = copy_to_user_slice(datap, &cap_data[..entries]) {
        return err.into();
    }

    0
}

/// `capset(hdrp, datap)` — Set process capabilities.
pub fn sys_capset(hdrp: *const CapUserHeader, datap: *const CapUserData) -> SyscallResult {
    let header = match copy_from_user(hdrp) {
        Ok(hdr) => hdr,
        Err(err) => return err.into(),
    };

    if header.version != LINUX_CAPABILITY_VERSION_1
        && header.version != LINUX_CAPABILITY_VERSION_2
        && header.version != LINUX_CAPABILITY_VERSION_3
    {
        return Errno::EINVAL.into();
    }

    let entries = if header.version == LINUX_CAPABILITY_VERSION_1 {
        1
    } else {
        2
    };

    let mut user_caps = [CapUserData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];

    if let Err(err) = copy_from_user_slice(datap, &mut user_caps[..entries]) {
        return err.into();
    }

    // Unprivileged users cannot raise capabilities
    if sys_geteuid() != 0 {
        return Errno::EPERM.into();
    }

    0
}

/// `setfsuid(fsuid)` — Set filesystem UID. Returns previous fsuid.
pub fn sys_setfsuid(_fsuid: u32) -> SyscallResult {
    sys_getuid()
}

/// `setfsgid(fsgid)` — Set filesystem GID. Returns previous fsgid.
pub fn sys_setfsgid(_fsgid: u32) -> SyscallResult {
    sys_getgid()
}
