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

//! Signal-related syscalls — kill, sigaction, sigprocmask, sigreturn.

use super::{Errno, SyscallResult};
use crate::kprintln;

pub const DEBUG_SIGNALS: bool = false;

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
    crate::kprintln!("[signal deliver] sig {} to PID {:?}", sig, pid);

    if let Some(task_arc) = scheduler::get_task_arc(pid) {
        let mut target_core = None;
        x86_64::instructions::interrupts::without_interrupts(|| {
            let sched_lock = scheduler::SCHEDULER.lock();
            if let Some(ref sched) = *sched_lock {
                for core_id in 0..32 {
                    if sched.current_cpus[core_id] == Some(pid) {
                        target_core = Some(core_id);
                        break;
                    }
                }
            }
        });

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
    }
}

/// `kill(pid, sig)` — Send a signal to a process.
pub fn sys_kill(pid: i32, sig: i32) -> SyscallResult {
    use crate::process::pid::Pid;
    // kprintln!("[syscall] kill(pid={}, sig={})", pid, sig);
    if sig < 0 || sig > 64 {
        return Errno::EINVAL.into();
    }

    let caller_ns_id = {
        use crate::process::scheduler;
        scheduler::current_pid()
            .and_then(scheduler::get_task_arc)
            .map(|t| t.lock().pid_ns_id)
            .unwrap_or(0)
    };

    if sig == 0 {
        if pid > 0 {
            use crate::process::scheduler;
            let target_pid = Pid::from_raw(pid as u64);
            if let Some(target_arc) = scheduler::get_task_arc(target_pid) {
                if caller_ns_id != 0 && target_arc.lock().pid_ns_id != caller_ns_id {
                    return Errno::ESRCH.into();
                }
                return 0;
            } else {
                return Errno::ESRCH.into();
            }
        } else if pid == 0 {
            return 0;
        } else if pid < -1 {
            let target_pgid = (-pid) as u64;
            use crate::process::scheduler;
            let tasks = scheduler::TASKS.read();
            let exists = tasks.iter().any(|t| {
                if let Some(task_arc) = t {
                    let task = task_arc.lock();
                    (caller_ns_id == 0 || task.pid_ns_id == caller_ns_id)
                        && task.pgid == target_pgid
                } else {
                    false
                }
            });
            if exists {
                return 0;
            } else {
                return Errno::ESRCH.into();
            }
        } else {
            // pid == -1
            return 0;
        }
    }

    if pid > 0 {
        use crate::process::scheduler;
        let target_pid = Pid::from_raw(pid as u64);
        if let Some(target_arc) = scheduler::get_task_arc(target_pid) {
            if caller_ns_id != 0 && target_arc.lock().pid_ns_id != caller_ns_id {
                return Errno::ESRCH.into();
            }
            deliver_signal(target_pid, sig);
        } else {
            return Errno::ESRCH.into();
        }
    } else if pid == 0 {
        let caller_pgid = {
            use crate::process::scheduler;
            if let Some(curr_pid) = scheduler::current_pid() {
                if let Some(task_arc) = scheduler::get_task_arc(curr_pid) {
                    task_arc.lock().pgid
                } else {
                    1
                }
            } else {
                1
            }
        };
        crate::fs::pty::deliver_signal_to_pgrp(caller_pgid, sig);
    } else if pid < -1 {
        let target_pgid = (-pid) as u64;
        crate::fs::pty::deliver_signal_to_pgrp(target_pgid, sig);
    } else if pid == -1 {
        // pid == -1: broadcast to all processes in the same PID namespace (except caller and namespace init)
        use crate::process::scheduler;
        let caller_pid = scheduler::current_pid().map(|p| p.as_u64()).unwrap_or(0);
        let tasks = scheduler::TASKS.read();
        let mut pids = alloc::vec::Vec::new();
        let mut seen_tgids = alloc::vec::Vec::new();
        for task_opt in tasks.iter() {
            if let Some(task_arc) = task_opt {
                if let Some(task) = task_arc.try_lock() {
                    if caller_ns_id != 0 && task.pid_ns_id != caller_ns_id {
                        continue;
                    }
                    let host_pid = task.pid.as_u64();
                    if host_pid == caller_pid || host_pid <= 1 || task.is_pid_ns_init {
                        continue;
                    }
                    if !task.is_user() {
                        continue;
                    }
                    let tgid = task.tgid;
                    if !seen_tgids.contains(&tgid) {
                        seen_tgids.push(tgid);
                        pids.push(tgid);
                    }
                }
            }
        }
        drop(tasks);
        for target_pid in pids {
            deliver_signal(target_pid, sig);
        }
    }
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
    let task = task_arc.lock();
    let mut sigactions = task.sigactions.lock();

    if !oldact.is_null() {
        unsafe {
            core::ptr::write(oldact, sigactions[(signum - 1) as usize]);
        }
    }
    if !act.is_null() {
        unsafe {
            sigactions[(signum - 1) as usize] = core::ptr::read(act);
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
    let new_set = if !set.is_null() {
        Some(unsafe { core::ptr::read(set) })
    } else {
        None
    };

    let (old_blocked, pending_unblocked) = {
        let mut task = task_arc.lock();
        let old = task.blocked_signals;
        if let Some(new_mask) = new_set {
            match how {
                0 => {
                    // SIG_BLOCK
                    task.blocked_signals |= new_mask;
                }
                1 => {
                    // SIG_UNBLOCK
                    task.blocked_signals &= !new_mask;
                }
                2 => {
                    // SIG_SETMASK
                    task.blocked_signals = new_mask;
                }
                _ => return Errno::EINVAL.into(),
            }
            // SIGKILL (9) and SIGSTOP (19) cannot be blocked
            task.blocked_signals &= !((1 << 8) | (1 << 18));
        }
        let pending = task.pending_signals & !task.blocked_signals;
        (old, pending)
    };

    if !oldset.is_null() {
        unsafe {
            core::ptr::write(oldset, old_blocked);
        }
    }

    let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
    unsafe {
        if apic_id < 32 {
            crate::syscall::CPU_SCRATCHES[apic_id].signals_pending =
                if pending_unblocked != 0 { 1 } else { 0 };
        }
    }
    0
}

/// Linux x86_64 `sigcontext` / `mcontext_t` structure.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SigContext {
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rsp: u64,
    pub rip: u64,
    pub eflags: u64,
    pub cs: u16,
    pub gs: u16,
    pub fs: u16,
    pub __pad0: u16,
    pub err: u64,
    pub trapno: u64,
    pub oldmask: u64,
    pub cr2: u64,
    pub fpstate: u64,
    pub reserved1: [u64; 8],
}

/// Linux x86_64 `ucontext_t` structure.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UContext {
    pub uc_flags: u64,
    pub uc_link: u64,
    pub uc_stack: crate::process::task::StackT,
    pub uc_mcontext: SigContext,
    pub uc_sigmask: u64,
    pub __fpregs_mem: [u8; 512],
}

impl Default for UContext {
    fn default() -> Self {
        Self {
            uc_flags: 0,
            uc_link: 0,
            uc_stack: crate::process::task::StackT::default(),
            uc_mcontext: SigContext::default(),
            uc_sigmask: 0,
            __fpregs_mem: [0u8; 512],
        }
    }
}

/// Linux x86_64 `rt_sigframe` stack structure.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RtSigFrame {
    pub pretcode: u64,
    pub info: crate::syscall::process::lifecycle::SigInfo,
    pub uc: UContext,
}

fn terminate_group_and_exit(current_pid: crate::process::pid::Pid, exit_code: i32) -> ! {
    use crate::process::scheduler;
    let tgid = if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
        task_arc.lock().tgid
    } else {
        scheduler::exit_current_thread(exit_code);
    };

    let mut other_pids = alloc::vec::Vec::new();
    let current_pid_val = current_pid.as_u64();
    let tgid_val = tgid.as_u64();
    for (p, atom) in scheduler::TASK_TGIDS.iter().enumerate() {
        if p as u64 != current_pid_val
            && atom.load(core::sync::atomic::Ordering::Acquire) == tgid_val
        {
            other_pids.push(crate::process::pid::Pid::from_raw(p as u64));
        }
    }

    if !other_pids.is_empty() {
        let fds = x86_64::instructions::interrupts::without_interrupts(|| {
            let mut collected = alloc::vec::Vec::new();
            if let Some(ref mut sched) = *scheduler::SCHEDULER.lock() {
                for pid in other_pids {
                    collected.push(sched.exit_task(pid, exit_code));
                }
            }
            collected
        });
        drop(fds);
    }

    scheduler::exit_current_thread(exit_code);
}

/// Delivers pending unblocked signals to the current process.
pub fn handle_pending_signals(regs: *mut super::SavedRegisters) {
    if regs.is_null() {
        return;
    }

    use crate::process::scheduler;

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return,
    };

    let (sig, action, old_mask, sigaltstack) = {
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

        let action = task.sigactions.lock()[(active_sig - 1) as usize];
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

        (active_sig, action, old_mask, task.sigaltstack)
    };

    if action.sa_handler == 1 {
        // SIG_IGN
        return;
    } else if action.sa_handler == 0 {
        // SIG_DFL
        // In POSIX/Linux, PID 1 (both host init and container namespace init) is immune
        // to signals for which it has not installed an explicit signal handler.
        // Fatal default actions must not kill init.
        let is_init = if let Some(t_arc) = scheduler::get_task_arc(current_pid) {
            let t = t_arc.lock();
            t.pid.as_u64() == 1 || t.is_pid_ns_init
        } else {
            current_pid.as_u64() == 1
        };
        if is_init {
            return;
        }

        // SIGCHLD (17), SIGCONT (18), SIGTSTP (20), SIGTTIN (21), SIGTTOU (22), SIGURG (23), SIGWINCH (28)
        if sig == 17 || sig == 18 || sig == 20 || sig == 21 || sig == 22 || sig == 23 || sig == 28 {
            return;
        }
        kprintln!(
            "[signal] Default action for signal {} is termination. Exiting task group.",
            sig
        );
        terminate_group_and_exit(current_pid, sig | 128);
    } else {
        let original_user_sp = unsafe { (*regs).rsp };
        let mut user_sp = original_user_sp;

        const SA_ONSTACK: u64 = 0x08000000;
        const SS_DISABLE: i32 = 2;
        if (action.sa_flags & SA_ONSTACK) != 0 {
            if let Some(ref alt) = sigaltstack {
                if (alt.ss_flags & SS_DISABLE) == 0 {
                    // Check if we are already executing on the alternate stack
                    if !(original_user_sp >= alt.ss_sp
                        && original_user_sp < alt.ss_sp + alt.ss_size)
                    {
                        user_sp = alt.ss_sp + alt.ss_size;
                    }
                }
            }
        }

        // In System V AMD64 ABI, function entry requires (rsp + 8) % 16 == 0.
        // Signal frames place `pretcode` at rsp, simulating a function call where the return address
        // has been pushed. Linux aligns this via: ((sp - frame_size) & !0xF) - 8.
        let frame_size = core::mem::size_of::<RtSigFrame>() as u64;
        let new_user_sp = (user_sp.saturating_sub(frame_size) & !0xF).saturating_sub(8);

        if crate::syscall::validation::validate_user_ptr_write(
            new_user_sp as *mut u8,
            core::mem::size_of::<RtSigFrame>(),
        )
        .is_err()
        {
            kprintln!("[signal] Invalid user stack for signal delivery. Exiting task group.");
            terminate_group_and_exit(current_pid, 11 | 128); // SIGSEGV
        }

        let frame = RtSigFrame {
            pretcode: action.sa_restorer,
            info: crate::syscall::process::lifecycle::SigInfo {
                si_signo: sig as i32,
                si_errno: 0,
                si_code: 0,
                si_pid: current_pid.as_u64() as i32,
                si_uid: 0,
                si_status: 0,
                _pad: [0u8; 104],
            },
            uc: UContext {
                uc_flags: 0,
                uc_link: 0,
                uc_stack: sigaltstack.unwrap_or(crate::process::task::StackT {
                    ss_sp: 0,
                    ss_flags: 2, // SS_DISABLE
                    _pad: 0,
                    ss_size: 0,
                }),
                uc_mcontext: SigContext {
                    r8: unsafe { (*regs).r8 },
                    r9: unsafe { (*regs).r9 },
                    r10: unsafe { (*regs).r10 },
                    r11: unsafe { (*regs).rflags },
                    r12: unsafe { (*regs).r12 },
                    r13: unsafe { (*regs).r13 },
                    r14: unsafe { (*regs).r14 },
                    r15: unsafe { (*regs).r15 },
                    rdi: unsafe { (*regs).rdi },
                    rsi: unsafe { (*regs).rsi },
                    rbp: unsafe { (*regs).rbp },
                    rbx: unsafe { (*regs).rbx },
                    rdx: unsafe { (*regs).rdx },
                    rax: unsafe { (*regs).rax },
                    rcx: unsafe { (*regs).rip },
                    rsp: original_user_sp,
                    rip: unsafe { (*regs).rip },
                    eflags: unsafe { (*regs).rflags },
                    cs: (crate::arch::x86_64::gdt::user_code_selector().0 | 3) as u16,
                    gs: 0,
                    fs: 0,
                    __pad0: 0,
                    err: 0,
                    trapno: 0,
                    oldmask: old_mask,
                    cr2: 0,
                    fpstate: 0,
                    reserved1: [0u64; 8],
                },
                uc_sigmask: old_mask,
                __fpregs_mem: [0u8; 512],
            },
        };

        let info_ptr = new_user_sp + 8; // pretcode is 8 bytes
        let uc_ptr = new_user_sp
            + 8
            + core::mem::size_of::<crate::syscall::process::lifecycle::SigInfo>() as u64;

        // SAFETY: Probe the user stack pages to verify write accessibility before writing the full frame.
        unsafe {
            core::ptr::write_volatile(new_user_sp as *mut u8, 0);
            core::ptr::write_volatile((new_user_sp + frame_size - 1) as *mut u8, 0);
        }

        // SAFETY: The stack pointer new_user_sp was validated with validate_user_ptr_write and probed.
        // regs points to the valid SavedRegisters structure on the current task's kernel stack.
        unsafe {
            core::ptr::write(new_user_sp as *mut RtSigFrame, frame);
            (*regs).rsp = new_user_sp;
            (*regs).rip = action.sa_handler;
            (*regs).rdi = sig as u64;
            (*regs).rsi = info_ptr;
            (*regs).rdx = uc_ptr;
        }

        if DEBUG_SIGNALS {
            kprintln!(
                "[signal] Delivered signal {} to custom handler at {:#x}, info={:#x}, ucontext={:#x}, stack={:#x}",
                sig,
                action.sa_handler,
                info_ptr,
                uc_ptr,
                new_user_sp
            );
        }
    }
}

/// `sys_rt_sigreturn` — Return from signal handler.
pub fn sys_rt_sigreturn(regs: *mut super::SavedRegisters) -> SyscallResult {
    if regs.is_null() {
        return Errno::EFAULT.into();
    }

    let user_sp = unsafe { (*regs).rsp };
    let frame_ptr = (user_sp - 8) as *const RtSigFrame;

    if !crate::syscall::fs::validate_user_ptr(
        frame_ptr as *const u8,
        core::mem::size_of::<RtSigFrame>(),
    ) {
        return Errno::EFAULT.into();
    }

    // SAFETY: frame_ptr was verified with validate_user_ptr.
    let frame = unsafe { &*frame_ptr };
    let mctx = &frame.uc.uc_mcontext;

    // Enforce canonical user-space address checks to prevent Ring 0 #GP or kernel space execution
    if mctx.rip >= 0x0000_8000_0000_0000 || mctx.rsp >= 0x0000_8000_0000_0000 {
        return Errno::EFAULT.into();
    }

    // SAFETY: regs points to the valid SavedRegisters structure on the current task's kernel stack.
    unsafe {
        (*regs).rflags = (mctx.eflags & !0x3000) | 0x202; // Strip IOPL, enable interrupts
        (*regs).rip = mctx.rip;
        (*regs).rax = mctx.rax;
        (*regs).rbx = mctx.rbx;
        (*regs).rbp = mctx.rbp;
        (*regs).rdi = mctx.rdi;
        (*regs).rsi = mctx.rsi;
        (*regs).rdx = mctx.rdx;
        (*regs).r8 = mctx.r8;
        (*regs).r9 = mctx.r9;
        (*regs).r10 = mctx.r10;
        (*regs).r12 = mctx.r12;
        (*regs).r13 = mctx.r13;
        (*regs).r14 = mctx.r14;
        (*regs).r15 = mctx.r15;

        (*regs).rsp = mctx.rsp;

        // Restore saved signal mask
        use crate::process::scheduler;
        if let Some(current_pid) = scheduler::current_pid() {
            if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
                let mut task = task_arc.lock();
                task.blocked_signals = frame.uc.uc_sigmask;
                // SIGKILL (9) and SIGSTOP (19) cannot be blocked
                task.blocked_signals &= !((1 << 8) | (1 << 18));

                let pending_unblocked = task.pending_signals & !task.blocked_signals;
                let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
                if apic_id < 32 {
                    crate::syscall::CPU_SCRATCHES[apic_id].signals_pending =
                        if pending_unblocked != 0 { 1 } else { 0 };
                }
            }
        }

        if DEBUG_SIGNALS {
            kprintln!(
                "[signal] sys_rt_sigreturn: restored execution context to RIP={:#x}, RSP={:#x}",
                mctx.rip,
                mctx.rsp
            );
        }

        (*regs).rax as SyscallResult
    }
}

/// `rt_sigsuspend(unewset, sigsetsize)` — Temporarily replace signal mask and suspend process.
pub fn sys_rt_sigsuspend(unewset: *const u64, sigsetsize: usize) -> SyscallResult {
    if sigsetsize != 8 || unewset.is_null() {
        return Errno::EINVAL.into();
    }
    if !crate::syscall::validation::validate_user_ptr(unewset as *const u8, 8) {
        return Errno::EFAULT.into();
    }
    let new_mask = unsafe { core::ptr::read(unewset) };
    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        let mut task = task_arc.lock();
        task.blocked_signals = new_mask & !((1 << 8) | (1 << 18));
    }
    crate::process::scheduler::yield_now();
    Errno::EINTR.into()
}

/// `rt_sigpending(uset, sigsetsize)` — Examine pending signals.
pub fn sys_rt_sigpending(uset: *mut u64, sigsetsize: usize) -> SyscallResult {
    if sigsetsize != 8 || uset.is_null() {
        return Errno::EINVAL.into();
    }
    if crate::syscall::validation::validate_user_ptr_write(uset as *mut u8, 8).is_err() {
        return Errno::EFAULT.into();
    }
    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let pending = if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        let task = task_arc.lock();
        task.pending_signals & task.blocked_signals
    } else {
        0
    };
    unsafe {
        core::ptr::write(uset, pending);
    }
    0
}

/// `rt_sigtimedwait(uthese, uinfo, uts, sigsetsize)` — Synchronously wait for queued signals.
pub fn sys_rt_sigtimedwait(
    uthese: *const u64,
    _uinfo: *mut u8,
    _uts: *const u8,
    sigsetsize: usize,
) -> SyscallResult {
    if sigsetsize != 8 || uthese.is_null() {
        return Errno::EINVAL.into();
    }
    if !crate::syscall::validation::validate_user_ptr(uthese as *const u8, 8) {
        return Errno::EFAULT.into();
    }
    let these = unsafe { core::ptr::read(uthese) };
    let current_pid = match crate::process::scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
        let mut task = task_arc.lock();
        let ready = task.pending_signals & these;
        if ready != 0 {
            let sig = ready.trailing_zeros() + 1;
            task.pending_signals &= !(1 << (sig - 1));
            return sig as SyscallResult;
        }
    }
    Errno::EAGAIN.into()
}

/// `rt_sigqueueinfo(pid, sig, uinfo)` — Queue a signal and data.
pub fn sys_rt_sigqueueinfo(pid: i32, sig: i32, _uinfo: *const u8) -> SyscallResult {
    sys_kill(pid, sig)
}

/// `rt_tgsigqueueinfo(tgid, tid, sig, uinfo)` — Queue a signal and data to a thread.
pub fn sys_rt_tgsigqueueinfo(_tgid: i32, tid: i32, sig: i32, _uinfo: *const u8) -> SyscallResult {
    sys_kill(tid, sig)
}

/// `pause()` — Wait for signal.
pub fn sys_pause() -> SyscallResult {
    crate::process::scheduler::yield_now();
    Errno::EINTR.into()
}
