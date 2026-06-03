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

        // Add the current task to the wait queue
        self.pids.lock().push_back(current_pid);

        // Mark the task as Blocked under the scheduler lock
        {
            let mut sched_lock = scheduler::SCHEDULER.lock();
            if let Some(ref mut sched) = *sched_lock {
                if let Some(task) = sched.get_task_mut(current_pid) {
                    task.state = TaskState::Blocked;
                }
            }
        }

        // Yield CPU control to execute other tasks
        scheduler::schedule();

        // When we wake up, ensure we are no longer in the queue (e.g. if woken up by a signal)
        let mut pids = self.pids.lock();
        pids.retain(|&x| x != current_pid);
    }

    /// Wake up all tasks currently sleeping on this wait queue.
    pub fn wake_all(&self) {
        let mut pids = self.pids.lock();
        let mut sched_lock = scheduler::SCHEDULER.lock();
        if let Some(ref mut sched) = *sched_lock {
            while let Some(pid) = pids.pop_front() {
                sched.wake_task(pid);
            }
        }
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
