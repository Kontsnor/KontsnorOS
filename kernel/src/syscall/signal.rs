//! Signal-related syscalls — kill, sigaction, sigprocmask, sigreturn.

use super::{Errno, SyscallResult};
use crate::kprintln;

/// Standard POSIX signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
#[allow(dead_code)]
pub enum Signal {
    /// Hangup.
    SIGHUP = 1,
    /// Interrupt (Ctrl+C).
    SIGINT = 2,
    /// Quit.
    SIGQUIT = 3,
    /// Illegal instruction.
    SIGILL = 4,
    /// Abort.
    SIGABRT = 6,
    /// Floating point exception.
    SIGFPE = 8,
    /// Kill (cannot be caught or ignored).
    SIGKILL = 9,
    /// Segmentation fault.
    SIGSEGV = 11,
    /// Broken pipe.
    SIGPIPE = 13,
    /// Alarm clock.
    SIGALRM = 14,
    /// Termination.
    SIGTERM = 15,
    /// Child process status change.
    SIGCHLD = 17,
    /// Continue.
    SIGCONT = 18,
    /// Stop (cannot be caught or ignored).
    SIGSTOP = 19,
    /// Terminal stop (Ctrl+Z).
    SIGTSTP = 20,
}

/// Send a signal to a process.
pub fn deliver_signal(pid: crate::process::pid::Pid, sig: i32) {
    use crate::process::scheduler;
    if sig < 1 || sig > 64 {
        return;
    }

    if let Some(task_arc) = scheduler::get_task_arc(pid) {
        // Scan current_cpus to find which core (if any) is running the target task
        // We lock SCHEDULER briefly only to scan current_cpus
        let mut target_core = None;
        let sched_lock = scheduler::SCHEDULER.lock();
        if let Some(ref sched) = *sched_lock {
            for core_id in 0..32 {
                if sched.current_cpus[core_id] == Some(pid) {
                    target_core = Some(core_id);
                    break;
                }
            }
        }
        drop(sched_lock);

        let mut task = task_arc.lock();
        task.pending_signals |= 1 << (sig - 1);
        if let Some(core_id) = target_core {
            let pending_unblocked = task.pending_signals & !task.blocked_signals;
            unsafe {
                crate::syscall::CPU_SCRATCHES[core_id].signals_pending =
                    if pending_unblocked != 0 { 1 } else { 0 };
            }
        }
        drop(task);
        scheduler::wake_task(pid);
        crate::fs::epoll::wake_all_epolls();
    }
}

/// `kill(pid, sig)` — Send a signal to a process.
pub fn sys_kill(pid: i32, sig: i32) -> SyscallResult {
    use crate::process::pid::Pid;
    kprintln!("[syscall] kill(pid={}, sig={})", pid, sig);
    if sig < 0 || sig > 64 {
        return Errno::EINVAL.into();
    }
    if pid <= 0 {
        return Errno::ENOSYS.into(); // We don't support process groups/broadcast yet
    }
    deliver_signal(Pid::from_raw(pid as u64), sig);
    0
}

/// `rt_sigaction(signum, act, oldact, sigsetsize)` — Set signal handler.
pub fn sys_rt_sigaction(
    signum: i32,
    act: *const crate::process::task::SigAction,
    oldact: *mut crate::process::task::SigAction,
    sigsetsize: usize,
) -> SyscallResult {
    if signum < 1 || signum > 64 || sigsetsize != 8 {
        return Errno::EINVAL.into();
    }
    if signum == 9 || signum == 19 {
        // SIGKILL, SIGSTOP cannot be caught
        return Errno::EINVAL.into();
    }

    if !act.is_null()
        && !crate::syscall::fs::validate_user_ptr(
            act as *const u8,
            core::mem::size_of::<crate::process::task::SigAction>(),
        )
    {
        return Errno::EFAULT.into();
    }
    if !oldact.is_null()
        && !crate::syscall::fs::validate_user_ptr(
            oldact as *const u8,
            core::mem::size_of::<crate::process::task::SigAction>(),
        )
    {
        return Errno::EFAULT.into();
    }

    use crate::process::scheduler;
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let mut task = task_arc.lock();

    if !oldact.is_null() {
        unsafe {
            core::ptr::write(oldact, task.sigactions[(signum - 1) as usize]);
        }
    }
    if !act.is_null() {
        unsafe {
            task.sigactions[(signum - 1) as usize] = core::ptr::read(act);
        }
    }
    0
}

/// `rt_sigprocmask(how, set, oldset, sigsetsize)` — Examine and change blocked signals.
pub fn sys_rt_sigprocmask(
    how: i32,
    set: *const u64,
    oldset: *mut u64,
    sigsetsize: usize,
) -> SyscallResult {
    if sigsetsize != 8 {
        return Errno::EINVAL.into();
    }

    if !set.is_null()
        && !crate::syscall::fs::validate_user_ptr(set as *const u8, core::mem::size_of::<u64>())
    {
        return Errno::EFAULT.into();
    }
    if !oldset.is_null()
        && !crate::syscall::fs::validate_user_ptr(oldset as *const u8, core::mem::size_of::<u64>())
    {
        return Errno::EFAULT.into();
    }

    use crate::process::scheduler;
    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };

    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };
    let mut task = task_arc.lock();

    if !oldset.is_null() {
        unsafe {
            core::ptr::write(oldset, task.blocked_signals);
        }
    }
    if !set.is_null() {
        let new_set = unsafe { core::ptr::read(set) };
        match how {
            0 => {
                // SIG_BLOCK
                task.blocked_signals |= new_set;
            }
            1 => {
                // SIG_UNBLOCK
                task.blocked_signals &= !new_set;
            }
            2 => {
                // SIG_SETMASK
                task.blocked_signals = new_set;
            }
            _ => return Errno::EINVAL.into(),
        }
        // SIGKILL (9) and SIGSTOP (19) cannot be blocked
        task.blocked_signals &= !((1 << 8) | (1 << 18));
    }

    let pending_unblocked = task.pending_signals & !task.blocked_signals;
    let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
    unsafe {
        if apic_id < 32 {
            crate::syscall::CPU_SCRATCHES[apic_id].signals_pending =
                if pending_unblocked != 0 { 1 } else { 0 };
        }
    }
    0
}

/// User space signal handler trampoline frame.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SignalFrame {
    pub ret_addr: u64,
    pub rflags: u64,
    pub rip: u64,
    pub rax: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub mask: u64, // Saved signal mask (blocked_signals)
}

/// Delivers pending unblocked signals to the current process.
pub fn handle_pending_signals(regs: *mut super::SavedRegisters) {
    use crate::process::scheduler;

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return,
    };

    let (sig, action, old_mask) = {
        let task_arc = match scheduler::get_task_arc(current_pid) {
            Some(t) => t,
            None => return,
        };
        let mut task = task_arc.lock();

        let unblocked = task.pending_signals & !task.blocked_signals;
        if unblocked == 0 {
            let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
            unsafe {
                if apic_id < 32 {
                    crate::syscall::CPU_SCRATCHES[apic_id].signals_pending = 0;
                }
            }
            return;
        }

        let mut active_sig = 0;
        for i in 1..=64 {
            if (unblocked & (1 << (i - 1))) != 0 {
                active_sig = i;
                break;
            }
        }

        if active_sig == 0 {
            return;
        }

        task.pending_signals &= !(1 << (active_sig - 1));

        let action = task.sigactions[(active_sig - 1) as usize];
        let old_mask = task.blocked_signals;

        if (action.sa_flags & 0x40000000) == 0 {
            task.blocked_signals |= 1 << (active_sig - 1);
        }
        task.blocked_signals |= action.sa_mask;
        task.blocked_signals &= !((1 << 8) | (1 << 18));

        let pending_unblocked = task.pending_signals & !task.blocked_signals;
        let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
        unsafe {
            if apic_id < 32 {
                crate::syscall::CPU_SCRATCHES[apic_id].signals_pending =
                    if pending_unblocked != 0 { 1 } else { 0 };
            }
        }

        (active_sig, action, old_mask)
    };

    if action.sa_handler == 1 {
        // SIG_IGN
        return;
    } else if action.sa_handler == 0 {
        // SIG_DFL
        if sig == 17 || sig == 18 || sig == 28 {
            return;
        }
        kprintln!(
            "[signal] Default action for signal {} is termination. Exiting task.",
            sig
        );
        scheduler::exit_current_thread(sig | 128);
    } else {
        let user_sp = unsafe { (*regs).rsp };
        let new_user_sp = (user_sp - core::mem::size_of::<SignalFrame>() as u64) & !0xF;

        if !crate::syscall::fs::validate_user_ptr(
            new_user_sp as *const u8,
            core::mem::size_of::<SignalFrame>(),
        ) {
            kprintln!("[signal] Invalid user stack for signal delivery. Exiting task.");
            scheduler::exit_current_thread(11 | 128); // SIGSEGV
        }

        let frame = SignalFrame {
            ret_addr: action.sa_restorer,
            rflags: unsafe { (*regs).rflags },
            rip: unsafe { (*regs).rip },
            rax: unsafe { (*regs).rax },
            rbx: unsafe { (*regs).rbx },
            rbp: unsafe { (*regs).rbp },
            rsp: user_sp,
            rdi: unsafe { (*regs).rdi },
            rsi: unsafe { (*regs).rsi },
            rdx: unsafe { (*regs).rdx },
            rcx: unsafe { (*regs).rip }, // Aliased to user RIP (rcx) on the syscall path
            r8: unsafe { (*regs).r8 },
            r9: unsafe { (*regs).r9 },
            r10: unsafe { (*regs).r10 },
            r11: unsafe { (*regs).rflags }, // Aliased to user RFLAGS (r11) on the syscall path
            r12: unsafe { (*regs).r12 },
            r13: unsafe { (*regs).r13 },
            r14: unsafe { (*regs).r14 },
            r15: unsafe { (*regs).r15 },
            mask: old_mask,
        };

        unsafe {
            core::ptr::write(new_user_sp as *mut SignalFrame, frame);
            (*regs).rsp = new_user_sp;
            (*regs).rip = action.sa_handler;
            (*regs).rdi = sig as u64;
        }

        kprintln!(
            "[signal] Delivered signal {} to custom handler at {:#x}, trampoline user stack: {:#x}",
            sig,
            action.sa_handler,
            new_user_sp
        );
    }
}

/// `sys_rt_sigreturn` — Return from signal handler.
pub fn sys_rt_sigreturn(regs: *mut super::SavedRegisters) -> SyscallResult {
    let user_sp = unsafe { (*regs).rsp };
    let frame_ptr = (user_sp - 8) as *const SignalFrame;

    if !crate::syscall::fs::validate_user_ptr(
        frame_ptr as *const u8,
        core::mem::size_of::<SignalFrame>(),
    ) {
        return Errno::EFAULT.into();
    }

    unsafe {
        let frame = &*frame_ptr;
        (*regs).rflags = (frame.rflags & !0x3000) | 0x202; // Strip IOPL, enable interrupts
        (*regs).rip = frame.rip;
        (*regs).rax = frame.rax;
        (*regs).rbx = frame.rbx;
        (*regs).rbp = frame.rbp;
        (*regs).rdi = frame.rdi;
        (*regs).rsi = frame.rsi;
        (*regs).rdx = frame.rdx;
        (*regs).r8 = frame.r8;
        (*regs).r9 = frame.r9;
        (*regs).r10 = frame.r10;
        (*regs).r12 = frame.r12;
        (*regs).r13 = frame.r13;
        (*regs).r14 = frame.r14;
        (*regs).r15 = frame.r15;

        (*regs).rsp = frame.rsp;

        // Restore saved signal mask
        use crate::process::scheduler;
        if let Some(current_pid) = scheduler::current_pid() {
            if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
                let mut task = task_arc.lock();
                task.blocked_signals = frame.mask;
                // SIGKILL (9) and SIGSTOP (19) cannot be blocked
                task.blocked_signals &= !((1 << 8) | (1 << 18));

                let pending_unblocked = task.pending_signals & !task.blocked_signals;
                let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
                unsafe {
                    if apic_id < 32 {
                        crate::syscall::CPU_SCRATCHES[apic_id].signals_pending =
                            if pending_unblocked != 0 { 1 } else { 0 };
                    }
                }
            }
        }

        kprintln!(
            "[signal] sys_rt_sigreturn: restored execution context to RIP={:#x}, RSP={:#x}",
            frame.rip,
            frame.rsp
        );

        (*regs).rax as SyscallResult
    }
}
