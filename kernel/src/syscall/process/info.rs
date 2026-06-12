//! Process system information, resource limits, and time syscalls.

use super::super::{Errno, SyscallResult};
use crate::syscall::validation::{validate_user_ptr, validate_user_ptr_write};

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

/// `uname(buf)` — Write kernel identity information into a `utsname` struct.
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
    fill(&mut u.nodename, b"kontsnoros");
    fill(&mut u.release, b"6.1.0-KontsnorOS");
    fill(&mut u.version, b"#1 SMP");
    fill(&mut u.machine, b"x86_64");
    fill(&mut u.domainname, b"(none)");

    unsafe {
        core::ptr::write(buf as *mut UtsName, u);
    }
    0
}

/// `timeval` struct used by `gettimeofday`.
#[repr(C)]
struct TimeVal {
    tv_sec: i64,
    tv_usec: i64,
}

/// `timezone` struct used by `gettimeofday`.
#[repr(C)]
struct TimeZone {
    tz_minuteswest: i32,
    tz_dsttime: i32,
}

/// `gettimeofday(tv, tz)` — Return current time-of-day.
pub fn sys_gettimeofday(tv: *mut u8, tz: *mut u8) -> SyscallResult {
    if !tv.is_null() {
        if validate_user_ptr_write(tv, core::mem::size_of::<TimeVal>()).is_err() {
            return Errno::EFAULT.into();
        }
        let t = TimeVal {
            tv_sec: 0,
            tv_usec: 0,
        };
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
        unsafe {
            core::ptr::write(tz as *mut TimeZone, z);
        }
    }
    0
}

/// `timespec` struct used by `clock_gettime` and `nanosleep`.
#[repr(C)]
struct TimeSpec {
    tv_sec: i64,
    tv_nsec: i64,
}

/// `clock_gettime(clockid, tp)` — Return current clock value.
pub fn sys_clock_gettime(_clockid: i32, tp: *mut u8) -> SyscallResult {
    if tp.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(tp, core::mem::size_of::<TimeSpec>()).is_err() {
        return Errno::EFAULT.into();
    }
    let ts = TimeSpec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        core::ptr::write(tp as *mut TimeSpec, ts);
    }
    0
}

/// `nanosleep(req, rem)` — High-resolution sleep.
pub fn sys_nanosleep(req: *const u8, rem: *mut u8) -> SyscallResult {
    if !req.is_null() {
        if !validate_user_ptr(req, core::mem::size_of::<TimeSpec>()) {
            return Errno::EFAULT.into();
        }
    }
    // Yield to the scheduler.
    crate::process::scheduler::yield_now();
    if !rem.is_null() {
        if validate_user_ptr_write(rem, core::mem::size_of::<TimeSpec>()).is_err() {
            return Errno::EFAULT.into();
        }
        let ts = TimeSpec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            core::ptr::write(rem as *mut TimeSpec, ts);
        }
    }
    0
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
        7 => RLimit {
            rlim_cur: 1024,
            rlim_max: 4096,
        }, // RLIMIT_NOFILE
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

/// `setrlimit(resource, rlim)` — Set resource limits (stub).
pub fn sys_setrlimit(_resource: i32, _rlim: *const u8) -> SyscallResult {
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
    pad: [u8; 22],
    totalhigh: u64,
    freehigh: u64,
    mem_unit: u32,
    _pad2: [u8; 8],
}

/// `sysinfo(info)` — Return overall system information.
pub fn sys_sysinfo(info: *mut u8) -> SyscallResult {
    if info.is_null() {
        return Errno::EFAULT.into();
    }
    if validate_user_ptr_write(info, core::mem::size_of::<SysInfo>()).is_err() {
        return Errno::EFAULT.into();
    }
    let si = SysInfo {
        uptime: 0,
        loads: [0, 0, 0],
        totalram: 128 * 1024 * 1024,
        freeram: 64 * 1024 * 1024,
        sharedram: 0,
        bufferram: 0,
        totalswap: 0,
        freeswap: 0,
        procs: 1,
        pad: [0u8; 22],
        totalhigh: 0,
        freehigh: 0,
        mem_unit: 1,
        _pad2: [0u8; 8],
    };
    unsafe {
        core::ptr::write(info as *mut SysInfo, si);
    }
    0
}

/// `sigaltstack(ss, old_ss)` — Set/get alternate signal stack (stub).
pub fn sys_sigaltstack(_ss: *const u8, _old_ss: *mut u8) -> SyscallResult {
    0
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
    _new_limit: *const u8,
    old_limit: *mut u8,
) -> SyscallResult {
    if !old_limit.is_null() {
        let ret = sys_getrlimit(resource, old_limit);
        if ret < 0 {
            return ret;
        }
    }
    0
}

/// `tgkill(tgid, tid, sig)` — Send signal to thread.
pub fn sys_tgkill(_tgid: i32, tid: i32, sig: i32) -> SyscallResult {
    crate::syscall::signal::sys_kill(tid, sig)
}
