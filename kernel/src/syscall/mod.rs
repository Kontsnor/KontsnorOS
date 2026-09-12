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

//! POSIX-compatible syscall interface for KontsnorOS.
//!
//! This module implements the system call layer that provides Unix
//! compatibility. User-space programs invoke syscalls via the `syscall`
//! instruction on x86_64, which transfers control to the kernel.

use crate::kprintln;
pub mod fs;
pub mod io;
pub mod ipc;
pub mod memory;
pub mod net;
pub mod process;
pub mod signal;
pub mod validation;

pub const DEBUG_SYSCALLS: bool = false;

/// Syscall numbers for KontsnorOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum SyscallNumber {
    Read = 0,
    Write = 1,
    Open = 2,
    Close = 3,
    Stat = 4,
    Fstat = 5,
    Lseek = 8,
    Mmap = 9,
    Mprotect = 10,
    Munmap = 11,
    Brk = 12,
    Mremap = 25,
    Getpid = 39,
    Fork = 57,
    Execve = 59,
    Exit = 60,
    Wait4 = 61,
    Kill = 62,
    Ioctl = 16,
    Pipe = 22,
    Dup = 32,
    Dup2 = 33,
    Fsync = 74,
    Getcwd = 79,
    Chdir = 80,
    Mkdir = 83,
    Rmdir = 84,
    Unlink = 87,
    Umask = 95,
    Getuid = 102,
    Getgid = 104,
    Setuid = 105,
    Setgid = 106,
    EpollWait = 232,
    EpollCtl = 233,
    TimerFdCreate = 283,
    TimerFdSetTime = 286,
    SignalFd4 = 289,
    EventFd2 = 290,
    EpollCreate1 = 291,
    SchedGetAffinity = 204,
}

/// Result type for syscalls.
pub type SyscallResult = i64;

/// Standard POSIX errno values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum Errno {
    EPERM = -1,
    ENOENT = -2,
    ESRCH = -3,
    EINTR = -4,
    EIO = -5,
    ENXIO = -6,
    E2BIG = -7,
    EBADF = -9,
    ECHILD = -10,
    EAGAIN = -11,
    ENOMEM = -12,
    EACCES = -13,
    EFAULT = -14,
    EEXIST = -17,
    EXDEV = -18,
    ENOTDIR = -20,
    EISDIR = -21,
    EINVAL = -22,
    EMFILE = -24,
    EFBIG = -27,
    ENOSPC = -28,
    EROFS = -30,
    ENOSYS = -38,
    ENOTEMPTY = -39,
    ENOEXEC = -8,
    ELOOP = -40,
    ENOTSOCK = -88,
    EDESTADDRREQ = -89,
    ENETUNREACH = -101,
    EISCONN = -106,
    ENOTCONN = -107,
    ECONNREFUSED = -111,
    ETIMEDOUT = -110,
    EBUSY = -16,
    ENODEV = -19,
    ENOMSG = -42,
    EOPNOTSUPP = -95,
    ENOKEY = -126,
}

impl From<Errno> for SyscallResult {
    fn from(e: Errno) -> Self {
        e as i64
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SavedRegisters {
    pub rax: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub r10: u64,
    pub r8: u64,
    pub r9: u64,
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rip: u64,    // rcx
    pub rflags: u64, // r11
    pub rsp: u64,    // User stack pointer, pushed first!
}

// ── Fast Syscall Assembly Entry Point ────────────────────────────────

core::arch::global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "swapgs",          // Swap GS with Kernel GS
    "mov gs:[0], rsp", // Save user RSP in CpuScratch.user_rsp
    "mov rsp, gs:[8]", // Load kernel stack pointer from CpuScratch.kernel_rsp
    // 1. Check if there are pending signals for the current task
    "cmp qword ptr gs:[24], 0", // gs:[24] is CpuScratch.signals_pending
    "jne .Lsyscall_slow",       // If signals are pending, go slow path
    // 2. Check if syscall is a fast-path candidate
    "cmp rax, 39", // sys_getpid
    "je .Lsyscall_fast",
    "cmp rax, 102", // sys_getuid
    "je .Lsyscall_fast",
    "cmp rax, 104", // sys_getgid
    "je .Lsyscall_fast",
    "cmp rax, 107", // sys_geteuid
    "je .Lsyscall_fast",
    "cmp rax, 108", // sys_getegid
    "je .Lsyscall_fast",
    "cmp rax, 110", // sys_getppid
    "je .Lsyscall_fast",
    "cmp rax, 186", // sys_gettid
    "je .Lsyscall_fast",
    "cmp rax, 218", // sys_set_tid_address
    "je .Lsyscall_fast",
    // 3. Fall through to slow path
    ".Lsyscall_slow:",
    // Push registers in reverse order of SavedRegisters struct
    "push qword ptr gs:[0]", // User RSP (rsp)
    "push r11",              // User RFLAGS (rflags)
    "push rcx",              // User RIP (rip)
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "push r9",
    "push r8",
    "push r10", // Fast syscall uses r10 for arg3 (since rcx is overwritten)
    "push rdx",
    "push rsi",
    "push rdi",
    "push rax", // Push rax so it is in the struct and we can modify/read it
    // Pass pointer to saved registers (rsp) as 1st argument (rdi)
    "mov rdi, rsp",
    // Pass syscall number (rax) as 2nd argument (rsi)
    "mov rsi, rax",
    "call syscall_dispatch_rust",
    // Pop rax (return value, which might be modified by the syscall!)
    "pop rax",
    // Restore general purpose registers
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop r10",
    "pop r8",
    "pop r9",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "pop rcx",              // User RIP
    "pop r11",              // User RFLAGS
    "pop qword ptr gs:[0]", // User RSP
    // Restore user stack pointer and swapgs back
    "mov rsp, gs:[0]",
    "swapgs",
    "sysretq",
    // 4. Fast path implementation
    ".Lsyscall_fast:",
    // Push caller-saved registers to preserve them
    "push r11", // User RFLAGS
    "push rcx", // User RIP
    "push r9",
    "push r8",
    "push r10",
    "push rdx",
    "push rsi",
    "push rdi",
    // Map System V ABI registers for Rust:
    // rdi = syscall_num (rax)
    // rsi = arg0        (rdi)
    // rdx = arg1        (rsi)
    // rcx = arg2        (rdx)
    // r8  = arg3        (r10)
    // r9  = arg4        (r8)
    "mov r9, r8",
    "mov r8, r10",
    "mov rcx, rdx",
    "mov rdx, rsi",
    "mov rsi, rdi",
    "mov rdi, rax",
    "call syscall_fast_dispatch",
    // Restore preserved caller-saved registers
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop r10",
    "pop r8",
    "pop r9",
    "pop rcx",
    "pop r11",
    // Restore user stack pointer and swapgs back
    "mov rsp, gs:[0]",
    "swapgs",
    "sysretq",
);

/// CPU-local scratch space for syscall privilege transitions.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct CpuScratch {
    pub user_rsp: u64,
    pub kernel_rsp: u64,
    pub current_pid: u64,
    pub signals_pending: u64,
}

/// Static mutable CPU scratch spaces.
#[no_mangle]
pub static mut CPU_SCRATCHES: [CpuScratch; 32] = [CpuScratch {
    user_rsp: 0,
    kernel_rsp: 0,
    current_pid: 0xFFFF_FFFF_FFFF_FFFF,
    signals_pending: 0,
}; 32];

/// Set the temporary kernel stack pointer for syscall entry.
pub fn set_kernel_stack(stack: u64) {
    unsafe {
        let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
        if apic_id >= 32 {
            panic!(
                "APIC ID {} out of bounds (>= 32) in set_kernel_stack",
                apic_id
            );
        }
        CPU_SCRATCHES[apic_id].kernel_rsp = stack;
    }
}

/// Dispatcher assembly calling wrapper.
#[no_mangle]
pub extern "C" fn syscall_dispatch_rust(regs: *mut SavedRegisters, syscall_num: u64) -> i64 {
    if syscall_num == 15 {
        return crate::syscall::signal::sys_rt_sigreturn(regs);
    }

    // Enable interrupts for the duration of the system call
    x86_64::instructions::interrupts::enable();

    let arg0 = unsafe { (*regs).rdi };
    let arg1 = unsafe { (*regs).rsi };
    let arg2 = unsafe { (*regs).rdx };
    let arg3 = unsafe { (*regs).r10 };
    let arg4 = unsafe { (*regs).r8 };
    let arg5 = unsafe { (*regs).r9 };

    if DEBUG_SYSCALLS {
        // Debug print FS_BASE and canary
        let fs_base = x86_64::registers::model_specific::FsBase::read().as_u64();
        let user_rsp = unsafe { (*regs).rsp };
        let mut canary_msg = alloc::string::String::new();
        if fs_base != 0 {
            let canary_addr = fs_base + 0x28;
            if let Some(phys) =
                crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(canary_addr))
            {
                let virt = phys.as_u64() + crate::memory::r#virtual::phys_mem_offset();
                let canary_val = unsafe { *(virt as *const u64) };
                canary_msg = alloc::format!("canary={:#x}", canary_val);
            } else {
                canary_msg = alloc::format!("canary_addr={:#x} (unmapped)", canary_addr);
            }
        }

        // Print stack values
        let mut stack_msg = alloc::string::String::new();
        if user_rsp != 0 && user_rsp < 0x0000_7FFF_FFFF_FFFF {
            if let Some(_phys) =
                crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(user_rsp))
            {
                // print 4 words from RSP
                let mut words = [0u64; 4];
                for i in 0..4 {
                    let addr = user_rsp + (i * 8);
                    if let Some(p) =
                        crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(addr))
                    {
                        let v = p.as_u64() + crate::memory::r#virtual::phys_mem_offset();
                        words[i as usize] = unsafe { *(v as *const u64) };
                    }
                }
                stack_msg = alloc::format!(
                    "rsp={:#x} stack=[{:#x}, {:#x}, {:#x}, {:#x}]",
                    user_rsp,
                    words[0],
                    words[1],
                    words[2],
                    words[3]
                );
            }
        }

        crate::kprintln!(
            "[debug syscall {}] args=[{:#x}, {:#x}, {:#x}] fs_base={:#x} {} {}",
            syscall_num,
            arg0,
            arg1,
            arg2,
            fs_base,
            canary_msg,
            stack_msg
        );
    }

    let current_pid_val = crate::process::scheduler::current_pid()
        .map(|p| p.as_u64())
        .unwrap_or(0);
    let _ = current_pid_val;

    let res = dispatch(regs, syscall_num, arg0, arg1, arg2, arg3, arg4, arg5);

    if DEBUG_SYSCALLS {
        crate::kprintln!("[debug syscall {} ret] res={}", syscall_num, res);
        if syscall_num == 16 {
            let user_rsp = unsafe { (*regs).rsp };
            let mut words_after = [0u64; 8];
            for i in 0..8 {
                let addr = user_rsp + (i * 8);
                if let Some(p) =
                    crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(addr))
                {
                    let v = p.as_u64() + crate::memory::r#virtual::phys_mem_offset();
                    words_after[i as usize] = unsafe { *(v as *const u64) };
                }
            }
            crate::kprintln!("[debug syscall 16 ret] stack_after=[{:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}]",
                words_after[0], words_after[1], words_after[2], words_after[3],
                words_after[4], words_after[5], words_after[6], words_after[7]);
        }
    }

    unsafe {
        (*regs).rax = res as u64;
    }

    // Call signal delivery handler before returning to user space
    crate::syscall::signal::handle_pending_signals(regs);

    // Disable interrupts before returning to assembly (which will exit to user space)
    x86_64::instructions::interrupts::disable();

    unsafe { (*regs).rax as i64 }
}

/// Fast-path syscall dispatcher.
///
/// Dispatches simple, non-yielding system calls directly from the fast assembly stub.
#[no_mangle]
pub extern "C" fn syscall_fast_dispatch(
    syscall_num: u64,
    arg0: u64,
    _arg1: u64,
    _arg2: u64,
    _arg3: u64,
    _arg4: u64,
) -> i64 {
    match syscall_num {
        39 => process::sys_getpid(),
        102 => process::sys_getuid(),
        104 => process::sys_getgid(),
        107 => process::sys_geteuid(),
        108 => process::sys_getegid(),
        110 => process::sys_getppid(),
        111 => process::sys_getpgrp(),
        186 => process::sys_gettid(),
        218 => process::sys_set_tid_address(arg0 as *mut i32),
        _ => -38, // ENOSYS
    }
}

/// Initialize the syscall interface.
///
/// Sets up STAR, LSTAR, SFMASK MSRs (Model-Specific Registers)
/// so that user-space programs can invoke fast system calls.
pub fn init() {
    use x86_64::registers::model_specific::Msr;

    let mut efer_msr = Msr::new(0xC0000080);
    let mut star_msr = Msr::new(0xC0000081);
    let mut lstar_msr = Msr::new(0xC0000082);
    let mut fmask_msr = Msr::new(0xC0000084);

    unsafe {
        // 1. Enable System Call Extensions (SCE) in EFER
        let efer = efer_msr.read();
        efer_msr.write(efer | 1);

        // 2. Set STAR segment selectors
        // For SYSRET in 64-bit mode: SS is loaded from STAR[63:48] + 8, CS from STAR[63:48] + 16.
        // Since user_data is immediately followed by user_code in our GDT, setting STAR[63:48]
        // to (user_data - 8) | 3 (which points to kernel_data but with user RPL 3) causes
        // SYSRET to load user_data into SS (base + 8) and user_code into CS (base + 16).
        let kernel_code = crate::arch::x86_64::gdt::kernel_code_selector().0;
        let user_data = crate::arch::x86_64::gdt::user_data_selector().0;
        let star = ((kernel_code as u64) << 32) | ((((user_data - 8) | 3) as u64) << 48);
        star_msr.write(star);

        // 3. Set LSTAR fast-syscall entry point (RIP)
        extern "C" {
            fn syscall_entry();
        }
        lstar_msr.write(syscall_entry as *const () as u64);

        // 4. Set FMASK flags to clear (clear Interrupt Flag IF, Direction Flag DF)
        fmask_msr.write(0x200 | 0x400);

        // 5. Configure IA32_GS_BASE (active GS base in kernel) to point to CPU_SCRATCHES slot for this core
        let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
        if apic_id >= 32 {
            panic!("APIC ID {} out of bounds (>= 32) in syscall::init", apic_id);
        }
        let scratch_addr = core::ptr::addr_of!(CPU_SCRATCHES[apic_id]) as u64;
        let mut gs_base_msr = Msr::new(0xC0000101);
        gs_base_msr.write(scratch_addr);

        // 6. Configure IA32_KERNEL_GS_BASE MSR to 0 (swapped GS base, initially 0 for user space)
        let mut kernel_gs_msr = Msr::new(0xC0000102);
        kernel_gs_msr.write(0);

        crate::process::scheduler::GS_BASE_ACTIVE.store(true, core::sync::atomic::Ordering::SeqCst);
    }

    kprintln!("[syscall] Syscall MSR registers configured. Syscall interface ready.");
}

/// Dispatch a syscall based on its number.
pub fn dispatch(
    regs: *mut SavedRegisters,
    syscall_num: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    _arg5: u64,
) -> SyscallResult {
    match syscall_num {
        // File I/O
        0 => fs::sys_read(arg0 as i32, arg1 as *mut u8, arg2 as usize),
        1 => fs::sys_write(arg0 as i32, arg1 as *const u8, arg2 as usize),
        2 => fs::sys_open(arg0 as *const u8, arg1 as i32, arg2 as u32),
        3 => fs::sys_close(arg0 as i32),
        4 => fs::sys_stat(arg0 as *const u8, arg1 as *mut fs::LinuxStat),
        5 => fs::sys_fstat(arg0 as i32, arg1 as *mut fs::LinuxStat),
        6 => fs::sys_lstat(arg0 as *const u8, arg1 as *mut fs::LinuxStat),
        7 => fs::sys_poll(arg0 as *mut u8, arg1, arg2 as i32),
        8 => fs::sys_lseek(arg0 as i32, arg1 as i64, arg2 as i32),
        16 => io::sys_ioctl(arg0 as i32, arg1, arg2),
        17 => fs::sys_pread64(arg0 as i32, arg1 as *mut u8, arg2 as usize, arg3 as i64),
        18 => fs::sys_pwrite64(arg0 as i32, arg1 as *const u8, arg2 as usize, arg3 as i64),
        19 => fs::sys_readv(arg0 as i32, arg1 as *const fs::IoVec, arg2 as i32),
        20 => fs::sys_writev(arg0 as i32, arg1 as *const fs::IoVec, arg2 as i32),
        21 => fs::sys_access(arg0 as *const u8, arg1 as i32),
        22 => fs::sys_pipe(arg0 as *mut i32),
        32 => fs::sys_dup(arg0 as i32),
        33 => fs::sys_dup2(arg0 as i32, arg1 as i32),
        72 => fs::sys_fcntl(arg0 as i32, arg1 as i32, arg2),
        73 => fs::sys_flock(arg0 as i32, arg1 as i32),
        74 => fs::sys_fsync(arg0 as i32),
        75 => fs::sys_fdatasync(arg0 as i32),
        162 => fs::sys_sync(),
        76 => fs::sys_truncate(arg0 as *const u8, arg1 as i64),
        77 => fs::sys_ftruncate(arg0 as i32, arg1 as i64),
        79 => fs::sys_getcwd(arg0 as *mut u8, arg1 as usize),
        80 => fs::sys_chdir(arg0 as *const u8),
        81 => fs::sys_fchdir(arg0 as i32),
        82 => fs::sys_rename(arg0 as *const u8, arg1 as *const u8),
        83 => fs::sys_mkdir(arg0 as *const u8, arg1 as u32),
        84 => fs::sys_rmdir(arg0 as *const u8),
        85 => fs::sys_creat(arg0 as *const u8, arg1 as u32),
        86 => fs::sys_link(arg0 as *const u8, arg1 as *const u8),
        87 => fs::sys_unlink(arg0 as *const u8),
        88 => fs::sys_symlink(arg0 as *const u8, arg1 as *const u8),
        89 => fs::sys_readlink(arg0 as *const u8, arg1 as *mut u8, arg2 as usize),
        90 => fs::sys_chmod(arg0 as *const u8, arg1 as u32),
        91 => fs::sys_fchmod(arg0 as i32, arg1 as u32),
        92 => fs::sys_chown(arg0 as *const u8, arg1 as u32, arg2 as u32),
        93 => fs::sys_fchown(arg0 as i32, arg1 as u32, arg2 as u32),
        94 => fs::sys_lchown(arg0 as *const u8, arg1 as u32, arg2 as u32),
        95 => fs::sys_umask(arg0 as u32),
        187 => fs::sys_readahead(arg0 as i32, arg1 as i64, arg2 as usize),
        188 => fs::sys_setxattr(
            arg0 as *const u8,
            arg1 as *const u8,
            arg2 as *const u8,
            arg3 as usize,
            arg4 as i32,
        ),
        189 => fs::sys_lsetxattr(
            arg0 as *const u8,
            arg1 as *const u8,
            arg2 as *const u8,
            arg3 as usize,
            arg4 as i32,
        ),
        190 => fs::sys_fsetxattr(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as *const u8,
            arg3 as usize,
            arg4 as i32,
        ),
        191 => fs::sys_getxattr(
            arg0 as *const u8,
            arg1 as *const u8,
            arg2 as *mut u8,
            arg3 as usize,
        ),
        192 => fs::sys_lgetxattr(
            arg0 as *const u8,
            arg1 as *const u8,
            arg2 as *mut u8,
            arg3 as usize,
        ),
        193 => fs::sys_fgetxattr(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as *mut u8,
            arg3 as usize,
        ),
        194 => fs::sys_listxattr(arg0 as *const u8, arg1 as *mut u8, arg2 as usize),
        195 => fs::sys_llistxattr(arg0 as *const u8, arg1 as *mut u8, arg2 as usize),
        196 => fs::sys_flistxattr(arg0 as i32, arg1 as *mut u8, arg2 as usize),
        197 => fs::sys_removexattr(arg0 as *const u8, arg1 as *const u8),
        198 => fs::sys_lremovexattr(arg0 as *const u8, arg1 as *const u8),
        199 => fs::sys_fremovexattr(arg0 as i32, arg1 as *const u8),
        213 => fs::sys_epoll_create(arg0 as i32),
        217 => fs::sys_getdents64(arg0 as i32, arg1 as *mut u8, arg2 as usize),
        257 => fs::sys_openat(arg0 as i32, arg1 as *const u8, arg2 as i32, arg3 as u32),
        258 => fs::sys_mkdirat(arg0 as i32, arg1 as *const u8, arg2 as u32),
        260 => fs::sys_fchownat(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as u32,
            arg3 as u32,
            arg4 as i32,
        ),
        262 => fs::sys_newfstatat(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as *mut fs::LinuxStat,
            arg3 as i32,
        ),
        263 => fs::sys_unlinkat(arg0 as i32, arg1 as *const u8, arg2 as i32),
        264 => fs::sys_renameat(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as i32,
            arg3 as *const u8,
        ),
        265 => fs::sys_linkat(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as i32,
            arg3 as *const u8,
            arg4 as i32,
        ),
        266 => fs::sys_symlinkat(arg0 as *const u8, arg1 as i32, arg2 as *const u8),
        267 => fs::sys_readlinkat(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as *mut u8,
            arg3 as usize,
        ),
        268 => fs::sys_fchmodat(arg0 as i32, arg1 as *const u8, arg2 as u32, 0),
        269 => fs::sys_faccessat(arg0 as i32, arg1 as *const u8, arg2 as i32, arg3 as i32),
        271 => fs::sys_ppoll(
            arg0 as *mut u8,
            arg1,
            arg2 as *const fs::TimeSpec,
            arg3 as *const u8,
            arg4 as usize,
        ),
        275 => fs::sys_splice(
            arg0 as i32,
            arg1 as *mut i64,
            arg2 as i32,
            arg3 as *mut i64,
            arg4 as usize,
            _arg5 as u32,
        ),
        276 => fs::sys_tee(arg0 as i32, arg1 as i32, arg2 as usize, arg3 as u32),
        277 => fs::sys_sync_file_range(arg0 as i32, arg1 as i64, arg2 as i64, arg3 as u32),
        278 => fs::sys_vmsplice(
            arg0 as i32,
            arg1 as *const fs::IoVec,
            arg2 as usize,
            arg3 as u32,
        ),
        281 => fs::sys_epoll_pwait(
            arg0 as i32,
            arg1 as *mut crate::fs::epoll::EpollEvent,
            arg2 as i32,
            arg3 as i32,
            arg4 as *const u8,
            _arg5 as usize,
        ),
        232 => fs::sys_epoll_wait(
            arg0 as i32,
            arg1 as *mut crate::fs::epoll::EpollEvent,
            arg2 as i32,
            arg3 as i32,
        ),
        233 => fs::sys_epoll_ctl(
            arg0 as i32,
            arg1 as i32,
            arg2 as i32,
            arg3 as *const crate::fs::epoll::EpollEvent,
        ),
        283 => fs::sys_timerfd_create(arg0 as i32, arg1 as i32),
        285 => fs::sys_fallocate(arg0 as i32, arg1 as i32, arg2 as i64, arg3 as i64),
        286 => fs::sys_timerfd_settime(
            arg0 as i32,
            arg1 as i32,
            arg2 as *const crate::fs::timerfd::Itimerspec,
            arg3 as *mut crate::fs::timerfd::Itimerspec,
        ),
        287 => fs::sys_timerfd_settime(
            arg0 as i32,
            0,
            arg1 as *const crate::fs::timerfd::Itimerspec,
            arg2 as *mut crate::fs::timerfd::Itimerspec,
        ),
        289 => fs::sys_signalfd4(arg0 as i32, arg1 as *const u64, arg2 as usize, arg3 as i32),
        290 => fs::sys_eventfd2(arg0 as u32, arg1 as i32),
        291 => fs::sys_epoll_create1(arg0 as i32),
        292 => fs::sys_dup3(arg0 as i32, arg1 as i32, arg2 as i32),
        295 => fs::sys_preadv(
            arg0 as i32,
            arg1 as *const fs::IoVec,
            arg2 as i32,
            arg3 as i64,
        ),
        296 => fs::sys_pwritev(
            arg0 as i32,
            arg1 as *const fs::IoVec,
            arg2 as i32,
            arg3 as i64,
        ),
        306 => fs::sys_syncfs(arg0 as i32),
        326 => fs::sys_copy_file_range(
            arg0 as i32,
            arg1 as *mut i64,
            arg2 as i32,
            arg3 as *mut i64,
            arg4 as usize,
            _arg5 as u32,
        ),
        327 => fs::sys_preadv2(
            arg0 as i32,
            arg1 as *const fs::IoVec,
            arg2 as i32,
            arg3 as i64,
            arg4 as i32,
        ),
        328 => fs::sys_pwritev2(
            arg0 as i32,
            arg1 as *const fs::IoVec,
            arg2 as i32,
            arg3 as i64,
            arg4 as i32,
        ),
        137 => fs::sys_statfs(arg0 as *const u8, arg1 as *mut fs::LinuxStatfs),
        138 => fs::sys_fstatfs(arg0 as i32, arg1 as *mut fs::LinuxStatfs),
        165 => fs::sys_mount(
            arg0 as *const u8,
            arg1 as *const u8,
            arg2 as *const u8,
            arg3 as u64,
            arg4 as *const u8,
        ),
        166 => fs::sys_umount2(arg0 as *const u8, arg1 as i32),
        132 => fs::sys_utime(arg0 as *const u8, arg1 as *const fs::UTimeBuf),
        235 => fs::sys_utimes(arg0 as *const u8, arg1 as *const fs::TimeVal),
        280 => fs::sys_utimensat(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as *const fs::TimeSpec,
            arg3 as i32,
        ),
        293 => fs::sys_pipe2(arg0 as *mut i32, arg1 as i32),
        319 => fs::sys_memfd_create(arg0 as *const u8, arg1 as u32),
        221 => 0, // fadvise64: advice accepted
        // Memory
        9 => memory::sys_mmap(
            arg0,
            arg1 as usize,
            arg2 as i32,
            arg3 as i32,
            arg4 as i32,
            _arg5 as i64,
        ),
        10 => memory::sys_mprotect(arg0, arg1 as usize, arg2 as i32),
        11 => memory::sys_munmap(arg0, arg1 as usize),
        12 => memory::sys_brk(arg0),
        25 => memory::sys_mremap(arg0, arg1 as usize, arg2 as usize, arg3 as i32, arg4),
        26 => memory::sys_msync(arg0, arg1 as usize, arg2 as i32),
        27 => memory::sys_mincore(arg0, arg1 as usize, arg2 as *mut u8),
        28 => memory::sys_madvise(arg0, arg1 as usize, arg2 as i32),
        149 => memory::sys_mlock(arg0, arg1 as usize),
        150 => memory::sys_munlock(arg0, arg1 as usize),
        151 => memory::sys_mlockall(arg0 as i32),
        152 => memory::sys_munlockall(),
        325 => memory::sys_mlock2(arg0, arg1 as usize, arg2 as i32),
        // System V IPC & POSIX MQ
        29 => ipc::sys_shmget(arg0 as i32, arg1 as usize, arg2 as i32),
        30 => ipc::sys_shmat(arg0 as i32, arg1 as *const u8, arg2 as i32),
        31 => ipc::sys_shmctl(arg0 as i32, arg1 as i32, arg2 as *mut ipc::ShmidDs),
        64 => ipc::sys_semget(arg0 as i32, arg1 as i32, arg2 as i32),
        65 => ipc::sys_semop(arg0 as i32, arg1 as *const ipc::SemBuf, arg2 as usize),
        66 => ipc::sys_semctl(arg0 as i32, arg1 as i32, arg2 as i32, arg3),
        67 => ipc::sys_shmdt(arg0 as *const u8),
        68 => ipc::sys_msgget(arg0 as i32, arg1 as i32),
        69 => ipc::sys_msgsnd(arg0 as i32, arg1 as *const u8, arg2 as usize, arg3 as i32),
        70 => ipc::sys_msgrcv(
            arg0 as i32,
            arg1 as *mut u8,
            arg2 as usize,
            arg3 as i64,
            arg4 as i32,
        ),
        71 => ipc::sys_msgctl(arg0 as i32, arg1 as i32, arg2 as *mut ipc::MsqidDs),
        220 => ipc::sys_semtimedop(
            arg0 as i32,
            arg1 as *const ipc::SemBuf,
            arg2 as usize,
            arg3 as *const u8,
        ),
        240 => ipc::sys_mq_open(
            arg0 as *const u8,
            arg1 as i32,
            arg2 as u32,
            arg3 as *const u8,
        ),
        241 => ipc::sys_mq_unlink(arg0 as *const u8),
        242 => ipc::sys_mq_timedsend(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as usize,
            arg3 as u32,
            arg4 as *const u8,
        ),
        243 => ipc::sys_mq_timedreceive(
            arg0 as i32,
            arg1 as *mut u8,
            arg2 as usize,
            arg3 as *mut u32,
            arg4 as *const u8,
        ),
        244 => ipc::sys_mq_notify(arg0 as i32, arg1 as *const u8),
        245 => ipc::sys_mq_getsetattr(arg0 as i32, arg1 as *const u8, arg2 as *mut u8),
        // Process & Signals
        13 => signal::sys_rt_sigaction(
            arg0 as i32,
            arg1 as *const crate::process::task::SigAction,
            arg2 as *mut crate::process::task::SigAction,
            arg3 as usize,
        ),
        14 => signal::sys_rt_sigprocmask(
            arg0 as i32,
            arg1 as *const u64,
            arg2 as *mut u64,
            arg3 as usize,
        ),
        23 => fs::sys_select(
            arg0 as i32,
            arg1 as *mut u64,
            arg2 as *mut u64,
            arg3 as *mut u64,
            arg4 as *const fs::TimeVal,
        ),
        34 => signal::sys_pause(),
        35 => process::sys_nanosleep(arg0 as *const u8, arg1 as *mut u8),
        36 => process::sys_getitimer(arg0 as i32, arg1 as *mut u8),
        37 => process::sys_alarm(arg0 as u32),
        38 => process::sys_setitimer(arg0 as i32, arg1 as *const u8, arg2 as *mut u8),
        39 => process::sys_getpid(),
        40 => fs::sys_sendfile(arg0 as i32, arg1 as i32, arg2 as *mut i64, arg3 as usize),
        56 => process::sys_clone(arg0, arg1, arg2 as *mut i32, arg3 as *mut i32, arg4, regs),
        57 => process::sys_fork(regs),
        58 => process::sys_vfork(regs),
        59 => process::sys_execve(
            arg0 as *const u8,
            arg1 as *const *const u8,
            arg2 as *const *const u8,
        ),
        60 => process::sys_exit(arg0 as i32),
        61 => process::sys_wait4(arg0 as i32, arg1 as *mut i32, arg2 as i32, arg3 as *mut u8),
        62 => signal::sys_kill(arg0 as i32, arg1 as i32),
        63 => process::sys_uname(arg0 as *mut u8),
        96 => process::sys_gettimeofday(arg0 as *mut u8, arg1 as *mut u8),
        97 => process::sys_getrlimit(arg0 as i32, arg1 as *mut u8),
        98 => process::sys_getrusage(arg0 as i32, arg1 as *mut u8),
        160 => process::sys_setrlimit(arg0 as i32, arg1 as *const u8),
        99 => process::sys_sysinfo(arg0 as *mut u8),
        100 => process::sys_times(arg0 as *mut u8),
        109 => process::sys_setpgid(arg0 as i32, arg1 as i32),
        110 => process::sys_getppid(),
        111 => process::sys_getpgrp(),
        112 => process::sys_setsid(),
        113 => process::sys_setreuid(arg0 as u32, arg1 as u32),
        114 => process::sys_setregid(arg0 as u32, arg1 as u32),
        115 => process::sys_getgroups(arg0 as i32, arg1 as *mut u32),
        116 => process::sys_setgroups(arg0 as usize, arg1 as *const u32),
        117 => process::sys_setresuid(arg0 as u32, arg1 as u32, arg2 as u32),
        118 => process::sys_getresuid(arg0 as *mut u32, arg1 as *mut u32, arg2 as *mut u32),
        119 => process::sys_setresgid(arg0 as u32, arg1 as u32, arg2 as u32),
        120 => process::sys_getresgid(arg0 as *mut u32, arg1 as *mut u32, arg2 as *mut u32),
        121 => process::sys_getpgid(arg0 as i32),
        122 => process::sys_setfsuid(arg0 as u32),
        123 => process::sys_setfsgid(arg0 as u32),
        124 => process::sys_getsid(arg0 as i32),
        125 => process::sys_capget(
            arg0 as *mut process::creds::CapUserHeader,
            arg1 as *mut process::creds::CapUserData,
        ),
        126 => process::sys_capset(
            arg0 as *const process::creds::CapUserHeader,
            arg1 as *const process::creds::CapUserData,
        ),
        127 => signal::sys_rt_sigpending(arg0 as *mut u64, arg1 as usize),
        128 => signal::sys_rt_sigtimedwait(
            arg0 as *const u64,
            arg1 as *mut u8,
            arg2 as *const u8,
            arg3 as usize,
        ),
        129 => signal::sys_rt_sigqueueinfo(arg0 as i32, arg1 as i32, arg2 as *const u8),
        130 => signal::sys_rt_sigsuspend(arg0 as *const u64, arg1 as usize),
        131 => process::sys_sigaltstack(arg0 as *const u8, arg1 as *mut u8, unsafe { (*regs).rsp }),
        135 => process::sys_personality(arg0),
        140 => process::sys_getpriority(arg0 as i32, arg1 as i32),
        141 => process::sys_setpriority(arg0 as i32, arg1 as i32, arg2 as i32),
        142 => process::sys_sched_setparam(arg0 as i32, arg1 as *const i32),
        143 => process::sys_sched_getparam(arg0 as i32, arg1 as *mut i32),
        144 => process::sys_sched_setscheduler(arg0 as i32, arg1 as i32, arg2 as *const i32),
        145 => process::sys_sched_getscheduler(arg0 as i32),
        146 => process::sys_sched_get_priority_max(arg0 as i32),
        147 => process::sys_sched_get_priority_min(arg0 as i32),
        148 => process::sys_sched_rr_get_interval(arg0 as i32, arg1 as *mut u8),
        157 => process::sys_prctl(arg0 as i32, arg1, arg2, arg3, arg4),
        158 => process::sys_arch_prctl(arg0 as i32, arg1),
        186 => process::sys_gettid(),
        200 => process::sys_tkill(arg0 as i32, arg1 as i32),
        201 => process::sys_time(arg0 as *mut i64),
        202 => process::sys_futex(
            arg0 as *mut i32,
            arg1 as i32,
            arg2 as i32,
            arg3,
            arg4 as *mut i32,
            _arg5 as i32,
        ),
        203 => process::sys_sched_setaffinity(arg0 as i32, arg1 as usize, arg2 as *const u8),
        204 => process::sys_sched_getaffinity(arg0 as i32, arg1 as usize, arg2 as *mut u8),
        218 => process::sys_set_tid_address(arg0 as *mut i32),
        227 => process::sys_clock_settime(arg0 as i32, arg1 as *const u8),
        228 => process::sys_clock_gettime(arg0 as i32, arg1 as *mut u8),
        229 => process::sys_clock_getres(arg0 as i32, arg1 as *mut u8),
        230 => process::sys_clock_nanosleep(
            arg0 as i32,
            arg1 as i32,
            arg2 as *const u8,
            arg3 as *mut u8,
        ),
        231 => process::sys_exit_group(arg0 as i32),
        234 => process::sys_tgkill(arg0 as i32, arg1 as i32, arg2 as i32),
        247 => process::sys_waitid(
            arg0 as i32,
            arg1 as i32,
            arg2 as *mut process::lifecycle::SigInfo,
            arg3 as i32,
            arg4 as *mut u8,
        ),
        270 => fs::sys_pselect6(
            arg0 as i32,
            arg1 as *mut u64,
            arg2 as *mut u64,
            arg3 as *mut u64,
            arg4 as *const fs::TimeSpec,
            _arg5 as *const u8,
        ),
        273 => process::sys_set_robust_list(arg0 as *const u8, arg1 as usize),
        274 => process::sys_get_robust_list(arg0 as i32, arg1 as *mut *mut u8, arg2 as *mut usize),
        297 => {
            signal::sys_rt_tgsigqueueinfo(arg0 as i32, arg1 as i32, arg2 as i32, arg3 as *const u8)
        }
        302 => process::sys_prlimit64(arg0 as i32, arg1 as i32, arg2 as *const u8, arg3 as *mut u8),
        309 => process::sys_getcpu(arg0 as *mut u32, arg1 as *mut u32, arg2 as *mut u8),
        316 => fs::sys_renameat2(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as i32,
            arg3 as *const u8,
            arg4 as u32,
        ),
        318 => process::sys_getrandom(arg0 as *mut u8, arg1 as usize, arg2 as u32),
        324 => memory::sys_mprotect(arg0, arg1 as usize, arg2 as i32),
        332 => fs::sys_statx(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as i32,
            arg3 as u32,
            arg4 as *mut fs::StatX,
        ),
        334 => process::sys_rseq(
            arg0 as *mut process::info::Rseq,
            arg1 as u32,
            arg2 as i32,
            arg3 as u32,
        ),
        424 => fs::sys_pidfd_send_signal(arg0 as i32, arg1 as i32, arg2 as *const u8, arg3 as u32),
        434 => fs::sys_pidfd_open(arg0 as i32, arg1 as u32),
        435 => process::sys_clone3(
            arg0 as *const process::lifecycle::CloneArgs,
            arg1 as usize,
            regs,
        ),
        436 => fs::sys_close_range(arg0 as u32, arg1 as u32, arg2 as u32),
        437 => fs::sys_openat2(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as *const fs::OpenHow,
            arg3 as usize,
        ),
        438 => fs::sys_pidfd_getfd(arg0 as i32, arg1 as i32, arg2 as u32),
        439 => fs::sys_faccessat(arg0 as i32, arg1 as *const u8, arg2 as i32, arg3 as i32),
        441 => fs::sys_epoll_pwait2(
            arg0 as i32,
            arg1 as *mut crate::fs::epoll::EpollEvent,
            arg2 as i32,
            arg3 as *const fs::TimeSpec,
            arg4 as *const u8,
            _arg5 as usize,
        ),
        449 => process::sys_futex_waitv(
            arg0 as *const process::futex::FutexWaitv,
            arg1 as u32,
            arg2 as u32,
            arg3 as *const process::futex::Timespec,
            arg4 as i32,
        ),
        452 => fs::sys_fchmodat(arg0 as i32, arg1 as *const u8, arg2 as u32, arg3 as i32),
        // System Administration & Kernel Control
        153 => 0, // vhangup
        154 => 0, // modify_ldt
        155 => fs::sys_pivot_root(arg0 as *const u8, arg1 as *const u8),
        159 => 0, // adjtimex
        161 => fs::sys_chroot(arg0 as *const u8),
        163 => 0, // acct
        164 => 0, // settimeofday
        167 => 0, // swapon
        168 => 0, // swapoff
        169 => fs::sys_reboot(arg0 as u32, arg1 as u32, arg2 as u32, arg3 as *const u8),
        170 => process::sys_sethostname(arg0 as *const u8, arg1 as usize),
        171 => process::sys_setdomainname(arg0 as *const u8, arg1 as usize),
        172 => 0,                       // iopl
        173 => 0,                       // ioperm
        175 => -(Errno::EPERM as i64),  // init_module
        176 => -(Errno::EPERM as i64),  // delete_module
        179 => -(Errno::ENOSYS as i64), // quotactl
        205 => 0,                       // set_thread_area
        211 => 0,                       // get_thread_area
        246 => -(Errno::EPERM as i64),  // kexec_load
        248 => -(Errno::ENOSYS as i64), // add_key
        249 => -(Errno::ENOSYS as i64), // request_key
        250 => -(Errno::ENOSYS as i64), // keyctl
        251 => 0,                       // ioprio_set
        252 => 0,                       // ioprio_get
        253 => fs::sys_inotify_init(),
        254 => fs::sys_inotify_add_watch(arg0 as i32, arg1 as *const u8, arg2 as u32),
        255 => fs::sys_inotify_rm_watch(arg0 as i32, arg1 as i32),
        256 => 0, // migrate_pages
        272 => process::sys_unshare(arg0),

        279 => 0, // move_pages
        294 => fs::sys_inotify_init1(arg0 as i32),
        298 => -(Errno::ENODEV as i64),     // perf_event_open
        300 => -(Errno::ENOSYS as i64),     // fanotify_init
        301 => -(Errno::ENOSYS as i64),     // fanotify_mark
        303 => -(Errno::EOPNOTSUPP as i64), // name_to_handle_at
        304 => -(Errno::EOPNOTSUPP as i64), // open_by_handle_at
        305 => 0,                           // clock_adjtime
        308 => 0,                           // setns
        310 => 0,                           // process_vm_readv
        311 => 0,                           // process_vm_writev
        312 => 0,                           // kcmp
        313 => -(Errno::EPERM as i64),      // finit_module
        314 => 0,                           // sched_setattr
        315 => 0,                           // sched_getattr
        320 => -(Errno::EPERM as i64),      // kexec_file_load
        321 => -(Errno::ENOSYS as i64),     // bpf
        322 => process::sys_execve(
            arg1 as *const u8,
            arg2 as *const *const u8,
            arg3 as *const *const u8,
        ), // execveat
        323 => -(Errno::ENOSYS as i64),     // userfaultfd
        329 => memory::sys_mprotect(arg0, arg1 as usize, arg2 as i32), // pkey_mprotect
        330 => -(Errno::ENOSYS as i64),     // pkey_alloc
        331 => 0,                           // pkey_free
        333 => 0,                           // io_pgetevents
        425 => -(Errno::ENOSYS as i64),     // io_uring_setup
        426 => -(Errno::ENOSYS as i64),     // io_uring_enter
        427 => -(Errno::ENOSYS as i64),     // io_uring_register
        // Identity
        102 => process::sys_getuid(),
        104 => process::sys_getgid(),
        105 => process::sys_setuid(arg0 as u32),
        106 => process::sys_setgid(arg0 as u32),
        107 => process::sys_geteuid(),
        108 => process::sys_getegid(),
        24 => process::sys_sched_yield(),
        // Network
        41 => net::sys_socket(arg0 as i32, arg1 as i32, arg2 as i32),
        42 => net::sys_connect(arg0 as i32, arg1 as *const net::SockAddrIn, arg2 as u32),
        43 => net::sys_accept(arg0 as i32, arg1 as *mut net::SockAddrIn, arg2 as *mut u32),
        44 => net::sys_sendto(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as usize,
            arg3 as i32,
            arg4 as *const net::SockAddrIn,
            _arg5 as u32,
        ),
        45 => net::sys_recvfrom(
            arg0 as i32,
            arg1 as *mut u8,
            arg2 as usize,
            arg3 as i32,
            arg4 as *mut net::SockAddrIn,
            _arg5 as *mut u32,
        ),
        46 => net::sys_sendmsg(arg0 as i32, arg1 as *const net::Msghdr, arg2 as i32),
        47 => net::sys_recvmsg(arg0 as i32, arg1 as *mut net::Msghdr, arg2 as i32),
        48 => net::sys_shutdown(arg0 as i32, arg1 as i32),
        49 => net::sys_bind(arg0 as i32, arg1 as *const net::SockAddrIn, arg2 as u32),
        50 => net::sys_listen(arg0 as i32, arg1 as i32),
        51 => net::sys_getsockname(arg0 as i32, arg1 as *mut net::SockAddrIn, arg2 as *mut u32),
        52 => net::sys_getpeername(arg0 as i32, arg1 as *mut net::SockAddrIn, arg2 as *mut u32),
        53 => net::sys_socketpair(arg0 as i32, arg1 as i32, arg2 as i32, arg3 as *mut i32),
        54 => net::sys_setsockopt(
            arg0 as i32,
            arg1 as i32,
            arg2 as i32,
            arg3 as *const u8,
            arg4 as u32,
        ),
        55 => net::sys_getsockopt(
            arg0 as i32,
            arg1 as i32,
            arg2 as i32,
            arg3 as *mut u8,
            arg4 as *mut u32,
        ),
        288 => net::sys_accept4(
            arg0 as i32,
            arg1 as *mut net::SockAddrIn,
            arg2 as *mut u32,
            arg3 as i32,
        ),
        299 => net::sys_recvmmsg(
            arg0 as i32,
            arg1 as *mut net::MMsghdr,
            arg2 as u32,
            arg3 as i32,
            arg4 as *const fs::TimeSpec,
        ),
        307 => net::sys_sendmmsg(
            arg0 as i32,
            arg1 as *mut net::MMsghdr,
            arg2 as u32,
            arg3 as i32,
        ),

        // Landlock security subsystem (not implemented -> clean ENOSYS)
        444 => -(Errno::ENOSYS as i64), // landlock_create_ruleset
        445 => -(Errno::ENOSYS as i64), // landlock_add_rule
        446 => -(Errno::ENOSYS as i64), // landlock_restrict_self

        _ => {
            kprintln!("[syscall] Unknown syscall: {}", syscall_num);
            Errno::ENOSYS.into()
        }
    }
}
