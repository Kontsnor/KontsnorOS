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

//! epoll — I/O event notification facility.

use crate::fs::inode::{DirEntry, FileType, Inode, InodeOps};
use crate::sync::wait_queue::WaitQueue;
use crate::syscall::{Errno, SyscallResult};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

/// EPOLLET flag.
pub const EPOLLET: u32 = 0x80000000;

#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default)]
pub struct EpollEvent {
    pub events: u32,
    pub data: u64,
}

pub struct EpollInstance {
    inode: Inode,
    pub monitored: Mutex<BTreeMap<i32, EpollEvent>>,
    pub last_ready: Mutex<BTreeMap<i32, u32>>,
    pub wait_queue: Arc<WaitQueue>,
}

impl EpollInstance {
    pub fn new() -> Self {
        let wq = Arc::new(WaitQueue::new());
        register_epoll_wait_queue(&wq);
        Self {
            inode: Inode::new(0, FileType::Regular),
            monitored: Mutex::new(BTreeMap::new()),
            last_ready: Mutex::new(BTreeMap::new()),
            wait_queue: wq,
        }
    }
}

impl InodeOps for EpollInstance {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn as_epoll(&self) -> Option<&EpollInstance> {
        Some(self)
    }

    fn wait_queue(&self) -> Option<Arc<WaitQueue>> {
        Some(self.wait_queue.clone())
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }
}

impl Drop for EpollInstance {
    fn drop(&mut self) {
        unregister_epoll_wait_queue(&self.wait_queue);
    }
}

pub static EPOLL_WAIT_QUEUES: Mutex<Vec<alloc::sync::Weak<WaitQueue>>> = Mutex::new(Vec::new());

/// Register a wait queue to receive global epoll/poll wakeups as a fallback.
pub fn register_epoll_wait_queue(wq: &Arc<WaitQueue>) {
    let mut list = EPOLL_WAIT_QUEUES.lock();
    if !list.iter().any(|w| w.as_ptr() == Arc::as_ptr(wq)) {
        list.push(Arc::downgrade(wq));
    }
}

/// Unregister a wait queue from global epoll/poll wakeups.
pub fn unregister_epoll_wait_queue(wq: &Arc<WaitQueue>) {
    let mut list = EPOLL_WAIT_QUEUES.lock();
    list.retain(|w| {
        if let Some(upgraded) = w.upgrade() {
            !Arc::ptr_eq(&upgraded, wq)
        } else {
            false
        }
    });
}

/// Wake all tasks waiting in poll/select or epoll wait queues.
pub fn wake_all_epolls() {
    let mut list = EPOLL_WAIT_QUEUES.lock();
    list.retain(|w| {
        if let Some(wq) = w.upgrade() {
            wq.wake_all();
            true
        } else {
            false
        }
    });
}

pub static SLEEP_TIMEOUTS: Mutex<Vec<(crate::process::pid::Pid, u64)>> = Mutex::new(Vec::new());

pub fn add_sleep_timeout(pid: crate::process::pid::Pid, expire_ticks: u64) {
    let current_ticks = crate::arch::x86_64::interrupts::timer_ticks();
    let now_ns = crate::syscall::process::get_monotonic_ns();
    let remaining_ticks = expire_ticks.saturating_sub(current_ticks);
    let deadline_ns = now_ns.saturating_add(remaining_ticks.saturating_mul(10_000_000));
    crate::process::scheduler::register_sleep_timer(pid, deadline_ns);

    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut timeouts = SLEEP_TIMEOUTS.lock();
        timeouts.retain(|&(p, _)| p != pid);
        timeouts.push((pid, expire_ticks));
    });
}

pub fn remove_sleep_timeout(pid: crate::process::pid::Pid) {
    crate::process::scheduler::remove_sleep_timer(pid);
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut timeouts = SLEEP_TIMEOUTS.lock();
        timeouts.retain(|&(p, _)| p != pid);
    });
}

pub fn check_sleep_timeouts() {
    let current_ticks = crate::arch::x86_64::interrupts::timer_ticks();
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(mut sched_lock) = crate::process::scheduler::SCHEDULER.try_lock() {
            if let Some(ref mut sched) = *sched_lock {
                let mut timeouts = SLEEP_TIMEOUTS.lock();
                timeouts.retain(|&(pid, expire_ticks)| {
                    if current_ticks >= expire_ticks {
                        sched.wake_task(pid);
                        false
                    } else {
                        true
                    }
                });
            }
        }
    });
}

/// `sys_epoll_create1(flags)` — Create an epoll instance.
pub fn sys_epoll_create1(flags: i32) -> SyscallResult {
    let cloexec = (flags & 0x80000) != 0; // EPOLL_CLOEXEC = O_CLOEXEC = 0x80000

    let mut open_flags = crate::fs::file::OpenFlags::O_RDWR;
    if cloexec {
        open_flags |= crate::fs::file::OpenFlags::O_CLOEXEC;
    }

    let epoll = Arc::new(EpollInstance::new());

    match crate::process::fd::current_task_alloc_fd_with_flags(
        epoll,
        crate::fs::file::OpenFlags(open_flags),
    ) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}

/// `sys_epoll_ctl(epfd, op, fd, event)` — Control interface for an epoll descriptor.
pub fn sys_epoll_ctl(epfd: i32, op: i32, fd: i32, event: *const EpollEvent) -> SyscallResult {
    if epfd < 0 || fd < 0 {
        return Errno::EBADF.into();
    }

    let epoll_inode = match crate::process::fd::current_task_read_fd(epfd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };

    let epoll = match epoll_inode.as_epoll() {
        Some(e) => e,
        None => return Errno::EINVAL.into(),
    };

    let target_inode = match crate::process::fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };

    match op {
        1 => {
            // EPOLL_CTL_ADD
            if event.is_null() {
                return Errno::EINVAL.into();
            }
            if !crate::syscall::validation::validate_user_ptr(
                event as *const u8,
                core::mem::size_of::<EpollEvent>(),
            ) {
                return Errno::EFAULT.into();
            }
            let ev = unsafe { *event };
            let mut monitored = epoll.monitored.lock();
            if monitored.contains_key(&fd) {
                return Errno::EEXIST.into();
            }
            monitored.insert(fd, ev);
            epoll.last_ready.lock().insert(fd, 0);
            if let Some(target_wq) = target_inode.wait_queue() {
                target_wq.add_listener(&epoll.wait_queue);
            }
            0
        }
        2 => {
            // EPOLL_CTL_DEL
            let mut monitored = epoll.monitored.lock();
            if !monitored.contains_key(&fd) {
                return Errno::ENOENT.into();
            }
            monitored.remove(&fd);
            epoll.last_ready.lock().remove(&fd);
            if let Some(target_wq) = target_inode.wait_queue() {
                target_wq.remove_listener(&epoll.wait_queue);
            }
            0
        }
        3 => {
            // EPOLL_CTL_MOD
            if event.is_null() {
                return Errno::EINVAL.into();
            }
            if !crate::syscall::validation::validate_user_ptr(
                event as *const u8,
                core::mem::size_of::<EpollEvent>(),
            ) {
                return Errno::EFAULT.into();
            }
            let ev = unsafe { *event };
            let mut monitored = epoll.monitored.lock();
            if !monitored.contains_key(&fd) {
                return Errno::ENOENT.into();
            }
            monitored.insert(fd, ev);
            epoll.last_ready.lock().insert(fd, 0);
            0
        }
        _ => Errno::EINVAL.into(),
    }
}

/// `sys_epoll_wait(epfd, events, maxevents, timeout)` — Wait for events on an epoll instance.
pub fn sys_epoll_wait(
    epfd: i32,
    events: *mut EpollEvent,
    maxevents: i32,
    timeout: i32,
) -> SyscallResult {
    if maxevents <= 0 || maxevents > 1024 || events.is_null() {
        return Errno::EINVAL.into();
    }

    if !crate::syscall::validation::validate_user_ptr(
        events as *const u8,
        maxevents as usize * core::mem::size_of::<EpollEvent>(),
    ) {
        return Errno::EFAULT.into();
    }

    let epoll_inode = match crate::process::fd::current_task_read_fd(epfd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };

    let epoll = match epoll_inode.as_epoll() {
        Some(e) => e,
        None => return Errno::EINVAL.into(),
    };

    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let start_ticks = crate::arch::x86_64::interrupts::timer_ticks();
    // Timeout of -1 blocks indefinitely. Otherwise timeout is in milliseconds.
    let expire_ticks = if timeout >= 0 {
        Some(start_ticks + (timeout as u64 + 9) / 10) // 10ms per tick, round up
    } else {
        None
    };

    let initial_cap = (maxevents as usize).min(1024);
    let mut ready_list = Vec::with_capacity(initial_cap);

    loop {
        ready_list.clear();
        {
            let monitored = epoll.monitored.lock();
            let mut last_ready = epoll.last_ready.lock();

            for (&fd, &ev) in monitored.iter() {
                if let Some(inode) = crate::process::fd::current_task_read_fd(fd) {
                    // Query readiness using the generalized poll method
                    let current_poll = inode.poll(ev.events);
                    let matched_ready = current_poll
                        & (ev.events | crate::fs::inode::POLLHUP | crate::fs::inode::POLLERR);

                    if matched_ready != 0 {
                        let is_et = (ev.events & EPOLLET) != 0;
                        let last_ready_mask = last_ready.get(&fd).cloned().unwrap_or(0);

                        if is_et {
                            // Edge-Triggered: Report only if it transitioned to ready or new events arose
                            let newly_ready = matched_ready & !last_ready_mask;
                            if newly_ready != 0 {
                                ready_list.push(EpollEvent {
                                    events: matched_ready,
                                    data: ev.data,
                                });
                            }
                        } else {
                            // Level-Triggered: Report as long as matching flags are active
                            ready_list.push(EpollEvent {
                                events: matched_ready,
                                data: ev.data,
                            });
                        }
                        last_ready.insert(fd, matched_ready);
                    } else {
                        // Reset last ready mask if it goes back to 0
                        last_ready.insert(fd, 0);
                    }
                }
                if ready_list.len() >= maxevents as usize {
                    break;
                }
            }
        }

        if !ready_list.is_empty() {
            // Write found events to user buffer
            unsafe {
                core::ptr::copy_nonoverlapping(ready_list.as_ptr(), events, ready_list.len());
            }
            return ready_list.len() as SyscallResult;
        }

        // Check if timeout has expired
        let current_ticks = crate::arch::x86_64::interrupts::timer_ticks();
        if timeout == 0 || expire_ticks.map_or(false, |exp| current_ticks >= exp) {
            return 0;
        }

        // Handle signals checking (should break with EINTR if a signal is pending)
        if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
            let task = task_arc.lock();
            let unblocked = task.pending_signals & !task.blocked_signals;
            if unblocked != 0 {
                return Errno::EINTR.into();
            }
        }

        // Add to sleep timeout if timeout is configured
        if let Some(exp) = expire_ticks {
            add_sleep_timeout(current_pid, exp);
        }

        // Block on wait queue
        epoll.wait_queue.wait();

        // Remove sleep timeout when woken up
        remove_sleep_timeout(current_pid);
    }
}
