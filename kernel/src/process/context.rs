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

//! CPU context for context switching.
//!
//! This module defines the CPU register state that must be saved and
//! restored when switching between tasks.
//!
//! On x86_64, the calling convention requires the callee to preserve:
//! - rbx, rbp, r12–r15, rsp
//! - RFLAGS
//! - Segment registers (not needed in 64-bit flat model)

/// Saved CPU register context for a task.
///
/// This structure holds all the registers that need to be preserved
/// across context switches. The `syscall`/`sysret` path handles
/// user-mode registers separately.
///
/// ## Layout (offsets used by assembly)
///
/// ```text
/// Offset  Register
/// 0x00    rbx
/// 0x08    rbp
/// 0x10    r12
/// 0x18    r13
/// 0x20    r14
/// 0x28    r15
/// 0x30    rsp
/// 0x38    rip
/// 0x40    rflags
/// 0x48    cr3
/// 0x50    fs_base
/// 0x58    gs_base
/// 0x60    kernel_gs_base
/// 0x68    _reserved (alignment pad)
/// 0x70    fxsave (512 bytes, 16-byte aligned FPU/SSE state)
/// ```
#[derive(Debug, Clone, Copy)]
#[repr(C, align(16))]
pub struct CpuContext {
    /// General purpose registers (callee-saved).
    pub rbx: u64,
    /// Base pointer.
    pub rbp: u64,
    /// Callee-saved register.
    pub r12: u64,
    /// Callee-saved register.
    pub r13: u64,
    /// Callee-saved register.
    pub r14: u64,
    /// Callee-saved register.
    pub r15: u64,
    /// Stack pointer.
    pub rsp: u64,
    /// Instruction pointer (return address).
    pub rip: u64,
    /// RFLAGS register.
    pub rflags: u64,
    /// CR3 (page table root) — for address space switching.
    pub cr3: u64,
    /// FS_BASE MSR (for Thread Local Storage) — offset 0x50.
    pub fs_base: u64,
    /// IA32_GS_BASE MSR — offset 0x58.
    pub gs_base: u64,
    /// IA32_KERNEL_GS_BASE MSR — offset 0x60.
    pub kernel_gs_base: u64,
    /// Reserved alignment padding for 16-byte aligned FXSAVE area.
    pub _reserved: u64,
    /// 512-byte x87 FPU and SSE/AVX register state.
    pub fxsave: [u8; 512],
}

impl Default for CpuContext {
    fn default() -> Self {
        let mut fxsave = [0u8; 512];
        // FCW = 0x037F (default x87 control word: mask all exceptions, 64-bit precision, round to nearest)
        fxsave[0] = 0x7f;
        fxsave[1] = 0x03;
        // MXCSR = 0x1F80 (default SSE control/status register: mask all SIMD exceptions, round to nearest)
        fxsave[24] = 0x80;
        fxsave[25] = 0x1f;
        Self {
            rbx: 0,
            rbp: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rsp: 0,
            rip: 0,
            rflags: 0x2, // Clear IF (Interrupt Flag) so tasks start with interrupts disabled
            cr3: 0,
            fs_base: 0,
            gs_base: 0,
            kernel_gs_base: 0,
            _reserved: 0,
            fxsave,
        }
    }
}

impl CpuContext {
    /// Create a new context that will start executing at the given
    /// instruction pointer with the given stack pointer.
    pub fn new(entry_point: u64, stack_pointer: u64, page_table: u64) -> Self {
        Self {
            rip: entry_point,
            rsp: stack_pointer,
            cr3: page_table,
            rflags: 0x2, // Interrupts disabled initially in Ring 0
            fs_base: 0,
            ..Default::default()
        }
    }
}

/// Perform a context switch from one task to another.
///
/// Saves the current CPU state into `old_ctx` and restores state from `new_ctx`.
///
/// # Safety
///
/// - Both context pointers must be valid and properly aligned.
/// - The new context must have a valid stack pointer and instruction pointer.
/// - This function effectively "returns" to a different call site.
///
/// ## Register Usage
///
/// - `rdi` = pointer to old CpuContext (first arg in System V ABI)
/// - `rsi` = pointer to new CpuContext (second arg)
///
/// ## How it works
///
/// 1. Save all callee-saved registers and FPU/SSE state into `old_ctx`
/// 2. Save the current stack pointer and return address
/// 3. If the new task has a different CR3 (page table), switch address spaces
/// 4. Restore FPU/SSE state and all callee-saved registers from `new_ctx`
/// 5. Jump to the new task's saved instruction pointer
///
/// After this function, execution continues at the point where `new_ctx`
/// was previously saved — effectively "returning" into a different task.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_context(_old_ctx: *mut CpuContext, _new_ctx: *const CpuContext) {
    // SAFETY: This is a naked function that manually manages the
    // stack and registers. The caller guarantees valid context pointers.
    core::arch::naked_asm!(
        // ── Save old context ───────────────────────────────────────
        // rdi = old_ctx pointer
        "mov [rdi + 0x00], rbx", // Save rbx
        "mov [rdi + 0x08], rbp", // Save rbp
        "mov [rdi + 0x10], r12", // Save r12
        "mov [rdi + 0x18], r13", // Save r13
        "mov [rdi + 0x20], r14", // Save r14
        "mov [rdi + 0x28], r15", // Save r15
        "lea rax, [rsp + 8]",    // Get RSP before the call
        "mov [rdi + 0x30], rax", // Save it
        // Save the return address (the address after the call
        // to switch_context in the old task)
        "mov rax, [rsp]",        // Get return address from stack
        "mov [rdi + 0x38], rax", // Save as rip
        // Save RFLAGS
        "pushfq",
        "pop rax",
        "mov [rdi + 0x40], rax",
        // Save CR3 (current page table)
        "mov rax, cr3",
        "mov [rdi + 0x48], rax",
        // Save FS_BASE using rdfsbase (since userspace can now modify FS_BASE directly)
        "rdfsbase rax",
        "mov [rdi + 0x50], rax",
        // Save FPU/SSE state (XMM0-XMM15, MXCSR, FPU control words)
        "fxsave64 [rdi + 0x70]",
        // ── Restore new context ────────────────────────────────────
        // rsi = new_ctx pointer

        // Restore FPU/SSE state before register clobbers
        "fxrstor64 [rsi + 0x70]",
        // 1. Read all values from [rsi] while still in the current address space
        // where [rsi] is guaranteed to be mapped.
        "mov rbx, [rsi + 0x00]",
        "mov rbp, [rsi + 0x08]",
        "mov r12, [rsi + 0x10]",
        "mov r13, [rsi + 0x18]",
        "mov r14, [rsi + 0x20]",
        "mov r15, [rsi + 0x28]",
        "mov r8,  [rsi + 0x30]", // r8 = new RSP
        "mov r9,  [rsi + 0x38]", // r9 = new RIP
        "mov r10, [rsi + 0x40]", // r10 = new RFLAGS
        "mov r11, [rsi + 0x48]", // r11 = new CR3
        "mov rax, [rsi + 0x50]", // rax = new FS_BASE
        "mov rdx, [rsi + 0x60]", // rdx = new KERNEL_GS_BASE
        // 2. Switch CR3 page table (all values are already safely in CPU registers)
        "test r11, r11",
        "jz 4f",
        "mov cr3, r11",
        "4:",
        // 3. Switch to the new stack (now mapped in the new address space)
        "mov rsp, r8",
        // 4. Restore RFLAGS
        "push r10",
        "popfq",
        // 5. Restore FS_BASE (TLS)
        "wrfsbase rax",
        // 6. Restore KERNEL_GS_BASE MSR if non-zero
        "test rdx, rdx",
        "jz 5f",
        "mov rax, rdx",
        "shr rdx, 32",
        "mov ecx, 0xC0000102",
        "wrmsr",
        "5:",
        // 7. Jump to the new task's entry point
        "jmp r9",
    );
}

/// Prepare a new task's stack for its first context switch.
///
/// Sets up the initial stack frame so that when `switch_context` restores
/// this task for the first time, it will "return" to `entry_point`.
///
/// The stack layout after this function:
/// ```text
/// stack_top:
///   ... (empty space) ...
///   [entry_point]       ← RSP will point here after switch_context
/// ```
pub fn prepare_initial_stack(
    stack_base: u64,
    stack_size: usize,
    entry_point: u64,
    page_table: u64,
) -> CpuContext {
    let stack_top = stack_base + stack_size as u64;

    // Align stack to 16 bytes (System V ABI requirement)
    let stack_top = stack_top & !0xF;

    CpuContext::new(entry_point, stack_top, page_table)
}

/// Transition from kernel space (Ring 0) to user space (Ring 3).
///
/// Switches CR3 to the new process page table, builds the iretq stack frame,
/// zeroes general-purpose registers to avoid data leaks, and runs `iretq` to
/// transition privileges and execute at `entry_point` in Ring 3.
///
/// # Safety
///
/// This function never returns. The caller must guarantee:
/// - The `page_table` is a valid physical address of a PML4 page table.
/// - The `user_stack` is a valid user space virtual address.
/// - The `entry_point` is a valid user space instruction address.
#[unsafe(naked)]
pub unsafe extern "C" fn enter_user_mode(
    _entry_point: u64,
    _user_stack: u64,
    _page_table: u64,
    _user_code_selector: u64,
    _user_data_selector: u64,
) -> ! {
    core::arch::naked_asm!(
        "cli", // Disable interrupts during transition
        // Switch CR3 to user page table (passed in rdx)
        "mov cr3, rdx",
        // Build the iretq stack frame:
        // SS (User Data segment selector passed in r8)
        "push r8",
        // RSP (User stack pointer, passed in rsi)
        "push rsi",
        // RFLAGS (0x202: Interrupts enabled)
        "push 0x202",
        // CS (User Code segment selector passed in rcx)
        "push rcx",
        // RIP (Entry point, passed in rdi)
        "push rdi",
        // Zero out all general-purpose registers to prevent register leaks
        "xor rax, rax",
        "xor rbx, rbx",
        "xor rcx, rcx",
        "xor rdx, rdx",
        "xor rsi, rsi",
        "xor rdi, rdi",
        "xor rbp, rbp",
        "xor r8, r8",
        "xor r9, r9",
        "xor r10, r10",
        "xor r11, r11",
        "xor r12, r12",
        "xor r13, r13",
        "xor r14, r14",
        "xor r15, r15",
        "swapgs",
        // Switch to user-space!
        "iretq"
    );
}

#[no_mangle]
pub extern "C" fn fork_child_return_debug() {
    unsafe {
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x3F8);
        for &b in b"[debug] Entering fork_child_return in child task!\n" {
            port.write(b);
        }
    }
}

/// Naked assembly return path for fork'ed child processes.
///
/// When the scheduler switches to a newly forked child, it enters here.
/// We restore all general-purpose registers from the `SavedRegisters` struct
/// located on the child's kernel stack, set rax to 0 (indicating the child process),
/// and return to user space via `sysretq`.
#[unsafe(naked)]
pub unsafe extern "C" fn fork_child_return() -> ! {
    core::arch::naked_asm!(
        "call {}",      // Release scheduler lock
        "pop rax",      // Pop and discard the parent's rax
        "xor rax, rax", // rax = 0 (child process return value)
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
        "pop rsp", // Restore User RSP
        "swapgs",
        "sysretq",
        sym crate::process::scheduler::scheduler_unlock_after_switch,
    );
}
