//! POSIX-compatible syscall interface for KontsnorOS.
//!
//! This module implements the system call layer that provides Unix
//! compatibility. User-space programs invoke syscalls via the `syscall`
//! instruction on x86_64, which transfers control to the kernel.

use crate::kprintln;
pub mod fs;
pub mod io;
pub mod memory;
pub mod process;
pub mod signal;
pub mod net;

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
    Getcwd = 79,
    Chdir = 80,
    Mkdir = 83,
    Rmdir = 84,
    Unlink = 87,
    Getuid = 102,
    Getgid = 104,
    Setuid = 105,
    Setgid = 106,
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
    pub rip: u64, // rcx
    pub rflags: u64, // r11
    pub rsp: u64, // User stack pointer, pushed first!
}

// ── Fast Syscall Assembly Entry Point ────────────────────────────────

core::arch::global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "swapgs",                             // Swap GS with Kernel GS
    "mov gs:[0], rsp",                    // Save user RSP in CpuScratch.user_rsp
    "mov rsp, gs:[8]",                    // Load kernel stack pointer from CpuScratch.kernel_rsp
    
    // Push registers in reverse order of SavedRegisters struct
    "push qword ptr gs:[0]", // User RSP (rsp)
    "push r11", // User RFLAGS (rflags)
    "push rcx", // User RIP (rip)
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
    "pop rcx", // User RIP
    "pop r11", // User RFLAGS
    "pop qword ptr gs:[0]", // User RSP
    
    // Restore user stack pointer and swapgs back
    "mov rsp, gs:[0]",
    "swapgs",
    "sysretq",
);

/// CPU-local scratch space for syscall privilege transitions.
#[repr(C, align(16))]
pub struct CpuScratch {
    pub user_rsp: u64,
    pub kernel_rsp: u64,
}

/// Static mutable CPU scratch space for the Bootstrap Processor (BSP).
#[no_mangle]
pub static mut CPU_SCRATCH: CpuScratch = CpuScratch {
    user_rsp: 0,
    kernel_rsp: 0,
};

/// Set the temporary kernel stack pointer for syscall entry.
pub fn set_kernel_stack(stack: u64) {
    unsafe {
        CPU_SCRATCH.kernel_rsp = stack;
    }
}

/// Dispatcher assembly calling wrapper.
#[no_mangle]
pub extern "C" fn syscall_dispatch_rust(
    regs: *mut SavedRegisters,
    syscall_num: u64,
) -> i64 {
    if syscall_num == 15 {
        return crate::syscall::signal::sys_rt_sigreturn(regs);
    }

    let arg0 = unsafe { (*regs).rdi };
    let arg1 = unsafe { (*regs).rsi };
    let arg2 = unsafe { (*regs).rdx };
    let arg3 = unsafe { (*regs).r10 };
    let arg4 = unsafe { (*regs).r8 };
    let arg5 = unsafe { (*regs).r9 };

    // Debug print FS_BASE and canary
    let fs_base = x86_64::registers::model_specific::FsBase::read().as_u64();
    let user_rsp = unsafe { (*regs).rsp };
    let mut canary_msg = alloc::string::String::new();
    if fs_base != 0 {
        let canary_addr = fs_base + 0x28;
        if let Some(phys) = crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(canary_addr)) {
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
        if let Some(_phys) = crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(user_rsp)) {
            // print 4 words from RSP
            let mut words = [0u64; 4];
            for i in 0..4 {
                let addr = user_rsp + (i * 8);
                if let Some(p) = crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(addr)) {
                    let v = p.as_u64() + crate::memory::r#virtual::phys_mem_offset();
                    words[i as usize] = unsafe { *(v as *const u64) };
                }
            }
            stack_msg = alloc::format!("rsp={:#x} stack=[{:#x}, {:#x}, {:#x}, {:#x}]", 
                user_rsp, words[0], words[1], words[2], words[3]);
        }
    }

    crate::kprintln!("[debug syscall {}] args=[{:#x}, {:#x}, {:#x}] fs_base={:#x} {} {}", 
        syscall_num, arg0, arg1, arg2, fs_base, canary_msg, stack_msg);

    let res = dispatch(regs, syscall_num, arg0, arg1, arg2, arg3, arg4, arg5);
    crate::kprintln!("[debug syscall {} ret] res={}", syscall_num, res);
    
    if syscall_num == 16 {
        let mut words_after = [0u64; 8];
        for i in 0..8 {
            let addr = user_rsp + (i * 8);
            if let Some(p) = crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(addr)) {
                let v = p.as_u64() + crate::memory::r#virtual::phys_mem_offset();
                words_after[i as usize] = unsafe { *(v as *const u64) };
            }
        }
        crate::kprintln!("[debug syscall 16 ret] stack_after=[{:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}]",
            words_after[0], words_after[1], words_after[2], words_after[3],
            words_after[4], words_after[5], words_after[6], words_after[7]);
    }

    unsafe {
        (*regs).rax = res as u64;
    }

    // Call signal delivery handler before returning to user space
    crate::syscall::signal::handle_pending_signals(regs);

    unsafe { (*regs).rax as i64 }
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

        // 5. Configure IA32_GS_BASE (active GS base in kernel) to point to CPU_SCRATCH
        let mut gs_base_msr = Msr::new(0xC0000101);
        let scratch_addr = core::ptr::addr_of!(CPU_SCRATCH) as u64;
        gs_base_msr.write(scratch_addr);

        // 6. Configure IA32_KERNEL_GS_BASE MSR to 0 (swapped GS base, initially 0 for user space)
        let mut kernel_gs_msr = Msr::new(0xC0000102);
        kernel_gs_msr.write(0);
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
        0  => fs::sys_read(arg0 as i32, arg1 as *mut u8, arg2 as usize),
        1  => fs::sys_write(arg0 as i32, arg1 as *const u8, arg2 as usize),
        2  => fs::sys_open(arg0 as *const u8, arg1 as i32, arg2 as u32),
        3  => fs::sys_close(arg0 as i32),
        4  => fs::sys_stat(arg0 as *const u8, arg1 as *mut fs::LinuxStat),
        5  => fs::sys_fstat(arg0 as i32, arg1 as *mut fs::LinuxStat),
        6  => fs::sys_lstat(arg0 as *const u8, arg1 as *mut fs::LinuxStat),
        7  => fs::sys_poll(arg0 as *mut u8, arg1, arg2 as i32),
        8  => fs::sys_lseek(arg0 as i32, arg1 as i64, arg2 as i32),
        16 => io::sys_ioctl(arg0 as i32, arg1, arg2),
        17 => fs::sys_pread64(arg0 as i32, arg1 as *mut u8, arg2 as usize, arg3 as i64),
        20 => fs::sys_writev(arg0 as i32, arg1 as *const fs::IoVec, arg2 as i32),
        21 => fs::sys_access(arg0 as *const u8, arg1 as i32),
        22 => fs::sys_pipe(arg0 as *mut i32),
        32 => fs::sys_dup(arg0 as i32),
        33 => fs::sys_dup2(arg0 as i32, arg1 as i32),
        72 => fs::sys_fcntl(arg0 as i32, arg1 as i32, arg2),
        79 => fs::sys_getcwd(arg0 as *mut u8, arg1 as usize),
        80 => fs::sys_chdir(arg0 as *const u8),
        82 => fs::sys_rename(arg0 as *const u8, arg1 as *const u8),
        83 => fs::sys_mkdir(arg0 as *const u8, arg1 as u32),
        84 => fs::sys_rmdir(arg0 as *const u8),
        86 => fs::sys_link(arg0 as *const u8, arg1 as *const u8),
        87 => fs::sys_unlink(arg0 as *const u8),
        89 => fs::sys_readlink(arg0 as *const u8, arg1 as *mut u8, arg2 as usize),
        217 => fs::sys_getdents64(arg0 as i32, arg1 as *mut u8, arg2 as usize),
        257 => fs::sys_openat(arg0 as i32, arg1 as *const u8, arg2 as i32, arg3 as u32),
        262 => fs::sys_newfstatat(arg0 as i32, arg1 as *const u8, arg2 as *mut fs::LinuxStat, arg3 as i32),
        267 => fs::sys_readlinkat(arg0 as i32, arg1 as *const u8, arg2 as *mut u8, arg3 as usize),
        269 => fs::sys_faccessat(arg0 as i32, arg1 as *const u8, arg2 as i32, arg3 as i32),
        // Memory
        9  => memory::sys_mmap(arg0, arg1 as usize, arg2 as i32, arg3 as i32, arg4 as i32, _arg5 as i64),
        10 => memory::sys_mprotect(arg0, arg1 as usize, arg2 as i32),
        11 => memory::sys_munmap(arg0, arg1 as usize),
        12 => memory::sys_brk(arg0),
        // Process
        13 => signal::sys_rt_sigaction(arg0 as i32, arg1 as *const crate::process::task::SigAction, arg2 as *mut crate::process::task::SigAction, arg3 as usize),
        14 => signal::sys_rt_sigprocmask(arg0 as i32, arg1 as *const u64, arg2 as *mut u64, arg3 as usize),
        35 => process::sys_nanosleep(arg0 as *const u8, arg1 as *mut u8),
        39 => process::sys_getpid(),
        56 => process::sys_clone(arg0, arg1, arg2 as *mut i32, arg3 as *mut i32, arg4, regs),
        57 => process::sys_fork(regs),
        59 => process::sys_execve(arg0 as *const u8, arg1 as *const *const u8, arg2 as *const *const u8),
        60 => process::sys_exit(arg0 as i32),
        61 => process::sys_wait4(arg0 as i32, arg1 as *mut i32, arg2 as i32, arg3 as *mut u8),
        62 => signal::sys_kill(arg0 as i32, arg1 as i32),
        63 => process::sys_uname(arg0 as *mut u8),
        96 => process::sys_gettimeofday(arg0 as *mut u8, arg1 as *mut u8),
        97 => process::sys_getrlimit(arg0 as i32, arg1 as *mut u8),
        99 => process::sys_sysinfo(arg0 as *mut u8),
        100 => process::sys_times(arg0 as *mut u8),
        109 => process::sys_setpgid(arg0 as i32, arg1 as i32),
        110 => process::sys_getppid(),
        112 => process::sys_setsid(),
        121 => process::sys_getpgid(arg0 as i32),
        131 => process::sys_sigaltstack(arg0 as *const u8, arg1 as *mut u8),
        157 => process::sys_prctl(arg0 as i32, arg1, arg2, arg3, arg4),
        158 => process::sys_arch_prctl(arg0 as i32, arg1),
        186 => process::sys_gettid(),
        218 => process::sys_set_tid_address(arg0 as *mut i32),
        228 => process::sys_clock_gettime(arg0 as i32, arg1 as *mut u8),
        231 => process::sys_exit_group(arg0 as i32),
        234 => process::sys_tgkill(arg0 as i32, arg1 as i32, arg2 as i32),
        302 => process::sys_prlimit64(arg0 as i32, arg1 as i32, arg2 as *const u8, arg3 as *mut u8),
        318 => process::sys_getrandom(arg0 as *mut u8, arg1 as usize, arg2 as u32),
        // Identity
        102 => process::sys_getuid(),
        104 => process::sys_getgid(),
        107 => process::sys_geteuid(),
        108 => process::sys_getegid(),
        // Network
        41 => net::sys_socket(arg0 as i32, arg1 as i32, arg2 as i32),
        42 => net::sys_connect(arg0 as i32, arg1 as *const net::SockAddrIn, arg2 as u32),
        43 => net::sys_accept(arg0 as i32, arg1 as *mut net::SockAddrIn, arg2 as *mut u32),
        44 => net::sys_sendto(arg0 as i32, arg1 as *const u8, arg2 as usize, arg3 as i32, arg4 as *const net::SockAddrIn, _arg5 as u32),
        45 => net::sys_recvfrom(arg0 as i32, arg1 as *mut u8, arg2 as usize, arg3 as i32, arg4 as *mut net::SockAddrIn, _arg5 as *mut u32),
        49 => net::sys_bind(arg0 as i32, arg1 as *const net::SockAddrIn, arg2 as u32),
        50 => net::sys_listen(arg0 as i32, arg1 as i32),
        _ => {
            kprintln!("[syscall] Unknown syscall: {}", syscall_num);
            Errno::ENOSYS.into()
        }
    }
}

