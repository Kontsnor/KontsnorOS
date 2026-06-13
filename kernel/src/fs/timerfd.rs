//! timerfd — high-precision timers as file descriptors.

use crate::fs::inode::{is_inode_nonblocking, DirEntry, FileType, Inode, InodeOps, POLLIN};
use crate::sync::wait_queue::WaitQueue;
use crate::syscall::{Errno, SyscallResult};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Itimerspec {
    pub it_interval: Timespec,
    pub it_value: Timespec,
}

pub struct TimerFd {
    inode: Inode,
    pub expiration_ticks: Mutex<Option<u64>>,
    pub interval_ticks: Mutex<u64>,
    pub num_expirations: Mutex<u64>,
    pub wait_queue: Arc<WaitQueue>,
}

impl TimerFd {
    pub fn new() -> Self {
        Self {
            inode: Inode::new(0, FileType::Regular),
            expiration_ticks: Mutex::new(None),
            interval_ticks: Mutex::new(0),
            num_expirations: Mutex::new(0),
            wait_queue: Arc::new(WaitQueue::new()),
        }
    }
}

impl InodeOps for TimerFd {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn as_timerfd(&self) -> Option<&TimerFd> {
        Some(self)
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.len() < 8 {
            return Err(-22); // EINVAL
        }

        loop {
            let mut num_exp = self.num_expirations.lock();
            if *num_exp > 0 {
                let val = *num_exp;
                *num_exp = 0;
                buf[..8].copy_from_slice(&val.to_ne_bytes());
                return Ok(8);
            }

            if is_inode_nonblocking(self) {
                return Err(-11); // EAGAIN
            }

            drop(num_exp);
            self.wait_queue.wait();
        }
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        if (events & POLLIN) != 0 {
            if *self.num_expirations.lock() > 0 {
                revents |= POLLIN;
            }
        }
        revents
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }
}

pub static ACTIVE_TIMERFDS: Mutex<Vec<alloc::sync::Weak<TimerFd>>> = Mutex::new(Vec::new());

pub fn register_timerfd(weak: alloc::sync::Weak<TimerFd>) {
    ACTIVE_TIMERFDS.lock().push(weak);
}

pub fn check_timers() {
    let mut timers = ACTIVE_TIMERFDS.lock();
    let current_ticks = crate::arch::x86_64::interrupts::timer_ticks();

    timers.retain(|weak| {
        if let Some(timer) = weak.upgrade() {
            let mut exp = timer.expiration_ticks.lock();
            if let Some(ticks) = *exp {
                if current_ticks >= ticks {
                    let mut num_exp = timer.num_expirations.lock();
                    let interval = *timer.interval_ticks.lock();
                    if interval > 0 {
                        // Periodic timer
                        let elapsed_ticks = current_ticks - ticks;
                        let expirations = 1 + (elapsed_ticks / interval);
                        *num_exp += expirations;
                        *exp = Some(ticks + expirations * interval);
                    } else {
                        // One-shot timer
                        *num_exp += 1;
                        *exp = None;
                    }
                    drop(exp);
                    drop(num_exp);
                    timer.wait_queue.wake_all();
                }
            }
            true
        } else {
            false
        }
    });
}

fn timespec_to_ticks(ts: Timespec) -> u64 {
    let mut ticks = ts.tv_sec as u64 * 100;
    ticks += (ts.tv_nsec as u64 + 9_999_999) / 10_000_000;
    ticks
}

fn ticks_to_timespec(ticks: u64) -> Timespec {
    Timespec {
        tv_sec: (ticks / 100) as i64,
        tv_nsec: ((ticks % 100) * 10_000_000) as i64,
    }
}

/// `timerfd_create(clockid, flags)` — Create a timerfd.
pub fn sys_timerfd_create(_clockid: i32, flags: i32) -> SyscallResult {
    let nonblock = (flags & 0o4000) != 0; // TFD_NONBLOCK = O_NONBLOCK = 0o4000
    let cloexec = (flags & 0x80000) != 0; // TFD_CLOEXEC = O_CLOEXEC = 0x80000

    let mut open_flags = crate::fs::file::OpenFlags::O_RDWR;
    if nonblock {
        open_flags |= crate::fs::file::OpenFlags::O_NONBLOCK;
    }
    if cloexec {
        open_flags |= crate::fs::file::OpenFlags::O_CLOEXEC;
    }

    let timerfd = Arc::new(TimerFd::new());
    register_timerfd(Arc::downgrade(&timerfd));

    match crate::process::fd::current_task_alloc_fd_with_flags(
        timerfd,
        crate::fs::file::OpenFlags(open_flags),
    ) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}

/// `timerfd_settime(fd, flags, new_value, old_value)` — Arm/disarm a timerfd.
pub fn sys_timerfd_settime(
    fd: i32,
    flags: i32,
    new_value: *const Itimerspec,
    old_value: *mut Itimerspec,
) -> SyscallResult {
    if new_value.is_null() {
        return Errno::EINVAL.into();
    }
    if !crate::syscall::validation::validate_user_ptr(
        new_value as *const u8,
        core::mem::size_of::<Itimerspec>(),
    ) {
        return Errno::EFAULT.into();
    }
    if !old_value.is_null()
        && crate::syscall::validation::validate_user_ptr_write(
            old_value as *mut u8,
            core::mem::size_of::<Itimerspec>(),
        )
        .is_err()
    {
        return Errno::EFAULT.into();
    }

    let inode = match crate::process::fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };

    let timerfd = match inode.as_timerfd() {
        Some(t) => t,
        None => return Errno::EINVAL.into(),
    };

    let new_val = unsafe { *new_value };

    // Fill old_value if requested
    if !old_value.is_null() {
        let current_ticks = crate::arch::x86_64::interrupts::timer_ticks();
        let exp = *timerfd.expiration_ticks.lock();
        let it_value = if let Some(ticks) = exp {
            if ticks > current_ticks {
                ticks_to_timespec(ticks - current_ticks)
            } else {
                Timespec::default()
            }
        } else {
            Timespec::default()
        };
        let it_interval = ticks_to_timespec(*timerfd.interval_ticks.lock());
        let old_val = Itimerspec {
            it_interval,
            it_value,
        };
        unsafe {
            core::ptr::write(old_value, old_val);
        }
    }

    // Set new_value
    let is_absolute = (flags & 1) != 0; // TFD_TIMER_ABSTIME = 1
    let val_ticks = timespec_to_ticks(new_val.it_value);
    let interval_ticks = timespec_to_ticks(new_val.it_interval);

    let mut exp = timerfd.expiration_ticks.lock();
    let mut interval = timerfd.interval_ticks.lock();
    *timerfd.num_expirations.lock() = 0;

    if val_ticks == 0 {
        // Disarm timer
        *exp = None;
        *interval = 0;
    } else {
        // Arm timer
        if is_absolute {
            *exp = Some(val_ticks);
        } else {
            *exp = Some(crate::arch::x86_64::interrupts::timer_ticks() + val_ticks);
        }
        *interval = interval_ticks;
    }

    0
}
