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

//! Fast Userspace Mutexes (Futex) system call.

use crate::process::pid::Pid;
use crate::process::scheduler;
use crate::process::task::TaskState;
use crate::sync::spinlock::TicketLock;
use crate::syscall::{Errno, SyscallResult};
use alloc::collections::{BTreeMap, VecDeque};

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[derive(Debug, Clone, Copy)]
struct FutexWaiter {
    pid: Pid,
    bitset: u32,
    deadline: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FutexKey {
    /// 0 for process-shared mappings, or `tgid` for private process mappings
    pub space_id: u64,
    /// User virtual address
    pub uaddr: u64,
}

impl FutexKey {
    pub const fn new(space_id: u64, uaddr: u64) -> Self {
        Self { space_id, uaddr }
    }
}

pub fn get_futex_key(current_pid: Pid, uaddr: u64, op: i32) -> FutexKey {
    let is_private = (op & FUTEX_PRIVATE_FLAG) != 0;
    if is_private {
        let tgid = if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
            task_arc.lock().tgid.as_u64()
        } else {
            0
        };
        FutexKey::new(tgid, uaddr)
    } else {
        // Non-private futex shared across tasks or processes:
        // Key by the physical memory address, matching Linux semantics!
        let phys = crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(uaddr));
        if let Some(phys_addr) = phys {
            FutexKey::new(0, phys_addr.as_u64())
        } else {
            FutexKey::new(0, uaddr)
        }
    }
}

static FUTEX_QUEUES: TicketLock<BTreeMap<FutexKey, VecDeque<FutexWaiter>>> =
    TicketLock::new(BTreeMap::new());

/// Wake futex waiters from the scheduler/kernel without re-locking SCHEDULER.
pub fn futex_wake_locked(
    space_id: u64,
    uaddr: u64,
    val: i32,
    bitset: u32,
    sched: &mut crate::process::scheduler::Scheduler,
) -> i32 {
    let val_to_wake = if val < 0 { i32::MAX } else { val };
    let mut keys_to_wake = alloc::vec![FutexKey::new(space_id, uaddr)];
    if let Some(phys) = crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(uaddr)) {
        let phys_key = FutexKey::new(0, phys.as_u64());
        if !keys_to_wake.contains(&phys_key) {
            keys_to_wake.push(phys_key);
        }
    }
    let virt_shared_key = FutexKey::new(0, uaddr);
    if !keys_to_wake.contains(&virt_shared_key) {
        keys_to_wake.push(virt_shared_key);
    }

    let mut queues = FUTEX_QUEUES.lock();
    let mut woken = 0;

    for key in keys_to_wake {
        if woken >= val_to_wake {
            break;
        }
        if let Some(queue) = queues.get_mut(&key) {
            let mut i = 0;
            while woken < val_to_wake && i < queue.len() {
                if (queue[i].bitset & bitset) != 0 {
                    let waiter = queue.remove(i).unwrap();
                    if sched.wake_task(waiter.pid) {
                        woken += 1;
                    }
                } else {
                    i += 1;
                }
            }
            if queue.is_empty() {
                queues.remove(&key);
            }
        }
    }
    woken
}

/// Remove all futex-queue entries for `pid`. Must be called while the SCHEDULER
/// lock is already held (caller passes `sched` to enforce this contract).
pub(crate) fn futex_drain_pid_locked(
    pid: crate::process::pid::Pid,
    _sched: &mut crate::process::scheduler::Scheduler,
) {
    let mut queues = FUTEX_QUEUES.lock();
    for queue in queues.values_mut() {
        queue.retain(|w| w.pid != pid);
    }
    queues.retain(|_, q| !q.is_empty());
}

/// Emit the POSIX `clear_child_tid` futex wake for a CLONE_THREAD thread that
/// is exiting.  Must be called while holding the SCHEDULER lock (i.e., from
/// inside `Scheduler::exit_task`).  The caller is responsible for validating
/// that `uaddr` is a legitimate user-space address before calling this.
///
/// This mirrors Linux's `mm_release()` behaviour: write 0 to `*uaddr`, then
/// `FUTEX_WAKE(uaddr, 1)` so that any `pthread_join` waiter is unblocked.
///
/// # Safety of the write
///
/// We do **not** call `validate_user_ptr_write` here because this function is
/// always invoked while the SCHEDULER lock is held.  `validate_user_ptr_write`
/// internally calls `scheduler::current_pid()`, which itself tries to read
/// scheduler state and would deadlock or silently fail under the lock.
///
/// The `uaddr` is trusted: it was stored in `task.clear_child_tid` by
/// `sys_set_tid_address`, which already validated the pointer at registration
/// time.  We still perform a basic canonical-address range check.
pub(crate) fn clear_child_tid_wake_locked(
    space_id: u64,
    uaddr: u64,
    sched: &mut crate::process::scheduler::Scheduler,
) {
    // Basic sanity: must be non-null and within the user-space canonical range.
    if uaddr == 0 || uaddr > 0x0000_7FFF_FFFF_FFFF {
        return;
    }
    // SAFETY: uaddr was validated by sys_set_tid_address at registration time.
    // We write 0 to notify pthread_join that the thread has exited, matching
    // the Linux kernel's mm_release() behaviour.
    unsafe {
        (uaddr as *mut i32).write_volatile(0);
    }
    // Wake all waiters on this futex word.
    futex_wake_locked(space_id, uaddr, i32::MAX, 0xffff_ffff, sched);
}

/// Scan futex queues and wake any waiters whose deadlines have expired.
pub fn check_futex_timeouts_locked(sched: &mut crate::process::scheduler::Scheduler) {
    let current_ticks = crate::arch::x86_64::interrupts::timer_ticks();
    let mut queues = FUTEX_QUEUES.lock();
    let mut empty_keys = alloc::vec::Vec::new();

    for (&key, queue) in queues.iter_mut() {
        let mut i = 0;
        while i < queue.len() {
            if let Some(dl) = queue[i].deadline {
                if current_ticks >= dl {
                    let waiter = queue.remove(i).unwrap();
                    sched.wake_task(waiter.pid);
                    continue;
                }
            }
            i += 1;
        }
        if queue.is_empty() {
            empty_keys.push(key);
        }
    }

    for key in empty_keys {
        queues.remove(&key);
    }
}

const FUTEX_PRIVATE_FLAG: i32 = 128;
const FUTEX_CLOCK_REALTIME: i32 = 256;

/// `futex(uaddr, op, val, timeout, uaddr2, val3)`
pub fn sys_futex(
    uaddr: *mut i32,
    op: i32,
    val: i32,
    timeout: u64,
    uaddr2: *mut i32,
    val3: i32,
) -> SyscallResult {
    if uaddr.is_null() {
        return Errno::EINVAL.into();
    }

    if !crate::syscall::validation::validate_user_ptr(
        uaddr as *const u8,
        core::mem::size_of::<i32>(),
    ) {
        return Errno::EFAULT.into();
    }

    let cmd = op & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    match cmd {
        0 | 9 => {
            // FUTEX_WAIT (0) or FUTEX_WAIT_BITSET (9)
            let bitset = if cmd == 9 {
                let b = val3 as u32;
                if b == 0 {
                    return Errno::EINVAL.into();
                }
                b
            } else {
                0xffff_ffff
            };

            let current_ticks = crate::arch::x86_64::interrupts::timer_ticks();
            let deadline = if timeout != 0 {
                if !crate::syscall::validation::validate_user_ptr(
                    timeout as *const u8,
                    core::mem::size_of::<Timespec>(),
                ) {
                    return Errno::EFAULT.into();
                }
                // SAFETY: validate_user_ptr verified memory location bounds
                let ts = unsafe { core::ptr::read_volatile(timeout as *const Timespec) };
                if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
                    return Errno::EINVAL.into();
                }

                if cmd == 0 {
                    // FUTEX_WAIT: relative timeout
                    if ts.tv_sec == 0 && ts.tv_nsec == 0 {
                        return Errno::ETIMEDOUT.into();
                    }
                    let rel_ns = (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64);
                    let rel_ticks = (rel_ns + 9_999_999) / 10_000_000;
                    Some(current_ticks + core::cmp::max(1, rel_ticks))
                } else {
                    // FUTEX_WAIT_BITSET: absolute timeout
                    let target_ns = (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64);
                    let is_realtime =
                        (op & FUTEX_CLOCK_REALTIME) != 0 || target_ns >= 1_000_000_000_000_000_000;
                    let current_now_ns = if is_realtime {
                        1782158506u64 * 1_000_000_000 + (current_ticks * 10_000_000)
                    } else {
                        current_ticks * 10_000_000
                    };

                    if target_ns <= current_now_ns {
                        return Errno::ETIMEDOUT.into();
                    }
                    let diff_ns = target_ns - current_now_ns;
                    let ticks = (diff_ns + 9_999_999) / 10_000_000;
                    Some(current_ticks + core::cmp::max(1, ticks))
                }
            } else {
                None
            };

            // SAFETY: We validated that uaddr points to a valid user memory location.
            // Read uaddr before taking locks to trigger demand paging safely with interrupts enabled.
            let pre_read_val = unsafe { core::ptr::read_volatile(uaddr) };
            if pre_read_val != val {
                return Errno::EAGAIN.into();
            }

            let key = get_futex_key(current_pid, uaddr as u64, op);

            {
                let mut queues = FUTEX_QUEUES.lock();

                let current_val = unsafe { core::ptr::read_volatile(uaddr) };
                if current_val != val {
                    return Errno::EAGAIN.into();
                }

                queues
                    .entry(key)
                    .or_insert_with(VecDeque::new)
                    .push_back(FutexWaiter {
                        pid: current_pid,
                        bitset,
                        deadline,
                    });

                if let Some(dl) = deadline {
                    crate::fs::epoll::add_sleep_timeout(current_pid, dl);
                }

                if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
                    task_arc.lock().state = TaskState::Blocked;
                }
            }

            scheduler::schedule();

            if deadline.is_some() {
                crate::fs::epoll::remove_sleep_timeout(current_pid);
            }

            // When unblocked, ensure this task is removed from FUTEX_QUEUES under all conditions
            {
                let mut queues = FUTEX_QUEUES.lock();
                if let Some(queue) = queues.get_mut(&key) {
                    queue.retain(|w| w.pid != current_pid);
                    if queue.is_empty() {
                        queues.remove(&key);
                    }
                }
            }

            // Check if deadline expired
            if let Some(dl) = deadline {
                if crate::arch::x86_64::interrupts::timer_ticks() >= dl {
                    return Errno::ETIMEDOUT.into();
                }
            }

            // Check for unblocked pending signals
            if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
                let task = task_arc.lock();
                let pending_unblocked = task.pending_signals & !task.blocked_signals;
                if pending_unblocked != 0 {
                    return Errno::EINTR.into();
                }
            }

            0
        }
        1 | 10 => {
            // FUTEX_WAKE (1) or FUTEX_WAKE_BITSET (10)
            let bitset = if cmd == 10 {
                let b = val3 as u32;
                if b == 0 {
                    return Errno::EINVAL.into();
                }
                b
            } else {
                0xffff_ffff
            };
            let val_to_wake = if val < 0 { i32::MAX } else { val };

            let key = get_futex_key(current_pid, uaddr as u64, op);
            let mut keys_to_wake = alloc::vec![key];
            if key.space_id != 0 {
                if let Some(phys) =
                    crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(uaddr as u64))
                {
                    let phys_key = FutexKey::new(0, phys.as_u64());
                    if !keys_to_wake.contains(&phys_key) {
                        keys_to_wake.push(phys_key);
                    }
                }
                let virt_shared_key = FutexKey::new(0, uaddr as u64);
                if !keys_to_wake.contains(&virt_shared_key) {
                    keys_to_wake.push(virt_shared_key);
                }
            } else {
                if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
                    let tgid = task_arc.lock().tgid.as_u64();
                    let priv_key = FutexKey::new(tgid, uaddr as u64);
                    if !keys_to_wake.contains(&priv_key) {
                        keys_to_wake.push(priv_key);
                    }
                }
                if let Some(phys) =
                    crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(uaddr as u64))
                {
                    let phys_key = FutexKey::new(0, phys.as_u64());
                    if !keys_to_wake.contains(&phys_key) {
                        keys_to_wake.push(phys_key);
                    }
                }
                let virt_shared_key = FutexKey::new(0, uaddr as u64);
                if !keys_to_wake.contains(&virt_shared_key) {
                    keys_to_wake.push(virt_shared_key);
                }
            }

            let pids_to_wake = {
                let mut queues = FUTEX_QUEUES.lock();
                let mut pids = alloc::vec::Vec::new();
                for k in keys_to_wake {
                    if (pids.len() as i32) >= val_to_wake {
                        break;
                    }
                    if let Some(queue) = queues.get_mut(&k) {
                        let mut i = 0;
                        while i < queue.len() && (pids.len() as i32) < val_to_wake {
                            if (queue[i].bitset & bitset) != 0 {
                                let waiter = queue.remove(i).unwrap();
                                pids.push(waiter.pid);
                            } else {
                                i += 1;
                            }
                        }
                        if queue.is_empty() {
                            queues.remove(&k);
                        }
                    }
                }
                pids
            };

            let mut woken = 0;
            if !pids_to_wake.is_empty() {
                x86_64::instructions::interrupts::without_interrupts(|| {
                    if let Some(ref mut sched) = *scheduler::SCHEDULER.lock() {
                        for pid in pids_to_wake {
                            if sched.wake_task(pid) {
                                woken += 1;
                            }
                        }
                    }
                });
            }

            woken as SyscallResult
        }
        3 | 4 => {
            // FUTEX_REQUEUE (3) or FUTEX_CMP_REQUEUE (4)
            if uaddr2.is_null() {
                return Errno::EINVAL.into();
            }
            if !crate::syscall::validation::validate_user_ptr(
                uaddr2 as *const u8,
                core::mem::size_of::<i32>(),
            ) {
                return Errno::EFAULT.into();
            }

            let num_to_wake = if val < 0 { i32::MAX } else { val };
            let num_to_requeue = if (timeout as i32) < 0 {
                i32::MAX
            } else {
                timeout as i32
            };

            if cmd == 4 {
                // SAFETY: uaddr is validated before match
                let pre_read_val = unsafe { core::ptr::read_volatile(uaddr) };
                if pre_read_val != val3 {
                    return Errno::EAGAIN.into();
                }
            }

            let key1 = get_futex_key(current_pid, uaddr as u64, op);
            let key2 = get_futex_key(current_pid, uaddr2 as u64, op);

            let pids_to_wake = {
                let mut queues = FUTEX_QUEUES.lock();

                if cmd == 4 {
                    let current_val = unsafe { core::ptr::read_volatile(uaddr) };
                    if current_val != val3 {
                        return Errno::EAGAIN.into();
                    }
                }

                let mut pids = alloc::vec::Vec::new();
                if let Some(mut queue) = queues.remove(&key1) {
                    // 1. Wake up to num_to_wake waiters
                    while (pids.len() as i32) < num_to_wake && !queue.is_empty() {
                        let waiter = queue.pop_front().unwrap();
                        pids.push(waiter.pid);
                    }

                    // 2. Requeue up to num_to_requeue remaining waiters to key2
                    if !queue.is_empty() {
                        let target_queue = queues.entry(key2).or_insert_with(VecDeque::new);

                        let mut requeued = 0;
                        while requeued < num_to_requeue && !queue.is_empty() {
                            let waiter = queue.pop_front().unwrap();
                            target_queue.push_back(waiter);
                            requeued += 1;
                        }

                        // If any waiters still remain, put them back into key1 queue
                        if !queue.is_empty() {
                            queues.insert(key1, queue);
                        }
                    }
                }
                pids
            };

            let mut woken = 0;
            if !pids_to_wake.is_empty() {
                x86_64::instructions::interrupts::without_interrupts(|| {
                    if let Some(ref mut sched) = *scheduler::SCHEDULER.lock() {
                        for pid in pids_to_wake {
                            if sched.wake_task(pid) {
                                woken += 1;
                            }
                        }
                    }
                });
            }
            woken as SyscallResult
        }
        _ => {
            crate::kprintln!("[syscall pid={}] sys_futex unknown op={}", current_pid, op);
            Errno::ENOSYS.into()
        }
    }
}

/// Diagnostic dump of all active futex queues and the current values at their user addresses.
pub fn dump_futex_waiters() {
    let queues = FUTEX_QUEUES.lock();
    if queues.is_empty() {
        return;
    }
    crate::kprint!("[monitor] futex: ");
    let mut first_queue = true;
    for (&key, queue) in queues.iter() {
        if !first_queue {
            crate::kprint!(" | ");
        }
        first_queue = false;
        crate::kprint!("{}:{:#x}: [", key.space_id, key.uaddr);
        let mut first = true;
        for waiter in queue.iter() {
            if !first {
                crate::kprint!(", ");
            }
            first = false;
            let val = if crate::syscall::validation::validate_user_ptr(key.uaddr as *const u8, 4) {
                // SAFETY: validate_user_ptr verified memory location bounds
                unsafe { core::ptr::read_volatile(key.uaddr as *const i32) }
            } else {
                -999
            };
            crate::kprint!("pid={}(val={})", waiter.pid, val);
        }
        crate::kprint!("]");
    }
    crate::kprintln!();
}
