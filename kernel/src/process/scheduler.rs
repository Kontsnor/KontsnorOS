//! Process scheduler — Multi-Level Feedback Queue (MLFQ).
//!
//! The scheduler manages which tasks run on the CPU. KontsnorOS uses
//! a Multi-Level Feedback Queue (MLFQ) algorithm, which provides:
//!
//! - **Good interactive response**: High-priority tasks run first
//! - **Fairness**: Tasks that use too much CPU time are demoted
//! - **No starvation**: Periodic priority boost prevents starvation
//!
//! ## Priority Queues
//!
//! ```text
//! Queue 0 (RealTime):  [task] → [task] → ...  (highest priority)
//! Queue 1 (High):      [task] → [task] → ...
//! Queue 2 (Normal):    [task] → [task] → ...
//! Queue 3 (Low):       [task] → [task] → ...
//! Queue 4 (Idle):      [task] → [task] → ...  (lowest priority)
//! ```

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use crate::sync::spinlock::TicketLock;
use crate::kprintln;

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

/// The Multi-Level Feedback Queue scheduler.
pub struct Scheduler {
    /// Priority queues — one per priority level.
    queues: [VecDeque<Pid>; NUM_PRIORITIES],

    /// All tasks indexed by PID.
    pub(crate) tasks: Vec<Option<alloc::boxed::Box<Task>>>,

    /// The currently running task's PID per CPU core.
    pub(crate) current_cpus: [Option<Pid>; 32],

    /// Tick counter for priority boosting.
    ticks_since_boost: u64,

    /// Total number of context switches performed.
    context_switches: u64,
}

impl Scheduler {
    /// Create a new scheduler.
    fn new() -> Self {
        Self {
            queues: [
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
            ],
            tasks: Vec::new(),
            current_cpus: [None; 32],
            ticks_since_boost: 0,
            context_switches: 0,
        }
    }

    /// Add a new task to the scheduler.
    pub fn add_task(&mut self, mut task: Task) {
        let pid = task.pid;
        let priority = task.priority as usize;
        let idx = pid.as_u64() as usize;

        // Ensure the task vector is large enough
        while self.tasks.len() <= idx {
            self.tasks.push(None);
        }

        task.in_queue = true;
        self.tasks[idx] = Some(alloc::boxed::Box::new(task));
        self.queues[priority].push_back(pid);
    }

    /// Select the next task to run.
    ///
    /// Returns the PID of the highest-priority ready task, or None
    /// if no tasks are ready.
    pub fn pick_next(&mut self) -> Option<Pid> {
        // Check queues from highest to lowest priority
        for queue in &mut self.queues {
            while let Some(pid) = queue.pop_front() {
                let idx = pid.as_u64() as usize;
                if let Some(Some(task)) = self.tasks.get_mut(idx) {
                    task.in_queue = false;
                    if task.state == TaskState::Ready {
                        return Some(pid);
                    }
                }
            }
        }
        None
    }

    /// Called on each timer tick to handle preemption.
    pub fn tick(&mut self) {
        self.ticks_since_boost += 1;

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
                if let Some(Some(task)) = self.tasks.get_mut(idx) {
                    task.cpu_ticks += 1;
                    if task.cpu_ticks % TIME_QUANTUM == 0 {
                        // Demote the task if it's not already at the lowest priority
                        if task.priority < Priority::Idle {
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

    /// Boost all tasks to the highest non-realtime priority.
    ///
    /// This prevents starvation by periodically moving all tasks
    /// back to a high priority queue.
    ///
    /// Invariant: Running tasks are boosted in priority, but their PIDs are
    /// not re-added to the queues here. Instead, they will be re-enqueued
    /// with their boosted priority by `schedule()` when they yield or are preempted.
    fn boost_priorities(&mut self) {
        for task in self.tasks.iter_mut().flatten() {
            if task.priority > Priority::High && (task.state == TaskState::Ready || task.state == TaskState::Running) {
                task.priority = Priority::High;
            }
        }

        // Rebuild queues
        for queue in &mut self.queues {
            queue.clear();
        }

        for task in self.tasks.iter_mut().flatten() {
            task.in_queue = false;
        }

        for task in self.tasks.iter_mut().flatten() {
            if task.state == TaskState::Ready {
                let priority = task.priority as usize;
                self.queues[priority].push_back(task.pid);
                task.in_queue = true;
            }
        }
    }

    /// Mark a task as blocked.
    pub fn block_task(&mut self, pid: Pid) {
        let idx = pid.as_u64() as usize;
        if let Some(Some(task)) = self.tasks.get_mut(idx) {
            task.state = TaskState::Blocked;
        }
    }

    /// Wake up a blocked task, making it ready to run.
    pub fn wake_task(&mut self, pid: Pid) {
        let idx = pid.as_u64() as usize;
        if let Some(Some(task)) = self.tasks.get_mut(idx) {
            if task.state == TaskState::Blocked {
                task.state = TaskState::Ready;
                if !task.in_queue {
                    let priority = task.priority as usize;
                    self.queues[priority].push_back(pid);
                    task.in_queue = true;
                }
            }
        }
    }

    /// Terminate a task.
    pub fn exit_task(&mut self, pid: Pid, exit_code: i32) {
        let idx = pid.as_u64() as usize;
        let mut parent_pid = None;
        if let Some(Some(task)) = self.tasks.get_mut(idx) {
            task.state = TaskState::Zombie;
            task.exit_code = Some(exit_code);
            task.fd_table.clear();
            parent_pid = Some(task.parent_pid);
        }

        if let Some(parent) = parent_pid {
            // Wake the parent task if it was blocked waiting
            self.wake_task(parent);

            // Deliver SIGCHLD (17) to the parent's pending signals
            let parent_idx = parent.as_u64() as usize;
            if parent_idx < self.tasks.len() {
                // Clone the child wait queue Arc to avoid holding a mutable borrow of self.tasks
                let child_wait_queue = if let Some(Some(ref parent_task)) = self.tasks.get(parent_idx) {
                    Some(parent_task.child_wait_queue.clone())
                } else {
                    None
                };

                if let Some(wq) = child_wait_queue {
                    wq.wake_all_locked(self);
                }

                if let Some(Some(ref mut parent_task)) = self.tasks.get_mut(parent_idx) {
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
                        let pending_unblocked = parent_task.pending_signals & !parent_task.blocked_signals;
                        unsafe {
                            crate::syscall::CPU_SCRATCHES[core_id].signals_pending = if pending_unblocked != 0 { 1 } else { 0 };
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
        }
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

    /// Get a reference to a task by PID.
    pub fn get_task(&self, pid: Pid) -> Option<&Task> {
        let idx = pid.as_u64() as usize;
        self.tasks.get(idx)?.as_ref().map(|t| &**t)
    }

    /// Get a mutable reference to a task by PID.
    pub fn get_task_mut(&mut self, pid: Pid) -> Option<&mut Task> {
        let idx = pid.as_u64() as usize;
        self.tasks.get_mut(idx)?.as_mut().map(|t| &mut **t)
    }
}

#[unsafe(naked)]
extern "C" fn idle_trampoline() -> ! {
    core::arch::naked_asm!(
        "sti",
        "1:",
        "hlt",
        "jmp 1b"
    );
}

/// Initialize the scheduler with the idle task.
pub fn init() {
    let mut scheduler = Scheduler::new();

    // Create the idle task (PID 0)
    let mut idle_task = Task::idle();
    
    // Allocate stack for idle task (32 KiB)
    let layout = alloc::alloc::Layout::from_size_align(32768, 16).unwrap();
    let stack_base = unsafe { alloc::alloc::alloc(layout) } as u64;
    idle_task.kernel_stack_base = stack_base;
    idle_task.kernel_stack_size = 32768;
    
    let stack_top = stack_base + 32768;
    let stack_top_aligned = stack_top & !0xF;
    
    let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
    let cr3_val = cr3_frame.start_address().as_u64();
    
    let context = super::context::CpuContext::new(
        idle_trampoline as *const () as u64,
        stack_top_aligned,
        cr3_val,
    );
    idle_task.context = context;

    scheduler.add_task(idle_task);
    for slot in scheduler.current_cpus.iter_mut() {
        *slot = Some(Pid::IDLE);
    }

    *SCHEDULER.lock() = Some(scheduler);
    kprintln!("[scheduler] MLFQ scheduler initialized with idle task.");
}

/// Add a task to the scheduler.
pub fn add_task(task: Task) {
    if let Some(ref mut scheduler) = *SCHEDULER.lock() {
        let pid = task.pid;
        let name = task.name.clone();
        scheduler.add_task(task);
        kprintln!("[scheduler] Added task: PID {} ({})", pid, name);
    }
}

/// Called on each timer tick.
pub fn tick() {
    if let Some(ref mut scheduler) = *SCHEDULER.lock() {
        scheduler.tick();
    }
}

/// Get the current task's PID (lock-free).
pub fn current_pid() -> Option<Pid> {
    let pid_val: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, gs:[16]",
            out(reg) pid_val,
            options(nostack, preserves_flags, readonly)
        );
    }
    if pid_val == 0xFFFF_FFFF_FFFF_FFFF {
        None
    } else {
        Some(Pid::from_raw(pid_val))
    }
}

/// Cooperative yield: give up the remaining time slice of the current task.
pub fn yield_now() {
    schedule();
}

/// Exits the currently running task.
pub fn exit_current_thread(exit_code: i32) -> ! {
    x86_64::instructions::interrupts::disable();
    if let Some(current_pid) = current_pid() {
        if let Some(ref mut scheduler) = *SCHEDULER.lock() {
            scheduler.exit_task(current_pid, exit_code);
        }
    }

    schedule();

    // If there is absolutely no other task left (should not happen due to idle task)
    loop {
        x86_64::instructions::hlt();
    }
}

/// Trigger the scheduler to run the next ready task.
///
/// This disables interrupts, selects the highest priority task, releases the global
/// scheduler lock to prevent deadlocks, and invokes the assembly switch_context.
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

        // Pick the next task to run
        let next_pid = match scheduler.pick_next() {
            Some(pid) => pid,
            None => {
                // No other ready tasks; keep running current if it's still runnable
                let current_idx = current_pid.as_u64() as usize;
                if let Some(Some(current_task)) = scheduler.tasks.get(current_idx) {
                    if current_task.state == TaskState::Running || current_task.state == TaskState::Ready {
                        return;
                    }
                }
                // Otherwise, switch to the idle task (PID 0)
                Pid::IDLE
            }
        };

        if next_pid == current_pid {
            // Restore current task's state to Running if it was set to Ready
            let current_idx = current_pid.as_u64() as usize;
            if let Some(Some(ref mut current_task)) = scheduler.tasks.get_mut(current_idx) {
                current_task.state = TaskState::Running;
            }
            return; // No switch needed
        }

        // Prepare for switch
        let current_idx = current_pid.as_u64() as usize;
        let next_idx = next_pid.as_u64() as usize;

        // Get stable pointers to the task context structures (they are boxed in the heap,
        // so their memory addresses are stable and won't change even if tasks reallocates!)
        let old_ctx_ptr = {
            let current_task = scheduler.tasks[current_idx].as_mut().expect("Current task missing");
            if current_task.state == TaskState::Running {
                current_task.state = TaskState::Ready;
                // Re-enqueue current task in its priority queue
                if !current_task.in_queue {
                    let prio = current_task.priority as usize;
                    scheduler.queues[prio].push_back(current_pid);
                    current_task.in_queue = true;
                }
            }
            &mut current_task.context as *mut super::context::CpuContext
        };

        let mut pending_unblocked = 0;
        let new_ctx_ptr = {
            let next_task = scheduler.tasks[next_idx].as_mut().expect("Next task missing");
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
                crate::syscall::CPU_SCRATCHES[apic_id].signals_pending = if pending_unblocked != 0 { 1 } else { 0 };
            }
        }

        // Drop lock before switching to prevent deadlock
        drop(sched_lock);

        // Perform raw context switch directly using the stable heap pointers
        unsafe {
            super::context::switch_context(old_ctx_ptr, new_ctx_ptr);
        }
    });
}

/// Register the current boot thread as a running task in the scheduler (PID 1).
pub fn set_bootstrap_thread(task: Task) {
    if let Some(ref mut scheduler) = *SCHEDULER.lock() {
        let pid = task.pid;
        scheduler.add_task(task);
        
        // Find the newly added task and set its state to Running
        let idx = pid.as_u64() as usize;
        if let Some(Some(ref mut t)) = scheduler.tasks.get_mut(idx) {
            t.state = TaskState::Running;
        }
        
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
}

/// Block a task.
pub fn block_task(pid: Pid) {
    if let Some(ref mut scheduler) = *SCHEDULER.lock() {
        scheduler.block_task(pid);
    }
}

/// Wake up a blocked task.
pub fn wake_task(pid: Pid) {
    if let Some(ref mut scheduler) = *SCHEDULER.lock() {
        scheduler.wake_task(pid);
    }
}

