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

//! Thread-safe WaitQueue for blocking and waking up tasks.

use crate::process::pid::Pid;
use crate::process::scheduler;
use crate::process::task::TaskState;
use crate::sync::spinlock::TicketLock;
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

/// A queue of task PIDs waiting for an event or resource.
pub struct WaitQueue {
    pids: TicketLock<VecDeque<Pid>>,
    listeners: TicketLock<Vec<Weak<WaitQueue>>>,
    /// Atomic counter of active waiting tasks and listeners.
    /// Used for fast-path check in `wake_all()` to avoid disabling interrupts
    /// and acquiring global scheduler locks when no waiters exist.
    waiter_count: AtomicUsize,
}

impl WaitQueue {
    /// Create a new, empty wait queue.
    pub const fn new() -> Self {
        Self {
            pids: TicketLock::new(VecDeque::new()),
            listeners: TicketLock::new(Vec::new()),
            waiter_count: AtomicUsize::new(0),
        }
    }

    /// Attach a listener wait queue (e.g. epoll or poll) to receive wakeups from this queue.
    pub fn add_listener(&self, listener: &Arc<WaitQueue>) {
        let mut list = self.listeners.lock();
        if !list.iter().any(|w| w.as_ptr() == Arc::as_ptr(listener)) {
            list.push(Arc::downgrade(listener));
            self.waiter_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Detach a listener wait queue.
    pub fn remove_listener(&self, listener: &Arc<WaitQueue>) {
        let mut list = self.listeners.lock();
        let old_len = list.len();
        list.retain(|w| {
            if let Some(upgraded) = w.upgrade() {
                !Arc::ptr_eq(&upgraded, listener)
            } else {
                false
            }
        });
        let removed = old_len - list.len();
        if removed > 0 {
            self.waiter_count.fetch_sub(removed, Ordering::Release);
        }
    }

    /// Sleep the current task on this wait queue.
    pub fn wait(&self) {
        let current_pid = match scheduler::current_pid() {
            Some(pid) => pid,
            None => return,
        };

        x86_64::instructions::interrupts::without_interrupts(|| {
            // F-09: Acquire SCHEDULER lock first to close the missed wakeup TOCTOU window
            let sched_lock = scheduler::SCHEDULER.lock();

            // Add the current task to the wait queue under both locks
            self.pids.lock().push_back(current_pid);
            self.waiter_count.fetch_add(1, Ordering::SeqCst);

            // Mark the task as Blocked
            if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
                task_arc.lock().state = TaskState::Blocked;
            }

            // Release the scheduler lock before rescheduling
            drop(sched_lock);
        });

        // Verify state validity before descheduling: if a concurrent wake_all() already
        // transitioned the task to Ready, clean up and return immediately to avoid a missed wakeup.
        if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
            if task_arc.lock().state != TaskState::Blocked {
                let mut pids = self.pids.lock();
                let old_len = pids.len();
                pids.retain(|&x| x != current_pid);
                if pids.len() < old_len {
                    self.waiter_count.fetch_sub(1, Ordering::Release);
                }
                return;
            }
        }

        // Yield CPU control to execute other tasks
        scheduler::schedule();

        // When we wake up, ensure we are no longer in the queue (e.g. if woken up by a signal)
        let mut pids = self.pids.lock();
        let old_len = pids.len();
        pids.retain(|&x| x != current_pid);
        if pids.len() < old_len {
            self.waiter_count.fetch_sub(1, Ordering::Release);
        }
    }

    /// Wake up all tasks currently sleeping on this wait queue.
    pub fn wake_all(&self) {
        // Fast path: if there are no waiting tasks or registered listeners, avoid disabling interrupts
        // and acquiring global SCHEDULER/pids/listeners locks.
        if self.waiter_count.load(Ordering::Acquire) == 0 {
            return;
        }

        x86_64::instructions::interrupts::without_interrupts(|| {
            if let Some(mut sched_lock) = scheduler::SCHEDULER.try_lock() {
                if let Some(ref mut sched) = *sched_lock {
                    self.wake_all_locked(sched);
                }
            } else {
                let apic_id = crate::arch::x86_64::smp::current_lapic_id() as u32;
                if scheduler::SCHEDULER.holding_cpu_id() == apic_id {
                    // SAFETY: The current CPU already holds SCHEDULER exclusively
                    unsafe {
                        if let Some(ref mut sched) = *scheduler::SCHEDULER.get_mut_unchecked() {
                            self.wake_all_locked(sched);
                        }
                    }
                } else {
                    let mut sched_lock = scheduler::SCHEDULER.lock();
                    if let Some(ref mut sched) = *sched_lock {
                        self.wake_all_locked(sched);
                    }
                }
            }
        });
    }

    /// Wake up all tasks currently sleeping on this wait queue and propagate to attached listeners.
    /// The caller must already hold the scheduler lock.
    pub fn wake_all_locked(&self, sched: &mut scheduler::Scheduler) {
        let mut pids = self.pids.lock();
        let woken_count = pids.len();
        while let Some(pid) = pids.pop_front() {
            sched.wake_task(pid);
        }
        drop(pids);
        if woken_count > 0 {
            self.waiter_count.fetch_sub(woken_count, Ordering::Release);
        }

        // Propagate wakeup to any attached listener queues (e.g. epoll, poll)
        let mut listeners = self.listeners.lock();
        let initial_listeners_count = listeners.len();
        listeners.retain(|weak_wq| {
            if let Some(child_wq) = weak_wq.upgrade() {
                child_wq.wake_all_locked(sched);
                true
            } else {
                false
            }
        });
        let pruned_listeners = initial_listeners_count - listeners.len();
        if pruned_listeners > 0 {
            self.waiter_count
                .fetch_sub(pruned_listeners, Ordering::Release);
        }
    }

    /// Register a task on this wait queue without locking the scheduler.
    pub fn register(&self, pid: Pid) {
        self.pids.lock().push_back(pid);
        self.waiter_count.fetch_add(1, Ordering::SeqCst);
    }

    /// Remove a task from this wait queue.
    pub fn remove(&self, pid: Pid) {
        let mut pids = self.pids.lock();
        let old_len = pids.len();
        pids.retain(|&x| x != pid);
        let removed = old_len - pids.len();
        if removed > 0 {
            self.waiter_count.fetch_sub(removed, Ordering::Release);
        }
    }
}
