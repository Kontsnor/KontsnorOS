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

/// A queue of task PIDs waiting for an event or resource.
pub struct WaitQueue {
    pids: TicketLock<VecDeque<Pid>>,
}

impl WaitQueue {
    /// Create a new, empty wait queue.
    pub const fn new() -> Self {
        Self {
            pids: TicketLock::new(VecDeque::new()),
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

            // Mark the task as Blocked
            if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
                task_arc.lock().state = TaskState::Blocked;
            }

            // Release the scheduler lock before rescheduling
            drop(sched_lock);
        });

        // Yield CPU control to execute other tasks
        scheduler::schedule();

        // When we wake up, ensure we are no longer in the queue (e.g. if woken up by a signal)
        let mut pids = self.pids.lock();
        pids.retain(|&x| x != current_pid);
    }

    /// Wake up all tasks currently sleeping on this wait queue.
    pub fn wake_all(&self) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut sched_lock = scheduler::SCHEDULER.lock();
            if let Some(ref mut sched) = *sched_lock {
                let mut pids = self.pids.lock();
                while let Some(pid) = pids.pop_front() {
                    sched.wake_task(pid);
                }
            }
        });
        crate::fs::epoll::wake_all_epolls();
    }

    /// Wake up all tasks currently sleeping on this wait queue.
    /// The caller must already hold the scheduler lock.
    pub fn wake_all_locked(&self, sched: &mut scheduler::Scheduler) {
        let mut pids = self.pids.lock();
        while let Some(pid) = pids.pop_front() {
            sched.wake_task(pid);
        }
    }
}
