//! Fast Userspace Mutexes (Futex) system call.

use crate::process::pid::Pid;
use crate::process::scheduler;
use crate::sync::spinlock::TicketLock;
use crate::syscall::{Errno, SyscallResult};
use alloc::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy)]
struct FutexWaiter {
    pid: Pid,
    bitset: u32,
}

static FUTEX_QUEUES: TicketLock<BTreeMap<u64, VecDeque<FutexWaiter>>> =
    TicketLock::new(BTreeMap::new());

/// Wake futex waiters from the scheduler/kernel without re-locking SCHEDULER.
pub fn futex_wake_locked(
    uaddr: u64,
    val: i32,
    bitset: u32,
    sched: &mut crate::process::scheduler::Scheduler,
) -> i32 {
    let mut queues = FUTEX_QUEUES.lock();
    let mut woken = 0;

    if let Some(queue) = queues.get_mut(&uaddr) {
        let mut i = 0;
        while woken < val && i < queue.len() {
            if (queue[i].bitset & bitset) != 0 {
                let waiter = queue.remove(i).unwrap();
                sched.wake_task(waiter.pid);
                woken += 1;
            } else {
                i += 1;
            }
        }
        if queue.is_empty() {
            queues.remove(&uaddr);
        }
    }
    woken
}

/// `futex(uaddr, op, val, timeout, uaddr2, val3)`
pub fn sys_futex(
    uaddr: *mut i32,
    op: i32,
    val: i32,
    _timeout: u64,
    _uaddr2: *mut i32,
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

    let cmd = op & 127;
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    match cmd {
        0 | 9 => {
            // FUTEX_WAIT or FUTEX_WAIT_BITSET
            let bitset = if cmd == 9 {
                let b = val3 as u32;
                if b == 0 {
                    return Errno::EINVAL.into();
                }
                b
            } else {
                0xffffffff
            };

            crate::kprintln!(
                "[syscall pid={}] sys_futex WAIT: uaddr={:#x}, val={}, op={}",
                current_pid,
                uaddr as u64,
                val,
                op
            );

            let mut queues = FUTEX_QUEUES.lock();

            // SAFETY: We validated that uaddr points to a valid user memory location
            // and contains a valid 32-bit integer.
            let current_val = unsafe { core::ptr::read_volatile(uaddr) };
            if current_val != val {
                crate::kprintln!(
                    "[syscall pid={}] sys_futex WAIT: val mismatch (current={}, expected={}) -> EAGAIN",
                    current_pid,
                    current_val,
                    val
                );
                return Errno::EAGAIN.into();
            }

            queues
                .entry(uaddr as u64)
                .or_insert_with(VecDeque::new)
                .push_back(FutexWaiter {
                    pid: current_pid,
                    bitset,
                });

            crate::process::lifecycle::block_task(current_pid);
            drop(queues);

            scheduler::yield_now();
            0
        }
        1 | 10 => {
            // FUTEX_WAKE or FUTEX_WAKE_BITSET
            let bitset = if cmd == 10 {
                let b = val3 as u32;
                if b == 0 {
                    return Errno::EINVAL.into();
                }
                b
            } else {
                0xffffffff
            };

            crate::kprintln!(
                "[syscall pid={}] sys_futex WAKE: uaddr={:#x}, val={}, op={}",
                current_pid,
                uaddr as u64,
                val,
                op
            );

            let mut queues = FUTEX_QUEUES.lock();
            let mut woken = 0;

            if let Some(queue) = queues.get_mut(&(uaddr as u64)) {
                let mut i = 0;
                while woken < val && i < queue.len() {
                    if (queue[i].bitset & bitset) != 0 {
                        let waiter = queue.remove(i).unwrap();
                        crate::process::lifecycle::wake_task(waiter.pid);
                        woken += 1;
                    } else {
                        i += 1;
                    }
                }
                if queue.is_empty() {
                    queues.remove(&(uaddr as u64));
                }
            }

            woken as SyscallResult
        }
        _ => {
            crate::kprintln!("[syscall pid={}] sys_futex unknown op={}", current_pid, op);
            Errno::ENOSYS.into()
        }
    }
}
