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
/// ```
#[derive(Debug, Clone, Copy)]
#[repr(C)]
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
}

impl Default for CpuContext {
    fn default() -> Self {
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
            gs_base: core::ptr::addr_of!(crate::syscall::CPU_SCRATCH) as u64,
            kernel_gs_base: 0,
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
/// 1. Save all callee-saved registers into `old_ctx`
/// 2. Save the current stack pointer and return address
/// 3. If the new task has a different CR3 (page table), switch address spaces
/// 4. Restore all callee-saved registers from `new_ctx`
/// 5. Jump to the new task's saved instruction pointer
///
/// After this function, execution continues at the point where `new_ctx`
/// was previously saved — effectively "returning" into a different task.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_context(
    _old_ctx: *mut CpuContext,
    _new_ctx: *const CpuContext,
) {
    // SAFETY: This is a naked function that manually manages the
    // stack and registers. The caller guarantees valid context pointers.
    core::arch::naked_asm!(
        // ── Save old context ───────────────────────────────────────
        // rdi = old_ctx pointer
        "mov [rdi + 0x00], rbx",        // Save rbx
        "mov [rdi + 0x08], rbp",        // Save rbp
        "mov [rdi + 0x10], r12",        // Save r12
        "mov [rdi + 0x18], r13",        // Save r13
        "mov [rdi + 0x20], r14",        // Save r14
        "mov [rdi + 0x28], r15",        // Save r15
        "lea rax, [rsp + 8]",           // Get RSP before the call
        "mov [rdi + 0x30], rax",        // Save it

        // Save the return address (the address after the call
        // to switch_context in the old task)
        "mov rax, [rsp]",               // Get return address from stack
        "mov [rdi + 0x38], rax",        // Save as rip

        // Save RFLAGS
        "pushfq",
        "pop rax",
        "mov [rdi + 0x40], rax",

        // Save CR3 (current page table)
        "mov rax, cr3",
        "mov [rdi + 0x48], rax",

        // Save FS_BASE MSR
        "mov ecx, 0xC0000100",          // FS_BASE MSR
        "rdmsr",                        // Reads MSR into edx:eax
        "shl rdx, 32",
        "or rax, rdx",                  // Full 64-bit value in rax
        "mov [rdi + 0x50], rax",        // Save to old context

        // Save GS_BASE MSR
        "mov ecx, 0xC0000101",          // GS_BASE MSR
        "rdmsr",                        // Reads MSR into edx:eax
        "shl rdx, 32",
        "or rax, rdx",                  // Full 64-bit value in rax
        "mov [rdi + 0x58], rax",        // Save to old context

        // Save KERNEL_GS_BASE MSR
        "mov ecx, 0xC0000102",          // KERNEL_GS_BASE MSR
        "rdmsr",                        // Reads MSR into edx:eax
        "shl rdx, 32",
        "or rax, rdx",                  // Full 64-bit value in rax
        "mov [rdi + 0x60], rax",        // Save to old context

        // ── Restore new context ────────────────────────────────────
        // rsi = new_ctx pointer

        // Check if we need to switch page tables
        "mov rax, [rsi + 0x48]",        // Load new CR3
        "test rax, rax",                // Skip if CR3 is 0 (kernel task)
        "jz 2f",
        "mov rcx, cr3",                 // Current CR3
        "cmp rax, rcx",                 // Same page table?
        "je 2f",                        // Skip if same
        "mov cr3, rax",                 // Switch page tables (flushes TLB)
        "2:",

        // Switch stack FIRST, so we are on a valid, mapped stack in the new page table
        "mov rsp, [rsi + 0x30]",

        // Restore RFLAGS
        "mov rax, [rsi + 0x40]",
        "push rax",
        "popfq",

        // Restore FS_BASE MSR
        "mov rax, [rsi + 0x50]",
        "mov rdx, rax",
        "shr rdx, 32",                  // High 32 bits in edx
        "mov ecx, 0xC0000100",          // FS_BASE MSR
        "wrmsr",                        // Writes edx:eax to MSR

        // Restore GS_BASE MSR
        "mov rax, [rsi + 0x58]",
        "mov rdx, rax",
        "shr rdx, 32",                  // High 32 bits in edx
        "mov ecx, 0xC0000101",          // GS_BASE MSR
        "wrmsr",                        // Writes edx:eax to MSR

        // Restore KERNEL_GS_BASE MSR
        "mov rax, [rsi + 0x60]",
        "mov rdx, rax",
        "shr rdx, 32",                  // High 32 bits in edx
        "mov ecx, 0xC0000102",          // KERNEL_GS_BASE MSR
        "wrmsr",                        // Writes edx:eax to MSR

        // Restore callee-saved registers
        "mov rbx, [rsi + 0x00]",
        "mov rbp, [rsi + 0x08]",
        "mov r12, [rsi + 0x10]",
        "mov r13, [rsi + 0x18]",
        "mov r14, [rsi + 0x20]",
        "mov r15, [rsi + 0x28]",

        // Jump to the new task's saved instruction pointer.
        // We push the RIP onto the new stack and ret into it,
        // which simulates a normal function return.
        "mov rax, [rsi + 0x38]",
        "push rax",
        "ret",
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
) -> ! {
    core::arch::naked_asm!(
        // Switch CR3 to user page table (passed in rdx)
        "mov cr3, rdx",

        // Build the iretq stack frame:
        // SS (User Data segment selector with RPL 3: 0x18 | 3 = 0x1B)
        "push 0x1B",
        // RSP (User stack pointer, passed in rsi)
        "push rsi",
        // RFLAGS (0x202: Interrupts enabled)
        "push 0x202",
        // CS (User Code segment selector with RPL 3: 0x20 | 3 = 0x23)
        "push 0x23",
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

/// Naked assembly return path for fork'ed child processes.
///
/// When the scheduler switches to a newly forked child, it enters here.
/// We restore all general-purpose registers from the `SavedRegisters` struct
/// located on the child's kernel stack, set rax to 0 (indicating the child process),
/// and return to user space via `sysretq`.
#[unsafe(naked)]
pub unsafe extern "C" fn fork_child_return() -> ! {
    core::arch::naked_asm!(
        "pop rax",          // Pop and discard the parent's rax
        "xor rax, rax",     // rax = 0 (child process return value)
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
        "pop rcx",          // User RIP
        "pop r11",          // User RFLAGS
        "pop rsp",          // Restore User RSP
        "swapgs",
        "sysretq"
    );
}


