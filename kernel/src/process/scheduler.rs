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

/// The Multi-Level Feedback Queue scheduler.
pub struct Scheduler {
    /// Priority queues — one per priority level.
    queues: [VecDeque<Pid>; NUM_PRIORITIES],

    /// The currently running task's PID per CPU core.
    pub(crate) current_cpus: [Option<Pid>; 32],

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
            ticks_since_boost: 0,
            context_switches: 0,
        }
    }

    /// Select the next task to run.
    ///
    /// Returns the PID of the highest-priority ready task, or None
    /// if no tasks are ready.
    pub fn pick_next(&mut self) -> Option<Pid> {
        let tasks = TASKS.read();
        // Check queues from highest to lowest priority
        for queue in &mut self.queues {
            while let Some(pid) = queue.pop_front() {
                let idx = pid.as_u64() as usize;
                if let Some(Some(task_arc)) = tasks.get(idx) {
                    let mut task = task_arc.lock();
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
                let tasks = TASKS.read();
                if let Some(Some(task_arc)) = tasks.get(idx) {
                    let mut task = task_arc.lock();
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
    fn boost_priorities(&mut self) {
        let tasks = TASKS.read();
        for task_opt in tasks.iter() {
            if let Some(task_arc) = task_opt {
                let mut task = task_arc.lock();
                if task.priority > Priority::High
                    && (task.state == TaskState::Ready || task.state == TaskState::Running)
                {
                    task.priority = Priority::High;
                }
            }
        }

        // Rebuild queues
        for queue in &mut self.queues {
            queue.clear();
        }

        for task_opt in tasks.iter() {
            if let Some(task_arc) = task_opt {
                task_arc.lock().in_queue = false;
            }
        }

        for task_opt in tasks.iter() {
            if let Some(task_arc) = task_opt {
                let mut task = task_arc.lock();
                if task.state == TaskState::Ready {
                    let priority = task.priority as usize;
                    self.queues[priority].push_back(task.pid);
                    task.in_queue = true;
                }
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
    pub fn wake_task(&mut self, pid: Pid) {
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
            }
        }
    }

    /// Terminate a task.
    pub fn exit_task(&mut self, pid: Pid, exit_code: i32) {
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
        let tasks = TASKS.read();
        if let Some(Some(task_arc)) = tasks.get(idx) {
            let mut task = task_arc.lock();
            task.state = TaskState::Zombie;
            task.exit_code = Some(exit_code);
            task.fd_table.clear();
            parent_pid = Some(task.parent_pid);
        }
        drop(tasks); // Drop TASKS read lock before calling wake_task to keep correct order

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

        let mut tasks = TASKS.write();
        while tasks.len() <= idx {
            tasks.push(None);
        }
        tasks[idx] = Some(task_arc);
        drop(tasks);

        self.queues[priority].push_back(pid);
    }
}

#[unsafe(naked)]
extern "C" fn idle_trampoline() -> ! {
    core::arch::naked_asm!("sti", "1:", "hlt", "jmp 1b");
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

    let idx = Pid::IDLE.as_u64() as usize;
    let task_arc = Arc::new(spin::Mutex::new(idle_task));

    let mut tasks = TASKS.write();
    while tasks.len() <= idx {
        tasks.push(None);
    }
    tasks[idx] = Some(task_arc);
    drop(tasks);

    for slot in scheduler.current_cpus.iter_mut() {
        *slot = Some(Pid::IDLE);
    }

    *SCHEDULER.lock() = Some(scheduler);
    kprintln!("[scheduler] MLFQ scheduler initialized with idle task.");
}

/// Add a task to the scheduler.
pub fn add_task(task: Task) {
    let pid = task.pid;
    let name = task.name.clone();
    let priority = task.priority as usize;
    let idx = pid.as_u64() as usize;

    let task_arc = Arc::new(spin::Mutex::new(task));
    task_arc.lock().in_queue = true;

    let mut tasks = TASKS.write();
    while tasks.len() <= idx {
        tasks.push(None);
    }
    tasks[idx] = Some(task_arc);
    drop(tasks);

    if let Some(ref mut scheduler) = *SCHEDULER.lock() {
        scheduler.queues[priority].push_back(pid);
        kprintln!("[scheduler] Added task: PID {} ({})", pid, name);
    }
}

/// Called on each timer tick.
pub fn tick() {
    if let Some(ref mut scheduler) = *SCHEDULER.lock() {
        scheduler.tick();
    }
}

pub static GS_BASE_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Get the current task's PID (lock-free).
pub fn current_pid() -> Option<Pid> {
    if !GS_BASE_ACTIVE.load(core::sync::atomic::Ordering::Relaxed) {
        return None;
    }
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
                let tasks = TASKS.read();
                if let Some(Some(current_task_arc)) = tasks.get(current_idx) {
                    let current_task = current_task_arc.lock();
                    if current_task.state == TaskState::Running
                        || current_task.state == TaskState::Ready
                    {
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
        let current_task_arc = tasks[current_idx].clone().expect("Current task missing");
        let next_task_arc = tasks[next_idx].clone().expect("Next task missing");
        drop(tasks); // Release TASKS read lock

        let old_ctx_ptr = {
            let mut current_task = current_task_arc.lock();
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

        // Drop lock before switching to prevent deadlock
        drop(sched_lock);

        // Perform raw context switch directly
        unsafe {
            super::context::switch_context(old_ctx_ptr, new_ctx_ptr);
        }
    });
}

/// Register the current boot thread as a running task in the scheduler (PID 1).
pub fn set_bootstrap_thread(task: Task) {
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

    if let Some(ref mut scheduler) = *SCHEDULER.lock() {
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
    let idx = pid.as_u64() as usize;
    let tasks = TASKS.read();
    if let Some(Some(task_arc)) = tasks.get(idx) {
        task_arc.lock().state = TaskState::Blocked;
    }
}

/// Wake up a blocked task.
pub fn wake_task(pid: Pid) {
    let idx = pid.as_u64() as usize;
    let tasks = TASKS.read();
    if let Some(Some(task_arc)) = tasks.get(idx) {
        let mut task = task_arc.lock();
        if task.state == TaskState::Blocked {
            task.state = TaskState::Ready;
            if !task.in_queue {
                if let Some(ref mut scheduler) = *SCHEDULER.lock() {
                    let priority = task.priority as usize;
                    scheduler.queues[priority].push_back(pid);
                    task.in_queue = true;
                }
            }
        }
    }
}
