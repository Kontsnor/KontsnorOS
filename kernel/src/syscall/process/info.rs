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

//! Process system information, resource limits, and time syscalls.

use super::super::{Errno, SyscallResult};
use crate::process::scheduler;
use crate::syscall::validation::{validate_user_ptr, validate_user_ptr_write};
use alloc::string::String;

/// Linux `uname` struct (sys/utsname.h), each field is 65 bytes.
#[repr(C)]
struct UtsName {
    sysname: [u8; 65],
    nodename: [u8; 65],
    release: [u8; 65],
    version: [u8; 65],
    machine: [u8; 65],
    domainname: [u8; 65],
}

static HOSTNAME: spin::Mutex<alloc::string::String> =
    spin::Mutex::new(alloc::string::String::new());
static DOMAINNAME: spin::Mutex<alloc::string::String> =
    spin::Mutex::new(alloc::string::String::new());

/// `uname(buf)` — Write kernel identity information into a `utsname` struct.
///
/// Reads `hostname` and `domainname` from the calling task's UTS namespace so
/// that containers that called `unshare(CLONE_NEWUTS)` + `sethostname()` get
/// their own view of the system identity.
pub fn sys_uname(buf: *mut u8) -> SyscallResult {
    if buf.is_null() {
        return Errno::EFAULT.into();
    }
    // UtsName is 6 × 65 = 390 bytes
    if validate_user_ptr_write(buf, core::mem::size_of::<UtsName>()).is_err() {
        return Errno::EFAULT.into();
    }

    let mut u = UtsName {
        sysname: [0u8; 65],
        nodename: [0u8; 65],
        release: [0u8; 65],
        version: [0u8; 65],
        machine: [0u8; 65],
        domainname: [0u8; 65],
    };

    // Helper: copy a &str into a fixed [u8;65], null-terminated.
    fn fill(dst: &mut [u8; 65], s: &[u8]) {
        let len = s.len().min(64);
        dst[..len].copy_from_slice(&s[..len]);
        dst[len] = 0;
    }

    fill(&mut u.sysname, b"Linux");
    fill(&mut u.release, b"6.1.0-KontsnorOS");
    fill(&mut u.version, b"#1 SMP");
    fill(&mut u.machine, b"x86_64");

    // Read hostname/domainname from the task's per-task UTS namespace.
    let (hostname, domainname) = if let Some(pid) = scheduler::current_pid() {
        if let Some(task_arc) = scheduler::get_task_arc(pid) {
            let task = task_arc.lock();
            (task.uts_ns.hostname.clone(), task.uts_ns.domainname.clone())
        } else {
            (String::from("kontsnoros"), String::from("(none)"))
        }
    } else {
        (String::from("kontsnoros"), String::from("(none)"))
    };

    if hostname.is_empty() {
        fill(&mut u.nodename, b"kontsnoros");
    } else {
        fill(&mut u.nodename, hostname.as_bytes());
    }
    if domainname.is_empty() {
        fill(&mut u.domainname, b"(none)");
    } else {
        fill(&mut u.domainname, domainname.as_bytes());
    }

    // SAFETY: buf has been validated above via validate_user_ptr_write.
    unsafe {
        core::ptr::write(buf as *mut UtsName, u);
    }
    0
}

/// `timeval` struct used by `gettimeofday`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeVal {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// `timezone` struct used by `gettimeofday`.
#[repr(C)]
struct TimeZone {
    tz_minuteswest: i32,
    tz_dsttime: i32,
}

/// Boot-time Unix timestamp (seconds since epoch), read from the CMOS RTC
/// during early kernel init by `init_boot_time()`. All `CLOCK_REALTIME`
/// values are computed as `BOOT_REALTIME_SEC + monotonic_elapsed`.
static BOOT_REALTIME_SEC: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Initialise the wall-clock base from the CMOS Real-Time Clock.
///
/// Must be called once during early kernel boot (before any time syscall can
/// be made) and after I/O port access is available (i.e. after GDT/IDT init).
pub fn init_boot_time() {
    let unix_sec = crate::arch::x86_64::boot::read_rtc_unix_time();
    BOOT_REALTIME_SEC.store(unix_sec, core::sync::atomic::Ordering::Relaxed);
    crate::kprintln!("[time] RTC boot time: {} s (Unix epoch)", unix_sec);
}

/// Return the boot-time Unix timestamp (seconds since epoch) read from the CMOS RTC.
///
/// Used by any kernel subsystem that needs wall-clock seconds without going
/// through the full `clock_gettime` syscall path.
#[inline]
pub fn boot_realtime_sec() -> u64 {
    BOOT_REALTIME_SEC.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn get_monotonic_ns() -> u64 {
    let ticks = crate::arch::x86_64::interrupts::timer_ticks();
    let current_count = crate::arch::x86_64::apic::get_lapic_timer_current() as u64;
    let init_count = 10_000_000;
    let sub_tick = if current_count <= init_count {
        init_count - current_count
    } else {
        0
    };
    ticks * 10_000_000 + sub_tick
}

pub fn get_realtime_ns() -> u64 {
    let boot_sec = BOOT_REALTIME_SEC.load(core::sync::atomic::Ordering::Relaxed);
    boot_sec * 1_000_000_000 + get_monotonic_ns()
}

/// `timespec` struct used by `clock_gettime` and `nanosleep`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TimeSpec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

/// `gettimeofday(tv, tz)` — Return current time-of-day.
pub fn sys_gettimeofday(tv: *mut u8, tz: *mut u8) -> SyscallResult {
    if !tv.is_null() {
        if validate_user_ptr_write(tv, core::mem::size_of::<TimeVal>()).is_err() {
            return Errno::EFAULT.into();
        }
        let boot_sec = BOOT_REALTIME_SEC.load(core::sync::atomic::Ordering::Relaxed);
        let realtime_ns = boot_sec * 1_000_000_000 + get_monotonic_ns();
        let t = TimeVal {
            tv_sec: (realtime_ns / 1_000_000_000) as i64,
            tv_usec: ((realtime_ns % 1_000_000_000) / 1000) as i64,
        };
        // SAFETY: The pointer was validated with validate_user_ptr_write and is safe to write.
        unsafe {
            core::ptr::write(tv as *mut TimeVal, t);
        }
    }
    if !tz.is_null() {
        if validate_user_ptr_write(tz, core::mem::size_of::<TimeZone>()).is_err() {
            return Errno::EFAULT.into();
        }
        let z = TimeZone {
            tz_minuteswest: 0,
            tz_dsttime: 0,
        };
        // SAFETY: The pointer was validated with validate_user_ptr_write and is safe to write.
        unsafe {
            core::ptr::write(tz as *mut TimeZone, z);
        }
    }
    0
}

/// `clock_gettime(clockid, tp)` — Return current clock value.
pub fn sys_clock_gettime(clockid: i32, tp: *mut u8) -> SyscallResult {
    if tp.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(tp, core::mem::size_of::<TimeSpec>()).is_err() {
        return Errno::EFAULT.into();
    }

    let ts = match clockid {
        0 => {
            // CLOCK_REALTIME
            let boot_sec = BOOT_REALTIME_SEC.load(core::sync::atomic::Ordering::Relaxed);
            let realtime_ns = boot_sec * 1_000_000_000 + get_monotonic_ns();
            TimeSpec {
                tv_sec: (realtime_ns / 1_000_000_000) as i64,
                tv_nsec: (realtime_ns % 1_000_000_000) as i64,
            }
        }
        1 => {
            // CLOCK_MONOTONIC
            let monotonic_ns = get_monotonic_ns();
            TimeSpec {
                tv_sec: (monotonic_ns / 1_000_000_000) as i64,
                tv_nsec: (monotonic_ns % 1_000_000_000) as i64,
            }
        }
        2 => {
            // CLOCK_PROCESS_CPUTIME_ID
            let cpu_ticks = if let Some(pid) = crate::process::scheduler::current_pid() {
                if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
                    task_arc.lock().cpu_ticks
                } else {
                    0
                }
            } else {
                0
            };
            let cpu_ns = cpu_ticks * 10_000_000;
            TimeSpec {
                tv_sec: (cpu_ns / 1_000_000_000) as i64,
                tv_nsec: (cpu_ns % 1_000_000_000) as i64,
            }
        }
        _ => return Errno::EINVAL.into(),
    };

    // SAFETY: The pointer was validated with validate_user_ptr_write and is safe to write.
    unsafe {
        core::ptr::write(tp as *mut TimeSpec, ts);
    }
    0
}

/// `nanosleep(req, rem)` — High-resolution sleep.
pub fn sys_nanosleep(req: *const u8, rem: *mut u8) -> SyscallResult {
    if req.is_null() {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(req, core::mem::size_of::<TimeSpec>()) {
        return Errno::EFAULT.into();
    }
    let ts = unsafe { core::ptr::read(req as *const TimeSpec) };
    if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
        return Errno::EINVAL.into();
    }

    let sleep_ns = (ts.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec as u64);
    let start_ns = get_monotonic_ns();
    let end_ns = start_ns.saturating_add(sleep_ns);

    sleep_until(end_ns, rem)
}

/// Helper to put the current task to sleep until an absolute monotonic deadline in nanoseconds.
pub fn sleep_until(end_ns: u64, rem: *mut u8) -> SyscallResult {
    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let mut now = get_monotonic_ns();
    if now >= end_ns {
        if !rem.is_null() {
            if validate_user_ptr_write(rem, core::mem::size_of::<TimeSpec>()).is_err() {
                return Errno::EFAULT.into();
            }
            let zero_ts = TimeSpec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            unsafe {
                core::ptr::write(rem as *mut TimeSpec, zero_ts);
            }
        }
        return 0;
    }

    // Register sleep timer and block task
    x86_64::instructions::interrupts::without_interrupts(|| {
        crate::process::scheduler::register_sleep_timer(current_pid, end_ns);
        let sched_lock = crate::process::scheduler::SCHEDULER.lock();
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
            task_arc.lock().state = crate::process::task::TaskState::Blocked;
        }
        drop(sched_lock);
    });

    // Schedule other tasks (or idle task which executes HLT!)
    crate::process::scheduler::schedule();

    // Woken up: remove timer in case it was woken early by signal
    crate::process::scheduler::remove_sleep_timer(current_pid);

    now = get_monotonic_ns();

    // Check if an unblocked signal woke us early
    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        let task = task_arc.lock();
        let unblocked = task.pending_signals & !task.blocked_signals;
        if unblocked != 0 && now < end_ns {
            let remaining_ns = end_ns.saturating_sub(now);
            if !rem.is_null() {
                if validate_user_ptr_write(rem, core::mem::size_of::<TimeSpec>()).is_ok() {
                    let remaining_ts = TimeSpec {
                        tv_sec: (remaining_ns / 1_000_000_000) as i64,
                        tv_nsec: (remaining_ns % 1_000_000_000) as i64,
                    };
                    unsafe {
                        core::ptr::write(rem as *mut TimeSpec, remaining_ts);
                    }
                }
            }
            return Errno::EINTR.into();
        }
    }

    if !rem.is_null() {
        if validate_user_ptr_write(rem, core::mem::size_of::<TimeSpec>()).is_err() {
            return Errno::EFAULT.into();
        }
        let zero_ts = TimeSpec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            core::ptr::write(rem as *mut TimeSpec, zero_ts);
        }
    }

    0
}

/// `time(tloc)` — Get time in seconds since the Epoch.
pub fn sys_time(tloc: *mut i64) -> SyscallResult {
    let boot_sec = BOOT_REALTIME_SEC.load(core::sync::atomic::Ordering::Relaxed);
    let sec = (boot_sec + get_monotonic_ns() / 1_000_000_000) as i64;
    if !tloc.is_null() {
        if validate_user_ptr_write(tloc as *mut u8, core::mem::size_of::<i64>()).is_err() {
            return Errno::EFAULT.into();
        }
        unsafe {
            core::ptr::write(tloc, sec);
        }
    }
    sec
}

/// `tms` struct used by `times`.
#[repr(C)]
struct Tms {
    tms_utime: i64,
    tms_stime: i64,
    tms_cutime: i64,
    tms_cstime: i64,
}

/// `times(buf)` — Return process and children CPU usage times.
pub fn sys_times(buf: *mut u8) -> SyscallResult {
    if !buf.is_null() {
        if validate_user_ptr_write(buf, core::mem::size_of::<Tms>()).is_err() {
            return Errno::EFAULT.into();
        }
        let t = Tms {
            tms_utime: 0,
            tms_stime: 0,
            tms_cutime: 0,
            tms_cstime: 0,
        };
        unsafe {
            core::ptr::write(buf as *mut Tms, t);
        }
    }
    0
}

/// `rlimit` struct used by `getrlimit`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct RLimit {
    rlim_cur: u64, // soft limit
    rlim_max: u64, // hard limit
}

const RLIM_INFINITY: u64 = !0u64;

/// `getrlimit(resource, rlim)` — Get resource limits.
pub fn sys_getrlimit(resource: i32, rlim: *mut u8) -> SyscallResult {
    if rlim.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(rlim, core::mem::size_of::<RLimit>()).is_err() {
        return Errno::EFAULT.into();
    }

    let limit = match resource {
        0 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_CPU
        1 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_FSIZE
        2 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_DATA
        3 => RLimit {
            rlim_cur: 8 * 1024 * 1024,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_STACK (8 MiB)
        4 => RLimit {
            rlim_cur: 0,
            rlim_max: 0,
        }, // RLIMIT_CORE
        5 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_RSS
        6 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_NPROC
        7 => {
            let (cur, max) = if let Some(pid) = crate::process::scheduler::current_pid() {
                if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
                    let task = task_arc.lock();
                    (task.rlimit_nofile_cur, task.rlimit_nofile_max)
                } else {
                    (1024, 4096)
                }
            } else {
                (1024, 4096)
            };
            RLimit {
                rlim_cur: cur,
                rlim_max: max,
            }
        } // RLIMIT_NOFILE
        8 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_MEMLOCK
        9 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_AS
        10 => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        }, // RLIMIT_LOCKS
        _ => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        },
    };
    unsafe {
        core::ptr::write(rlim as *mut RLimit, limit);
    }
    0
}

/// `setrlimit(resource, rlim)` — Set resource limits.
pub fn sys_setrlimit(resource: i32, rlim: *const u8) -> SyscallResult {
    if rlim.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(rlim, core::mem::size_of::<RLimit>()) {
        return Errno::EFAULT.into();
    }
    // SAFETY: The pointer has been checked for nullity and validated using validate_user_ptr for the size of RLimit.
    let limit = unsafe { *(rlim as *const RLimit) };

    if resource == 7 {
        // RLIMIT_NOFILE
        let current_pid = match crate::process::scheduler::current_pid() {
            Some(pid) => pid,
            None => return Errno::ESRCH.into(),
        };
        let task_arc = match crate::process::scheduler::get_task_arc(current_pid) {
            Some(arc) => arc,
            None => return Errno::ESRCH.into(),
        };
        let mut task = task_arc.lock();
        if limit.rlim_cur > limit.rlim_max {
            return Errno::EINVAL.into();
        }
        task.rlimit_nofile_cur = limit.rlim_cur;
        task.rlimit_nofile_max = limit.rlim_max;
    }
    0
}

/// `sysinfo` struct (linux/sysinfo.h).
#[repr(C)]
struct SysInfo {
    uptime: i64,
    loads: [u64; 3],
    totalram: u64,
    freeram: u64,
    sharedram: u64,
    bufferram: u64,
    totalswap: u64,
    freeswap: u64,
    procs: u16,
    pad: u16,
    totalhigh: u64,
    freehigh: u64,
    mem_unit: u32,
    _pad2: [u8; 4],
}

/// `sysinfo(info)` — Return overall system information.
pub fn sys_sysinfo(info: *mut u8) -> SyscallResult {
    if info.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(info, core::mem::size_of::<SysInfo>()).is_err() {
        return Errno::EFAULT.into();
    }
    let (total_frames, _allocated_frames, free_frames) = crate::memory::physical::stats();
    let uptime = (crate::arch::x86_64::interrupts::timer_ticks() / 18) as i64;
    let si = SysInfo {
        uptime,
        loads: [0, 0, 0],
        totalram: (total_frames * 4096) as u64,
        freeram: (free_frames * 4096) as u64,
        sharedram: 0,
        bufferram: 0,
        totalswap: 0,
        freeswap: 0,
        procs: 1,
        pad: 0,
        totalhigh: 0,
        freehigh: 0,
        mem_unit: 1,
        _pad2: [0u8; 4],
    };
    unsafe {
        core::ptr::write(info as *mut SysInfo, si);
    }
    0
}

/// `sigaltstack(ss, old_ss)` — Set/get alternate signal stack.
pub fn sys_sigaltstack(ss_ptr: *const u8, old_ss_ptr: *mut u8, user_rsp: u64) -> SyscallResult {
    use crate::process::scheduler;
    use crate::process::task::StackT;

    if !old_ss_ptr.is_null()
        && validate_user_ptr_write(old_ss_ptr, core::mem::size_of::<StackT>()).is_err()
    {
        return Errno::EFAULT.into();
    }
    if !ss_ptr.is_null() && !validate_user_ptr(ss_ptr, core::mem::size_of::<StackT>()) {
        return Errno::EFAULT.into();
    }

    let current_pid = match scheduler::current_pid() {
        Some(pid) => pid,
        None => return Errno::ESRCH.into(),
    };

    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };

    let mut task = task_arc.lock();

    // 1. If old_ss_ptr is not null, write the current alternate stack configuration
    if !old_ss_ptr.is_null() {
        let mut flags = 0;
        let mut sp = 0;
        let mut size = 0;

        if let Some(ref alt) = task.sigaltstack {
            sp = alt.ss_sp;
            size = alt.ss_size;
            if user_rsp >= alt.ss_sp && user_rsp < alt.ss_sp + alt.ss_size {
                flags |= 1; // SS_ONSTACK
            }
            flags |= alt.ss_flags & 2; // SS_DISABLE
        } else {
            flags = 2; // SS_DISABLE
        }

        let old_ss = StackT {
            ss_sp: sp,
            ss_flags: flags,
            _pad: 0,
            ss_size: size,
        };

        // SAFETY: old_ss_ptr is non-null and was verified writable with size_of::<StackT>() bytes above.
        unsafe {
            core::ptr::write(old_ss_ptr as *mut StackT, old_ss);
        }
    }

    // 2. If ss_ptr is not null, update the alternate stack configuration
    if !ss_ptr.is_null() {
        // SAFETY: ss_ptr is non-null and was validated user pointer with size_of::<StackT>() bytes above.
        let ss = unsafe { *(ss_ptr as *const StackT) };

        // Check if we are currently executing on the alternate stack
        if let Some(ref alt) = task.sigaltstack {
            if user_rsp >= alt.ss_sp && user_rsp < alt.ss_sp + alt.ss_size {
                return Errno::EPERM.into(); // Cannot change stack while executing on it
            }
        }

        const SS_DISABLE: i32 = 2;
        if (ss.ss_flags & !SS_DISABLE) != 0 {
            return Errno::EINVAL.into(); // Invalid flags
        }

        if (ss.ss_flags & SS_DISABLE) != 0 {
            // Disable alternate stack
            task.sigaltstack = Some(StackT {
                ss_sp: 0,
                ss_flags: SS_DISABLE,
                _pad: 0,
                ss_size: 0,
            });
        } else {
            // Enable/set alternate stack
            // Check size (must be >= MINSIGSTKSZ, typically 2048)
            if ss.ss_size < 2048 {
                return Errno::ENOMEM.into();
            }
            task.sigaltstack = Some(ss);
        }
    }

    0 // Success
}

/// `getrandom(buf, buflen, flags)` — Get random bytes.
pub fn sys_getrandom(buf: *mut u8, buflen: usize, _flags: u32) -> SyscallResult {
    if buf.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(buf, buflen) {
        return Errno::EFAULT.into();
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf, buflen) };
    if !crate::crypto::prng::fill_bytes(slice) {
        return Errno::EAGAIN.into();
    }
    buflen as SyscallResult
}

/// `prlimit64(pid, resource, new_limit, old_limit)` — Get/set resource limits.
pub fn sys_prlimit64(
    _pid: i32,
    resource: i32,
    new_limit: *const u8,
    old_limit: *mut u8,
) -> SyscallResult {
    let target_pid = if _pid == 0 {
        match crate::process::scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        crate::process::pid::Pid::from_raw(_pid as u64)
    };

    if !old_limit.is_null() {
        if validate_user_ptr_write(old_limit, core::mem::size_of::<RLimit>()).is_err() {
            return Errno::EFAULT.into();
        }
        let limit = match resource {
            7 => {
                if let Some(task_arc) = crate::process::scheduler::get_task_arc(target_pid) {
                    let task = task_arc.lock();
                    RLimit {
                        rlim_cur: task.rlimit_nofile_cur,
                        rlim_max: task.rlimit_nofile_max,
                    }
                } else {
                    return Errno::ESRCH.into();
                }
            }
            0 => RLimit {
                rlim_cur: RLIM_INFINITY,
                rlim_max: RLIM_INFINITY,
            }, // RLIMIT_CPU
            1 => RLimit {
                rlim_cur: RLIM_INFINITY,
                rlim_max: RLIM_INFINITY,
            }, // RLIMIT_FSIZE
            2 => RLimit {
                rlim_cur: RLIM_INFINITY,
                rlim_max: RLIM_INFINITY,
            }, // RLIMIT_DATA
            3 => RLimit {
                rlim_cur: 8 * 1024 * 1024,
                rlim_max: RLIM_INFINITY,
            }, // RLIMIT_STACK
            4 => RLimit {
                rlim_cur: 0,
                rlim_max: 0,
            }, // RLIMIT_CORE
            5 => RLimit {
                rlim_cur: RLIM_INFINITY,
                rlim_max: RLIM_INFINITY,
            }, // RLIMIT_RSS
            6 => RLimit {
                rlim_cur: RLIM_INFINITY,
                rlim_max: RLIM_INFINITY,
            }, // RLIMIT_NPROC
            8 => RLimit {
                rlim_cur: RLIM_INFINITY,
                rlim_max: RLIM_INFINITY,
            }, // RLIMIT_MEMLOCK
            9 => RLimit {
                rlim_cur: RLIM_INFINITY,
                rlim_max: RLIM_INFINITY,
            }, // RLIMIT_AS
            10 => RLimit {
                rlim_cur: RLIM_INFINITY,
                rlim_max: RLIM_INFINITY,
            }, // RLIMIT_LOCKS
            _ => RLimit {
                rlim_cur: RLIM_INFINITY,
                rlim_max: RLIM_INFINITY,
            },
        };
        // SAFETY: The pointer has been checked for nullity and validated using validate_user_ptr_write.
        unsafe {
            core::ptr::write(old_limit as *mut RLimit, limit);
        }
    }

    if !new_limit.is_null() {
        if !validate_user_ptr(new_limit, core::mem::size_of::<RLimit>()) {
            return Errno::EFAULT.into();
        }
        // SAFETY: The pointer has been checked for nullity and validated using validate_user_ptr.
        let limit = unsafe { *(new_limit as *const RLimit) };
        if limit.rlim_cur > limit.rlim_max {
            return Errno::EINVAL.into();
        }
        if resource == 7 {
            // RLIMIT_NOFILE
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(target_pid) {
                let mut task = task_arc.lock();
                task.rlimit_nofile_cur = limit.rlim_cur;
                task.rlimit_nofile_max = limit.rlim_max;
            } else {
                return Errno::ESRCH.into();
            }
        }
    }

    0
}

/// `tgkill(tgid, tid, sig)` — Send signal to thread.
pub fn sys_tgkill(_tgid: i32, tid: i32, sig: i32) -> SyscallResult {
    crate::syscall::signal::sys_kill(tid, sig)
}

/// `tkill(tid, sig)` — Send signal to thread.
pub fn sys_tkill(tid: i32, sig: i32) -> SyscallResult {
    crate::syscall::signal::sys_kill(tid, sig)
}

/// `sched_getaffinity(pid, cpusetsize, mask)` — Get CPU affinity mask.
pub fn sys_sched_getaffinity(_pid: i32, cpusetsize: usize, mask: *mut u8) -> SyscallResult {
    if mask.is_null() {
        return Errno::EFAULT.into();
    }
    if cpusetsize < 8 {
        return Errno::EINVAL.into();
    }
    if validate_user_ptr_write(mask, cpusetsize).is_err() {
        return Errno::EFAULT.into();
    }

    // Zero out the whole mask first
    // SAFETY: The pointer mask is validated with validate_user_ptr_write and has at least cpusetsize bytes.
    unsafe {
        core::ptr::write_bytes(mask, 0, cpusetsize);
    }

    let cpu_count = crate::arch::x86_64::smp::get_cpu_count();
    let mut cpu_mask = 0u64;
    for i in 0..cpu_count.min(64) {
        cpu_mask |= 1 << i;
    }

    // SAFETY: The pointer mask was validated and has a size of at least 8 bytes.
    unsafe {
        *(mask as *mut u64) = cpu_mask;
    }

    8
}

/// `set_robust_list(head, len)` — Set robust futex list head.
pub fn sys_set_robust_list(head: *const u8, len: usize) -> SyscallResult {
    if !head.is_null() {
        if !validate_user_ptr(head, len) {
            return Errno::EFAULT.into();
        }
    }
    if let Some(pid) = scheduler::current_pid() {
        if let Some(task_arc) = scheduler::get_task_arc(pid) {
            let mut task = task_arc.lock();
            task.robust_list_head = head as u64;
            task.robust_list_len = len;
        }
    }
    0
}

/// `get_robust_list(pid, head_ptr, len_ptr)` — Get robust futex list head.
pub fn sys_get_robust_list(pid: i32, head_ptr: *mut *mut u8, len_ptr: *mut usize) -> SyscallResult {
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        crate::process::pid::Pid::from_raw(pid as u64)
    };

    let (head, len) = if let Some(task_arc) = scheduler::get_task_arc(target_pid) {
        let task = task_arc.lock();
        (task.robust_list_head, task.robust_list_len)
    } else {
        return Errno::ESRCH.into();
    };

    if !head_ptr.is_null() {
        if validate_user_ptr_write(head_ptr as *mut u8, core::mem::size_of::<*mut u8>()).is_err() {
            return Errno::EFAULT.into();
        }
        // SAFETY: The pointer was validated using validate_user_ptr_write and is safe to write.
        unsafe {
            head_ptr.write(head as *mut u8);
        }
    }
    if !len_ptr.is_null() {
        if validate_user_ptr_write(len_ptr as *mut u8, core::mem::size_of::<usize>()).is_err() {
            return Errno::EFAULT.into();
        }
        // SAFETY: The pointer was validated using validate_user_ptr_write and is safe to write.
        unsafe {
            len_ptr.write(len);
        }
    }
    0
}

/// `sched_setaffinity(pid, cpusetsize, mask)` — Set CPU affinity mask.
pub fn sys_sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const u8) -> SyscallResult {
    if cpusetsize == 0 || mask.is_null() {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(mask, cpusetsize.min(8)) {
        return Errno::EFAULT.into();
    }
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        crate::process::pid::Pid::from_raw(pid as u64)
    };
    if scheduler::get_task_arc(target_pid).is_none() {
        return Errno::ESRCH.into();
    }
    0
}

/// `sched_getparam(pid, param)` — Get scheduling parameters.
pub fn sys_sched_getparam(pid: i32, param: *mut i32) -> SyscallResult {
    if param.is_null() {
        return Errno::EINVAL.into();
    }
    if validate_user_ptr_write(param as *mut u8, core::mem::size_of::<i32>()).is_err() {
        return Errno::EFAULT.into();
    }
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        crate::process::pid::Pid::from_raw(pid as u64)
    };
    if scheduler::get_task_arc(target_pid).is_none() {
        return Errno::ESRCH.into();
    }
    // SAFETY: Pointer validated with validate_user_ptr_write.
    unsafe { core::ptr::write(param, 0) };
    0
}

/// `sched_setparam(pid, param)` — Set scheduling parameters.
pub fn sys_sched_setparam(pid: i32, param: *const i32) -> SyscallResult {
    if param.is_null() {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(param as *const u8, core::mem::size_of::<i32>()) {
        return Errno::EFAULT.into();
    }
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        crate::process::pid::Pid::from_raw(pid as u64)
    };
    if scheduler::get_task_arc(target_pid).is_none() {
        return Errno::ESRCH.into();
    }
    let prio = unsafe { core::ptr::read(param) };
    if prio != 0 {
        return Errno::EINVAL.into();
    }
    0
}

/// `sched_getscheduler(pid)` — Get scheduling policy.
pub fn sys_sched_getscheduler(pid: i32) -> SyscallResult {
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        crate::process::pid::Pid::from_raw(pid as u64)
    };
    if scheduler::get_task_arc(target_pid).is_none() {
        return Errno::ESRCH.into();
    }
    0 // SCHED_OTHER (standard round-robin / time sharing)
}

/// `sched_setscheduler(pid, policy, param)` — Set scheduling policy and parameters.
pub fn sys_sched_setscheduler(pid: i32, policy: i32, param: *const i32) -> SyscallResult {
    if policy != 0 {
        return Errno::EINVAL.into();
    }
    sys_sched_setparam(pid, param)
}

/// `sched_get_priority_max(policy)` — Get maximum priority value.
pub fn sys_sched_get_priority_max(policy: i32) -> SyscallResult {
    if policy != 0 {
        return Errno::EINVAL.into();
    }
    0
}

/// `sched_get_priority_min(policy)` — Get minimum priority value.
pub fn sys_sched_get_priority_min(policy: i32) -> SyscallResult {
    if policy != 0 {
        return Errno::EINVAL.into();
    }
    0
}

/// `sched_rr_get_interval(pid, tp)` — Get the SCHED_RR interval for the named process.
pub fn sys_sched_rr_get_interval(pid: i32, tp: *mut u8) -> SyscallResult {
    if tp.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(tp, core::mem::size_of::<TimeSpec>()).is_err() {
        return Errno::EFAULT.into();
    }
    let target_pid = if pid == 0 {
        match scheduler::current_pid() {
            Some(p) => p,
            None => return Errno::ESRCH.into(),
        }
    } else {
        crate::process::pid::Pid::from_raw(pid as u64)
    };
    if scheduler::get_task_arc(target_pid).is_none() {
        return Errno::ESRCH.into();
    }
    // KontsnorOS scheduler quantum is 10 milliseconds (10,000,000 ns)
    let ts = TimeSpec {
        tv_sec: 0,
        tv_nsec: 10_000_000,
    };
    // SAFETY: Pointer validated with validate_user_ptr_write.
    unsafe { core::ptr::write(tp as *mut TimeSpec, ts) };
    0
}

/// `getpriority(which, who)` — Get program scheduling priority.
pub fn sys_getpriority(_which: i32, _who: i32) -> SyscallResult {
    20 // Return nice value 0 (represented as 20 - nice in kernel ABI)
}

/// `setpriority(which, who, nice)` — Set program scheduling priority.
pub fn sys_setpriority(_which: i32, _who: i32, _nice: i32) -> SyscallResult {
    0
}

/// `rusage` struct for getrusage
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RUsage {
    pub ru_utime: TimeVal,
    pub ru_stime: TimeVal,
    pub ru_maxrss: i64,
    pub ru_ixrss: i64,
    pub ru_idrss: i64,
    pub ru_isrss: i64,
    pub ru_minflt: i64,
    pub ru_majflt: i64,
    pub ru_nswap: i64,
    pub ru_inblock: i64,
    pub ru_oublock: i64,
    pub ru_msgsnd: i64,
    pub ru_msgrcv: i64,
    pub ru_nsignals: i64,
    pub ru_nvcsw: i64,
    pub ru_nivcsw: i64,
}

/// `getrusage(who, usage)` — Get resource usage.
pub fn sys_getrusage(who: i32, usage: *mut u8) -> SyscallResult {
    if usage.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(usage, core::mem::size_of::<RUsage>()).is_err() {
        return Errno::EFAULT.into();
    }

    if who != 0 && who != -1 && who != 1 {
        // RUSAGE_SELF (0), RUSAGE_CHILDREN (-1), RUSAGE_THREAD (1)
        return Errno::EINVAL.into();
    }

    let cpu_ticks = if let Some(pid) = scheduler::current_pid() {
        if let Some(task_arc) = scheduler::get_task_arc(pid) {
            task_arc.lock().cpu_ticks
        } else {
            0
        }
    } else {
        0
    };

    let total_us = cpu_ticks * 10_000;
    let mut ru = RUsage::default();
    ru.ru_utime = TimeVal {
        tv_sec: (total_us / 1_000_000) as i64,
        tv_usec: (total_us % 1_000_000) as i64,
    };
    ru.ru_stime = TimeVal {
        tv_sec: (total_us / 2_000_000) as i64,
        tv_usec: ((total_us / 2) % 1_000_000) as i64,
    };
    ru.ru_maxrss = 4096; // 4 MiB baseline

    // SAFETY: Pointer validated with validate_user_ptr_write.
    unsafe {
        core::ptr::write(usage as *mut RUsage, ru);
    }
    0
}

/// `getcpu(cpup, nodep, unused)` — Determine CPU and NUMA node on which the calling thread is running.
pub fn sys_getcpu(cpup: *mut u32, nodep: *mut u32, _unused: *mut u8) -> SyscallResult {
    let lapic_id = crate::arch::x86_64::smp::current_lapic_id();
    if !cpup.is_null() {
        if validate_user_ptr_write(cpup as *mut u8, core::mem::size_of::<u32>()).is_err() {
            return Errno::EFAULT.into();
        }
        // SAFETY: Pointer validated with validate_user_ptr_write.
        unsafe { core::ptr::write(cpup, lapic_id as u32) };
    }
    if !nodep.is_null() {
        if validate_user_ptr_write(nodep as *mut u8, core::mem::size_of::<u32>()).is_err() {
            return Errno::EFAULT.into();
        }
        // SAFETY: Pointer validated with validate_user_ptr_write.
        unsafe { core::ptr::write(nodep, 0) };
    }
    0
}

/// `personality(persona)` — Set the process execution domain.
pub fn sys_personality(persona: u64) -> SyscallResult {
    if persona == 0xFFFF_FFFF {
        return 0; // Return current personality: PER_LINUX (0)
    }
    0
}

/// `clock_getres(clock_id, res)` — Find the resolution (precision) of the specified clock.
pub fn sys_clock_getres(clock_id: i32, res: *mut u8) -> SyscallResult {
    if clock_id < 0 || clock_id > 11 {
        return Errno::EINVAL.into();
    }
    if !res.is_null() {
        if validate_user_ptr_write(res, core::mem::size_of::<TimeSpec>()).is_err() {
            return Errno::EFAULT.into();
        }
        // KontsnorOS clock resolution is 1 nanosecond (LAPIC/HPET timer backed)
        let ts = TimeSpec {
            tv_sec: 0,
            tv_nsec: 1,
        };
        // SAFETY: Pointer validated with validate_user_ptr_write.
        unsafe { core::ptr::write(res as *mut TimeSpec, ts) };
    }
    0
}

pub const TIMER_ABSTIME: i32 = 1;

/// `clock_nanosleep(clock_id, flags, req, rem)` — High-resolution sleep with a specified clock.
pub fn sys_clock_nanosleep(
    clock_id: i32,
    flags: i32,
    req: *const u8,
    rem: *mut u8,
) -> SyscallResult {
    if clock_id < 0 || clock_id > 11 {
        return Errno::EINVAL.into();
    }
    if req.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(req, core::mem::size_of::<TimeSpec>()) {
        return Errno::EFAULT.into();
    }

    let ts = unsafe { *(req as *const TimeSpec) };
    if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
        return Errno::EINVAL.into();
    }

    let req_ns = (ts.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec as u64);

    let end_ns = if flags & TIMER_ABSTIME != 0 {
        if clock_id == 0 {
            // CLOCK_REALTIME: req is absolute Unix epoch timestamp
            let boot_sec = BOOT_REALTIME_SEC.load(core::sync::atomic::Ordering::Relaxed);
            let boot_ns = boot_sec.saturating_mul(1_000_000_000);
            if req_ns <= boot_ns {
                0
            } else {
                req_ns - boot_ns
            }
        } else {
            // CLOCK_MONOTONIC or other clock: req is absolute monotonic timestamp
            req_ns
        }
    } else {
        // Relative sleep
        get_monotonic_ns().saturating_add(req_ns)
    };

    sleep_until(end_ns, rem)
}

/// `clock_settime(clock_id, tp)` — Set the specified clock.
pub fn sys_clock_settime(clock_id: i32, tp: *const u8) -> SyscallResult {
    if clock_id < 0 || clock_id > 11 {
        return Errno::EINVAL.into();
    }
    if tp.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(tp, core::mem::size_of::<TimeSpec>()) {
        return Errno::EFAULT.into();
    }
    if crate::syscall::process::sys_geteuid() != 0 {
        return Errno::EPERM.into();
    }
    0
}

/// ITimerVal struct
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ITimerVal {
    pub it_interval: TimeVal,
    pub it_value: TimeVal,
}

/// `getitimer(which, curr_value)` — Get value of an interval timer.
pub fn sys_getitimer(which: i32, curr_value: *mut u8) -> SyscallResult {
    if which < 0 || which > 2 {
        return Errno::EINVAL.into();
    }
    if curr_value.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(curr_value, core::mem::size_of::<ITimerVal>()).is_err() {
        return Errno::EFAULT.into();
    }
    // Return disarmed timer
    let it = ITimerVal::default();
    // SAFETY: Pointer validated with validate_user_ptr_write.
    unsafe { core::ptr::write(curr_value as *mut ITimerVal, it) };
    0
}

/// `setitimer(which, new_value, old_value)` — Set value of an interval timer.
pub fn sys_setitimer(which: i32, new_value: *const u8, old_value: *mut u8) -> SyscallResult {
    if which < 0 || which > 2 {
        return Errno::EINVAL.into();
    }
    if new_value.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(new_value, core::mem::size_of::<ITimerVal>()) {
        return Errno::EFAULT.into();
    }
    if !old_value.is_null() {
        let _ = sys_getitimer(which, old_value);
    }
    0
}

/// `alarm(seconds)` — Set an alarm clock for delivery of a signal.
pub fn sys_alarm(_seconds: u32) -> SyscallResult {
    0 // Return 0 (no previous alarm was scheduled)
}

/// `sethostname(name, len)` — set system host name.
///
/// Writes to the calling task's per-task UTS namespace so that containers
/// which have called `unshare(CLONE_NEWUTS)` get their own hostname without
/// affecting the host or sibling processes.
pub fn sys_sethostname(name: *const u8, len: usize) -> SyscallResult {
    if len > 64 {
        return Errno::EINVAL.into();
    }
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        if task_arc.lock().euid != 0 {
            return Errno::EPERM.into();
        }
    }
    if !validate_user_ptr(name, len) {
        return Errno::EFAULT.into();
    }
    // SAFETY: Pointer and length validated above
    let bytes = unsafe { core::slice::from_raw_parts(name, len) };
    let s = alloc::string::String::from_utf8_lossy(bytes).into_owned();
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        task_arc.lock().uts_ns.hostname = s;
    }
    0
}

/// `setdomainname(name, len)` — set system NIS domain name.
///
/// Writes to the calling task's per-task UTS namespace.
pub fn sys_setdomainname(name: *const u8, len: usize) -> SyscallResult {
    if len > 64 {
        return Errno::EINVAL.into();
    }
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        if task_arc.lock().euid != 0 {
            return Errno::EPERM.into();
        }
    }
    if !validate_user_ptr(name, len) {
        return Errno::EFAULT.into();
    }
    // SAFETY: Pointer and length validated above
    let bytes = unsafe { core::slice::from_raw_parts(name, len) };
    let s = alloc::string::String::from_utf8_lossy(bytes).into_owned();
    if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        task_arc.lock().uts_ns.domainname = s;
    }
    0
}

/// Linux `rseq` structure.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Rseq {
    pub cpu_id_start: u32,
    pub cpu_id: u32,
    pub rseq_cs: u64,
    pub flags: u32,
    pub node_id: u32,
    pub mm_cid: u32,
}

pub const RSEQ_FLAG_UNREGISTER: i32 = 1;

/// `rseq(rseq, rseq_len, flags, sig)` — register/unregister restartable sequence.
///
/// NOTE: Full rseq requires compiler-level restartable sequence support and
/// kernel scheduler preemption abort handlers. Returning ENOSYS instructs the
/// userspace C runtime (musl/glibc) to safely fall back to standard mutexes/atomics,
/// preventing multi-core data races on CPU 0 arena allocations.
pub fn sys_rseq(_rseq_ptr: *mut Rseq, _rseq_len: u32, _flags: i32, _sig: u32) -> SyscallResult {
    Errno::ENOSYS.into()
}
