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

//! Process scheduler — Multi-Level Feedback Queue (MLFQ).

use crate::kprintln;
use crate::sync::rwlock::KRwLock;
use crate::sync::spinlock::TicketLock;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::pid::Pid;
use super::task::{Priority, Task, TaskState};

/// Number of priority levels in the scheduler.
const NUM_PRIORITIES: usize = 5;

/// Time quantum (in timer ticks) before a task is preempted.
const TIME_QUANTUM: u64 = 10;

/// Number of ticks between priority boosts (prevents starvation).
const BOOST_INTERVAL: u64 = 1000;

/// The global scheduler instance.
pub(crate) static SCHEDULER: TicketLock<Option<Scheduler>> = TicketLock::new(None);

/// The global master task table.
pub(crate) static TASKS: KRwLock<Vec<Option<Arc<spin::Mutex<Task>>>>> = KRwLock::new(Vec::new());

/// Dummy CPU contexts to save old registers when the current task has already exited/been reaped.
static mut DUMMY_CONTEXTS: [super::context::CpuContext; 32] = [super::context::CpuContext {
    rbx: 0,
    rbp: 0,
    r12: 0,
    r13: 0,
    r14: 0,
    r15: 0,
    rsp: 0,
    rip: 0,
    rflags: 0x2,
    cr3: 0,
    fs_base: 0,
    gs_base: 0,
    kernel_gs_base: 0,
    _reserved: 0,
    fxsave: [0u8; 512],
}; 32];

/// The Multi-Level Feedback Queue scheduler.
pub struct Scheduler {
    /// Priority queues — one per priority level.
    pub(crate) queues: [VecDeque<Pid>; NUM_PRIORITIES],

    /// The currently running task's PID per CPU core.
    pub(crate) current_cpus: [Option<Pid>; 32],

    /// The core-specific idle task's PID per CPU core.
    pub(crate) idle_cpus: [Pid; 32],

    /// transitional field to hold the PID of the task currently being switched away from on each CPU core.
    pub(crate) suspending_tasks: [Option<Pid>; 32],

    /// Tick counter for priority boosting.
    ticks_since_boost: u64,

    /// Total number of context switches performed.
    context_switches: u64,
}

/// Get a clone of a task's Arc by PID.
pub fn get_task_arc(pid: Pid) -> Option<Arc<spin::Mutex<Task>>> {
    let tasks = TASKS.read();
    let idx = pid.as_u64() as usize;
    tasks.get(idx)?.as_ref().cloned()
}

impl Scheduler {
    /// Create a new scheduler.
    pub(crate) fn new() -> Self {
        Self {
            queues: [
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
            ],
            current_cpus: [None; 32],
            idle_cpus: [Pid::IDLE; 32],
            suspending_tasks: [None; 32],
            ticks_since_boost: 0,
            context_switches: 0,
        }
    }

    /// Select the next task to run.
    ///
    /// Returns the PID of the highest-priority ready task, or None
    /// if no tasks are ready.
    pub fn pick_next(&mut self) -> Option<(Pid, Priority)> {
        let tasks = TASKS.read();
        // Check queues from highest to lowest priority
        for queue in &mut self.queues {
            let mut i = 0;
            while i < queue.len() {
                let pid = queue[i];
                // Never pick a task that is currently running or suspending on any CPU core
                if self.current_cpus.iter().any(|&c| c == Some(pid))
                    || self.suspending_tasks.iter().any(|&s| s == Some(pid))
                {
                    i += 1;
                    continue;
                }

                let idx = pid.as_u64() as usize;
                if let Some(Some(task_arc)) = tasks.get(idx) {
                    if let Some(mut task) = task_arc.try_lock() {
                        if task.state == TaskState::Ready {
                            queue.remove(i);
                            task.in_queue = false;
                            let priority = task.priority;
                            return Some((pid, priority));
                        }
                    } else {
                        // Skip locked tasks in this round to prevent permanent queue loss / starvation
                        i += 1;
                        continue;
                    }
                }
                queue.remove(i);
            }
        }
        None
    }

    /// Called on each timer tick to handle preemption.
    pub fn tick(&mut self) {
        self.ticks_since_boost += 1;

        // Check for timed futex expirations
        crate::syscall::process::futex::check_futex_timeouts_locked(self);

        // Periodic priority boost to prevent starvation
        if self.ticks_since_boost >= BOOST_INTERVAL {
            self.boost_priorities();
            self.ticks_since_boost = 0;
        }

        // Check if current task on the active core has exceeded its time quantum
        let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
        if apic_id < 32 {
            if let Some(current_pid) = self.current_cpus[apic_id] {
                let idx = current_pid.as_u64() as usize;
                let tasks = TASKS.read();
                if let Some(Some(task_arc)) = tasks.get(idx) {
                    if let Some(mut task) = task_arc.try_lock() {
                        task.cpu_ticks += 1;
                        if task.cpu_ticks % TIME_QUANTUM == 0 {
                            // Demote the task if it's not already at the lowest priority
                            if !task.is_idle && task.priority < Priority::Idle {
                                task.priority = match task.priority {
                                    Priority::RealTime => Priority::High,
                                    Priority::High => Priority::Normal,
                                    Priority::Normal => Priority::Low,
                                    Priority::Low => Priority::Idle,
                                    Priority::Idle => Priority::Idle,
                                };
                            }
                        }
                    }
                }
            }
        }
    }

    /// Boost all tasks to the highest non-realtime priority.
    fn boost_priorities(&mut self) {
        let tasks = TASKS.read();

        // Boost Normal (2) and Low (3) priority tasks in the queues to High (1) priority.
        for prio in [2, 3] {
            let mut temp_queue = core::mem::take(&mut self.queues[prio]);
            while let Some(pid) = temp_queue.pop_front() {
                let idx = pid.as_u64() as usize;
                if let Some(Some(task_arc)) = tasks.get(idx) {
                    if let Some(mut task) = task_arc.try_lock() {
                        if !task.is_idle && task.state == TaskState::Ready {
                            task.priority = Priority::High;
                            self.queues[1].push_back(pid);
                            task.in_queue = true;
                            continue;
                        }
                    }
                }
                // If try_lock failed or task is not Ready, put it back in its original queue
                self.queues[prio].push_back(pid);
            }
        }
    }

    /// Mark a task as blocked.
    pub fn block_task(&mut self, pid: Pid) {
        let idx = pid.as_u64() as usize;
        let tasks = TASKS.read();
        if let Some(Some(task_arc)) = tasks.get(idx) {
            task_arc.lock().state = TaskState::Blocked;
        }
    }

    /// Wake up a blocked task, making it ready to run.
    /// Returns `true` if the task was in `TaskState::Blocked` and transitioned to `TaskState::Ready`.
    pub fn wake_task(&mut self, pid: Pid) -> bool {
        let idx = pid.as_u64() as usize;
        let tasks = TASKS.read();
        if let Some(Some(task_arc)) = tasks.get(idx) {
            let mut task = task_arc.lock();
            if task.state == TaskState::Blocked {
                task.state = TaskState::Ready;
                if !task.in_queue {
                    let priority = task.priority as usize;
                    self.queues[priority].push_back(pid);
                    task.in_queue = true;
                }
                return true;
            }
        }
        false
    }

    /// Terminate a task.
    pub fn exit_task(
        &mut self,
        pid: Pid,
        exit_code: i32,
    ) -> Vec<Option<Arc<crate::fs::file::FileDescription>>> {
        // Drain stale futex registrations (safety net — primary drain is in exit_current_thread).
        crate::syscall::process::futex::futex_drain_pid_locked(pid, self);

        // Re-parent orphan children of the exiting task to PID 1 (INIT)
        let mut adopted_any = false;
        let tasks = TASKS.read();
        for task_opt in tasks.iter() {
            if let Some(task_arc) = task_opt {
                let mut other_task = task_arc.lock();
                if other_task.parent_pid == pid {
                    other_task.parent_pid = Pid::INIT;
                    adopted_any = true;
                }
            }
        }
        drop(tasks);

        if adopted_any {
            let init_idx = Pid::INIT.as_u64() as usize;
            let tasks = TASKS.read();
            if let Some(Some(init_task_arc)) = tasks.get(init_idx) {
                let init_wait_queue = {
                    let init_task = init_task_arc.lock();
                    init_task.child_wait_queue.clone()
                };
                init_wait_queue.wake_all_locked(self);
                self.wake_task(Pid::INIT);
            }
            drop(tasks);
        }

        let idx = pid.as_u64() as usize;
        let mut parent_pid = None;
        let mut fds_to_drop = Vec::new();
        let mut is_thread = false;
        let tasks = TASKS.read();
        if let Some(Some(task_arc)) = tasks.get(idx) {
            let mut task = task_arc.lock();
            task.state = TaskState::Zombie;
            task.exit_code = Some(exit_code);
            parent_pid = Some(task.parent_pid);
            is_thread = task.tgid != pid;

            let old_fd_table = task.fd_table.clone();
            task.fd_table = Arc::new(spin::Mutex::new(crate::process::task::FdTable {
                entries: Vec::new(),
                cloexec: Vec::new(),
            }));
            // Only clear fd_table entries if this task was the sole owner of the fd_table
            // (strong_count == 1 because only old_fd_table holds the last reference after detaching).
            if Arc::strong_count(&old_fd_table) == 1 {
                let mut fd_table = old_fd_table.lock();
                fds_to_drop = core::mem::take(&mut fd_table.entries);
                fd_table.cloexec.clear();
            }
        }
        drop(tasks); // Drop TASKS read lock before calling wake_task to keep correct order

        if is_thread {
            // Non-leader threads (CLONE_THREAD) do not send SIGCHLD to the parent process.
            // Their task slot in TASKS will be safely reaped after the context switch away in schedule().
            return fds_to_drop;
        }

        if let Some(parent) = parent_pid {
            // Wake the parent task if it was blocked waiting
            self.wake_task(parent);

            // Deliver SIGCHLD (17) to the parent's pending signals
            let parent_idx = parent.as_u64() as usize;
            let tasks = TASKS.read();
            if let Some(Some(parent_task_arc)) = tasks.get(parent_idx) {
                // Clone the child wait queue Arc to avoid holding a lock borrow
                let child_wait_queue = {
                    let parent_task = parent_task_arc.lock();
                    parent_task.child_wait_queue.clone()
                };

                child_wait_queue.wake_all_locked(self);

                let mut parent_task = parent_task_arc.lock();
                parent_task.pending_signals |= 1 << (17 - 1);

                // Scan current_cpus to find which core (if any) is running the parent task
                let mut parent_core = None;
                for core_id in 0..32 {
                    if self.current_cpus[core_id] == Some(parent) {
                        parent_core = Some(core_id);
                        break;
                    }
                }

                if let Some(core_id) = parent_core {
                    let pending_unblocked =
                        parent_task.pending_signals & !parent_task.blocked_signals;
                    unsafe {
                        crate::syscall::CPU_SCRATCHES[core_id].signals_pending =
                            if pending_unblocked != 0 { 1 } else { 0 };
                    }
                }

                // Wake parent task from blocked state if it was waiting
                if parent_task.state == TaskState::Blocked {
                    parent_task.state = TaskState::Ready;
                    if !parent_task.in_queue {
                        let priority = parent_task.priority as usize;
                        self.queues[priority].push_back(parent);
                        parent_task.in_queue = true;
                    }
                }
            }
        }
        fds_to_drop
    }

    /// Get the currently running task's PID.
    pub fn current_pid(&self) -> Option<Pid> {
        let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
        if apic_id < 32 {
            self.current_cpus[apic_id]
        } else {
            None
        }
    }

    /// Add a task to this scheduler instance.
    pub fn add_task(&mut self, task: Task) {
        let pid = task.pid;
        let priority = task.priority as usize;
        let idx = pid.as_u64() as usize;

        let task_arc = Arc::new(spin::Mutex::new(task));
        task_arc.lock().in_queue = true;

        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut tasks = TASKS.write();
            while tasks.len() <= idx {
                tasks.push(None);
            }
            tasks[idx] = Some(task_arc);
        });

        self.queues[priority].push_back(pid);
    }
}

#[unsafe(naked)]
extern "C" fn idle_trampoline() -> ! {
    core::arch::naked_asm!(
        "call {}",
        "1:",
        "sti",
        "hlt",
        "jmp 1b",
        sym scheduler_unlock_after_switch,
    );
}

/// Initialize the scheduler with the idle task.
pub fn init() {
    let mut scheduler = Scheduler::new();

    let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
    let cr3_val = cr3_frame.start_address().as_u64();

    let mut tasks = TASKS.write();

    // Create 32 separate idle tasks (one for each potential CPU core)
    for i in 0..32 {
        let mut idle_task = Task::idle();
        idle_task.is_idle = true;

        // Allocate a unique kernel stack for each core's idle task (32 KiB)
        let layout = alloc::alloc::Layout::from_size_align(32768, 16).unwrap();
        let stack_base = unsafe { alloc::alloc::alloc(layout) } as u64;
        idle_task.kernel_stack_base = stack_base;
        idle_task.kernel_stack_size = 32768;

        let stack_top = stack_base + 32768;
        let stack_top_aligned = stack_top & !0xF;

        let mut context = super::context::CpuContext::new(
            idle_trampoline as *const () as u64,
            stack_top_aligned,
            cr3_val,
        );
        context.rflags = 0x200; // IF (Interrupt Flag) enabled
        idle_task.context = context;

        // Use PID 900 + i for the idle task of Core i
        let pid_val = 900 + i as u64;
        idle_task.pid = Pid::from_raw(pid_val);

        let task_arc = Arc::new(spin::Mutex::new(idle_task));
        let idx = pid_val as usize;
        while tasks.len() <= idx {
            tasks.push(None);
        }
        tasks[idx] = Some(task_arc);

        scheduler.idle_cpus[i] = Pid::from_raw(pid_val);
        scheduler.current_cpus[i] = Some(Pid::from_raw(pid_val));
    }
    drop(tasks);

    *SCHEDULER.lock() = Some(scheduler);
    kprintln!("[scheduler] MLFQ scheduler initialized with 32 unique idle tasks.");
}

/// Add a task to the scheduler.
pub fn add_task(task: Task) {
    let pid = task.pid;
    let name = task.name.clone();
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(ref mut scheduler) = *SCHEDULER.lock() {
            scheduler.add_task(task);
        }
    });
    kprintln!("[scheduler] Added task: PID {} ({})", pid, name);
}

/// Called on each timer tick.
pub fn tick() {
    if let Some(mut sched_lock) = SCHEDULER.try_lock() {
        if let Some(ref mut scheduler) = *sched_lock {
            scheduler.tick();
        }
    }
}

pub static GS_BASE_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Get the current task's PID (lock-free).
pub fn current_pid() -> Option<Pid> {
    let core_id = (crate::arch::x86_64::smp::current_lapic_id() as usize) % 32;
    unsafe {
        let pid_val = crate::syscall::CPU_SCRATCHES[core_id].current_pid;
        if pid_val != 0xFFFF_FFFF_FFFF_FFFF && pid_val != 0 {
            return Some(Pid::from_raw(pid_val));
        }
    }

    let apic_id = crate::arch::x86_64::smp::current_lapic_id() as u32;
    if SCHEDULER.holding_cpu_id() == apic_id {
        unsafe {
            if let Some(ref sched) = *SCHEDULER.get_mut_unchecked() {
                sched.current_cpus[core_id]
            } else {
                None
            }
        }
    } else if let Some(sched_lock) = SCHEDULER.try_lock() {
        if let Some(ref sched) = *sched_lock {
            sched.current_cpus[core_id]
        } else {
            None
        }
    } else {
        unsafe {
            let pid_val = crate::syscall::CPU_SCRATCHES[core_id].current_pid;
            if pid_val != 0xFFFF_FFFF_FFFF_FFFF && pid_val != 0 {
                Some(Pid::from_raw(pid_val))
            } else {
                None
            }
        }
    }
}

/// Cooperative yield: give up the remaining time slice of the current task.
pub fn yield_now() {
    schedule();
}

pub use super::lifecycle::exit_current_thread;

/// Trigger the scheduler to run the next ready task.
pub fn schedule() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched_lock = SCHEDULER.lock();
        let scheduler = match &mut *sched_lock {
            Some(s) => s,
            None => return,
        };

        let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
        if apic_id >= 32 {
            return;
        }

        let current_pid = match scheduler.current_cpus[apic_id] {
            Some(pid) => pid,
            None => return,
        };

        // If the current task is already locked by the interrupted thread on this core,
        // we cannot safely reschedule. Skip this tick.
        let current_idx = current_pid.as_u64() as usize;
        {
            let tasks = TASKS.read();
            if let Some(Some(current_task_arc)) = tasks.get(current_idx) {
                if current_task_arc.try_lock().is_none() {
                    return;
                }
            }
        }

        // Pick the next task to run
        let next_pid = match scheduler.pick_next() {
            Some((pid, prio)) => {
                let next_idx = pid.as_u64() as usize;
                let tasks = TASKS.read();
                let is_locked = if let Some(Some(next_task_arc)) = tasks.get(next_idx) {
                    next_task_arc.try_lock().is_none()
                } else {
                    false
                };
                if is_locked {
                    let prio_idx = prio as usize;
                    scheduler.queues[prio_idx].push_back(pid);
                    return;
                }
                pid
            }
            None => {
                // No other ready tasks; keep running current if it's still runnable
                let current_idx = current_pid.as_u64() as usize;
                let tasks = TASKS.read();

                if let Some(Some(current_task_arc)) = tasks.get(current_idx) {
                    let mut current_task = current_task_arc.lock();
                    if current_task.state == TaskState::Running
                        || current_task.state == TaskState::Ready
                    {
                        current_task.state = TaskState::Running;
                        scheduler.suspending_tasks[apic_id] = None;
                        return;
                    }
                }

                // Otherwise, switch to this core's idle task
                scheduler.idle_cpus[apic_id]
            }
        };

        if next_pid == current_pid {
            // Restore current task's state to Running if it was set to Ready
            let current_idx = current_pid.as_u64() as usize;
            let tasks = TASKS.read();
            if let Some(Some(current_task_arc)) = tasks.get(current_idx) {
                current_task_arc.lock().state = TaskState::Running;
            }
            return; // No switch needed
        }

        // Prepare for switch
        let current_idx = current_pid.as_u64() as usize;
        let next_idx = next_pid.as_u64() as usize;

        // Get stable pointers to the task context structures from heap-allocated Tasks in Arc.
        let tasks = TASKS.read();
        let current_task_arc = tasks.get(current_idx).and_then(|t| t.clone());
        let next_task_arc = match tasks.get(next_idx).and_then(|t| t.clone()) {
            Some(t) => t,
            None => {
                drop(tasks);
                return;
            }
        };
        drop(tasks); // Release TASKS read lock

        let old_ctx_ptr = if let Some(ref current_task_arc) = current_task_arc {
            let mut current_task = current_task_arc.lock();
            if current_task.state == TaskState::Running {
                current_task.state = TaskState::Ready;
            }
            // Always set suspending task to prevent scheduling before context switch completes
            scheduler.suspending_tasks[apic_id] = Some(current_pid);
            &mut current_task.context as *mut super::context::CpuContext
        } else {
            scheduler.suspending_tasks[apic_id] = None;
            unsafe { core::ptr::addr_of_mut!(DUMMY_CONTEXTS[apic_id]) }
        };

        let pending_unblocked;
        let new_ctx_ptr = {
            let mut next_task = next_task_arc.lock();
            next_task.state = TaskState::Running;

            // Set the active task's kernel stack pointer in syscall module and TSS
            if next_task.kernel_stack_base != 0 {
                let stack_top = next_task.kernel_stack_base + next_task.kernel_stack_size as u64;
                crate::syscall::set_kernel_stack(stack_top);
                crate::arch::x86_64::gdt::set_interrupt_stack(stack_top);
            }

            pending_unblocked = next_task.pending_signals & !next_task.blocked_signals;

            &next_task.context as *const super::context::CpuContext
        };

        scheduler.current_cpus[apic_id] = Some(next_pid);
        scheduler.context_switches += 1;

        // Update CPU-local scratch space with the new PID and pending signals
        unsafe {
            if apic_id < 32 {
                crate::syscall::CPU_SCRATCHES[apic_id].current_pid = next_pid.as_u64();
                crate::syscall::CPU_SCRATCHES[apic_id].signals_pending =
                    if pending_unblocked != 0 { 1 } else { 0 };
            }
        }

        let _from_pid = current_pid;
        let _to_pid = next_pid;

        #[cfg(feature = "test")]
        crate::kprintln!(
            "[sched] Core {} switching from PID {} to PID {}",
            apic_id,
            current_pid,
            next_pid
        );

        // Disarm the RAII lock guard before switching stacks so it doesn't double-unlock when we resume later
        core::mem::forget(sched_lock);

        // Perform raw context switch directly
        unsafe {
            super::context::switch_context(old_ctx_ptr, new_ctx_ptr);
        }

        // Re-read apic_id and get a fresh reference to Scheduler from memory to bypass register caching
        let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
        let mut zombie_to_reap = None;
        unsafe {
            if let Some(ref mut scheduler) = *SCHEDULER.get_mut_unchecked() {
                if let Some(suspended_pid) = scheduler.suspending_tasks[apic_id].take() {
                    let idx = suspended_pid.as_u64() as usize;
                    let tasks = TASKS.read();
                    let is_zombie_thread = if let Some(Some(suspended_task_arc)) = tasks.get(idx) {
                        let mut suspended_task = suspended_task_arc.lock();
                        if suspended_task.state == TaskState::Ready {
                            if !suspended_task.is_idle && !suspended_task.in_queue {
                                let prio = suspended_task.priority as usize;
                                scheduler.queues[prio].push_back(suspended_pid);
                                suspended_task.in_queue = true;
                            }
                            false
                        } else {
                            suspended_task.state == TaskState::Zombie
                                && suspended_task.tgid != suspended_pid
                        }
                    } else {
                        false
                    };
                    drop(tasks);

                    if is_zombie_thread {
                        zombie_to_reap = Some(idx);
                    }
                }
            }
            SCHEDULER.force_unlock();
        }

        if let Some(idx) = zombie_to_reap {
            let mut tasks_write = TASKS.write();
            if let Some(slot) = tasks_write.get_mut(idx) {
                slot.take();
            }
        }
    });
}

/// Release the scheduler lock and enable interrupts after context switch in trampolines.
///
/// # Safety
/// This is unsafe because it manually releases the global scheduler lock and enables interrupts.
#[no_mangle]
pub unsafe extern "C" fn scheduler_unlock_after_switch() {
    let mut zombie_to_reap = None;
    unsafe {
        if let Some(ref mut scheduler) = *SCHEDULER.get_mut_unchecked() {
            let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
            if let Some(suspended_pid) = scheduler.suspending_tasks[apic_id].take() {
                let idx = suspended_pid.as_u64() as usize;
                let tasks = TASKS.read();
                let is_zombie_thread = if let Some(Some(suspended_task_arc)) = tasks.get(idx) {
                    let mut suspended_task = suspended_task_arc.lock();
                    if suspended_task.state == TaskState::Ready {
                        if !suspended_task.is_idle && !suspended_task.in_queue {
                            let prio = suspended_task.priority as usize;
                            scheduler.queues[prio].push_back(suspended_pid);
                            suspended_task.in_queue = true;
                        }
                        false
                    } else {
                        suspended_task.state == TaskState::Zombie
                            && suspended_task.tgid != suspended_pid
                    }
                } else {
                    false
                };
                drop(tasks);

                if is_zombie_thread {
                    zombie_to_reap = Some(idx);
                }
            }
        }
        SCHEDULER.force_unlock();

        if let Some(idx) = zombie_to_reap {
            let mut tasks_write = TASKS.write();
            if let Some(slot) = tasks_write.get_mut(idx) {
                slot.take();
            }
        }
    }
}

/// Register the current boot thread as a running task in the scheduler (PID 1).
pub fn set_bootstrap_thread(task: Task) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(ref mut scheduler) = *SCHEDULER.lock() {
            let pid = task.pid;
            let idx = pid.as_u64() as usize;

            let task_arc = Arc::new(spin::Mutex::new(task));
            task_arc.lock().state = TaskState::Running;

            let mut tasks = TASKS.write();
            while tasks.len() <= idx {
                tasks.push(None);
            }
            tasks[idx] = Some(task_arc);
            drop(tasks);

            let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
            if apic_id < 32 {
                scheduler.current_cpus[apic_id] = Some(pid);
            }

            // Update CPU-local scratch space for the current bootstrap thread
            unsafe {
                if apic_id < 32 {
                    crate::syscall::CPU_SCRATCHES[apic_id].current_pid = pid.as_u64();
                    crate::syscall::CPU_SCRATCHES[apic_id].signals_pending = 0;
                }
            }
        }
    });
}

pub use super::lifecycle::wake_task;
