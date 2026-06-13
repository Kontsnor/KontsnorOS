//! Fast Userspace Mutexes (Futex) system call.

use crate::process::pid::Pid;
use crate::process::scheduler;
use crate::sync::spinlock::TicketLock;
use crate::syscall::{Errno, SyscallResult};
use alloc::collections::{BTreeMap, VecDeque};

static FUTEX_QUEUES: TicketLock<BTreeMap<u64, VecDeque<Pid>>> = TicketLock::new(BTreeMap::new());

/// `futex(uaddr, op, val, timeout, uaddr2, val3)`
pub fn sys_futex(
    uaddr: *mut i32,
    op: i32,
    val: i32,
    _timeout: u64,
    _uaddr2: *mut i32,
    _val3: i32,
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

    match cmd {
        0 => {
            // FUTEX_WAIT
            let current_pid = match scheduler::current_pid() {
                Some(p) => p,
                None => return Errno::ESRCH.into(),
            };

            let mut queues = FUTEX_QUEUES.lock();

            // SAFETY: We validated that uaddr points to a valid user memory location
            // and contains a valid 32-bit integer.
            let current_val = unsafe { core::ptr::read_volatile(uaddr) };
            if current_val != val {
                return Errno::EAGAIN.into();
            }

            queues
                .entry(uaddr as u64)
                .or_insert_with(VecDeque::new)
                .push_back(current_pid);

            crate::process::lifecycle::block_task(current_pid);
            drop(queues);

            scheduler::yield_now();
            0
        }
        1 => {
            // FUTEX_WAKE
            let mut queues = FUTEX_QUEUES.lock();
            let mut woken = 0;

            if let Some(queue) = queues.get_mut(&(uaddr as u64)) {
                while woken < val && !queue.is_empty() {
                    if let Some(pid) = queue.pop_front() {
                        crate::process::lifecycle::wake_task(pid);
                        woken += 1;
                    }
                }
                if queue.is_empty() {
                    queues.remove(&(uaddr as u64));
                }
            }

            woken as SyscallResult
        }
        _ => {
            crate::kprintln!("[syscall] sys_futex unknown op={}", op);
            Errno::ENOSYS.into()
        }
    }
}
