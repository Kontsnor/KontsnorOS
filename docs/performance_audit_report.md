# KontsnorOS Core Performance & Security Audit Report

This report presents a comprehensive performance and security audit of the **KontsnorOS** codebase (kernel version 0.1.0, Rust-native, SMP, hybrid architecture). It identifies critical bottlenecks that restrict system throughput and scalability under load, and outlines concrete optimization plans designed to preserve the security boundaries of Ring 0/Ring 3.

---

## 1. Lock Contention & Serialization

### 1.1 MLFQ Scheduler Global Lock (`SCHEDULER`)
* **Location**: [kernel/src/process/scheduler.rs](file:///home/kontsnor/Projects/KontsnorOS/kernel/src/process/scheduler.rs#L38)
* **Bottleneck**: The scheduler uses a single global `TicketLock<Option<Scheduler>>` to serialize access to the entire MLFQ queues, tasks table, and active core tracking array (`current_cpus`). On SMP systems with multiple active CPU cores, every context switch, cooperative yield, thread blocking/waking event, and APIC timer tick requires acquiring this global lock. This serializes multi-tasking across all cores.
* **Remedy**:
  1. **Per-Core Run Queues**: Partition the MLFQ queues so that each logical CPU core manages its own local array of priority run queues (`queues: [VecDeque<Pid>; NUM_PRIORITIES]`), protected by a local spinlock.
  2. **Work-Stealing/Load Balancing**: Implement cooperative work-stealing. When a core's local runqueue is empty, it can attempt to acquire another core's queue lock to steal runnable tasks.
  3. **Decoupled Task Table**: Move the master tasks vector (`tasks: Vec<Option<Box<Task>>>`) out of the scheduler's lock. Tasks can be ref-counted via `Arc<Task>` or protected with fine-grained spinlocks so that task updates (e.g., changing state, updating descriptors) do not serialize the scheduler.

### 1.2 CPU-Local PID Resolution (`current_pid()`)
* **Location**: [kernel/src/process/scheduler.rs](file:///home/kontsnor/Projects/KontsnorOS/kernel/src/process/scheduler.rs#L315-L318)
* **Bottleneck**: Checking the currently executing task's PID via `scheduler::current_pid()` requires locking the global `SCHEDULER` lock to read `scheduler.current_cpus[apic_id]`. This is called frequently (e.g., in VFS path resolution and permission checks).
* **Remedy**: Store the currently running task's PID (or a pointer to the active `Task` struct) directly inside the CPU-local scratch space (`CpuScratch` or a per-core scratch structure). This allows `current_pid()` to be resolved in a single lock-free assembly/Rust read using the `GS` segment register (`gs:[16]`), completely bypassing the scheduler.

### 1.3 Global GDT and TSS Lock (`CORE_GDT`)
* **Location**: [kernel/src/arch/x86_64/gdt.rs](file:///home/kontsnor/Projects/KontsnorOS/kernel/src/arch/x86_64/gdt.rs#L121)
* **Bottleneck**: During a context switch, the scheduler updates the kernel stack top (`RSP0`) in the Task State Segment (TSS) so that privilege transitions switch to the new task's kernel stack. It does this by calling `gdt::set_interrupt_stack(stack_top)`, which must acquire the global `CORE_GDT` spinlock.
* **Remedy**: Allocate GDT and TSS per-core (e.g., an array of `CoreGdt` structures indexed by LAPIC ID). During a context switch, a core only updates its own local TSS, avoiding any cross-core lock contention.

### 1.4 VFS Scheduler Lock Contention
* **Location**: [kernel/src/fs/vfs.rs](file:///home/kontsnor/Projects/KontsnorOS/kernel/src/fs/vfs.rs#L286-L307)
* **Bottleneck**: `resolve_relative_path` is called during every file lookup (`sys_open`, `sys_stat`, `sys_chdir`, etc.) to locate the process's current working directory (`cwd`). It does this by locking the global `SCHEDULER` to find the task by PID.
* **Remedy**: Access the current task's `cwd` using the lock-free CPU-local task pointer discussed in Section 1.2, eliminating scheduler lock operations on path resolution.

---

## 2. Virtual Memory Operations & TLB Latency

### 2.1 Fine-Grained TLB Shootdowns (IPI Storms)
* **Location**: [kernel/src/memory/virtual.rs](file:///home/kontsnor/Projects/KontsnorOS/kernel/src/memory/virtual.rs#L96)
* **Bottleneck**: Individual mapping functions (`map_user_page`, `unmap_user_page`, `update_user_page_flags`) invoke `shootdown_tlb()` on every single page mapped or unmapped. When user-space maps or unmaps multi-page buffers (e.g. 1 MiB mapping requires 256 pages), the kernel broadcasts 256 remote IPIs. Each broadcast waits synchronously on the APIC ICR register (`broadcast_ipi_all_excluding_self`), creating severe IPI storms and serializing all cores.
* **Remedy**:
  1. **Batched TLB Shootdowns**: Refactor block mapping APIs (`sys_mmap`, `sys_munmap`, `sys_mprotect`, `sys_brk`) to execute page table writes for the entire range *without* flushing the TLB or broadcasting IPIs on every page. Perform a single batched TLB shootdown (IPI broadcast) at the end of the syscall.
  2. **Deferred TLB Invalidation (Lazy Shootdowns)**: Invalidate page translations only when switching contexts or when entering user space if the page has not been accessed yet, or queue unmaps and broadcast shootdowns in batches when a threshold is met.

---

## 3. Syscall & Context Switch Overhead

### 3.1 Unnecessary MSR Accesses (`rdmsr`/`wrmsr`)
* **Location**: [kernel/src/process/context.rs](file:///home/kontsnor/Projects/KontsnorOS/kernel/src/process/context.rs#L161-L221)
* **Bottleneck**: `switch_context` performs three `rdmsr` reads and three `wrmsr` writes to save and restore `FS_BASE`, `GS_BASE`, and `KERNEL_GS_BASE` model-specific registers on every context switch. MSR access is highly expensive (hundreds of clock cycles per instruction).
* **Remedy**:
  1. **Skip `rdmsr` for `FS_BASE`**: Since `FS_BASE` is only modified by user-space via `sys_arch_prctl` (which stores the updated address in `task.context.fs_base`), the kernel already knows the active base value. Reading it with `rdmsr` during context switch is redundant.
  2. **Omit `GS` MSR Switches for Kernel-to-Kernel Switches**: Kernel `GS_BASE` (pointing to `CPU_SCRATCH`) is constant per CPU core. User `GS` is stored in `KERNEL_GS_BASE` while in Ring 0. There is no need to swap or read/write `GS_BASE` or `KERNEL_GS_BASE` unless we are switching between two distinct user-space threads.

### 3.2 Redundant Register Saving on Fast Syscalls
* **Location**: [kernel/src/syscall/mod.rs](file:///home/kontsnor/Projects/KontsnorOS/kernel/src/syscall/mod.rs#L129-L145)
* **Bottleneck**: The fast syscall entry handler `syscall_entry` pushes and pops all 15 general-purpose registers (including callee-saved registers `rbx`, `rbp`, `r12`-`r15`) on every single syscall.
* **Remedy**: Since the System V ABI defines `rbx`, `rbp`, `r12`-`r15` as callee-saved, the compiler preserves them across C/Rust calls. We only need to save them if the syscall yields or switches context. We can implement a "fast path" that only saves scratch registers (`rdi`, `rsi`, `rdx`, `rcx`, `r11`, `r10`, `r8`, `r9`, `rax`), saving callee-saved registers only in the "slow path" (context switch).

---

## 4. Hot-Path Memory Allocations

### 4.1 Temporary Heap Vectors in Syscall I/O
* **Location**: [kernel/src/syscall/fs.rs](file:///home/kontsnor/Projects/KontsnorOS/kernel/src/syscall/fs.rs#L115) and [L162](file:///home/kontsnor/Projects/KontsnorOS/kernel/src/syscall/fs.rs#L162)
* **Bottleneck**: To harden against TOCTOU and unsafe dereferencing, `sys_read` and `sys_write` allocate a temporary vector on the heap (`alloc::vec![0u8; count]`) on every call. This triggers heap allocator locks and fragmentation for every high-frequency console print, shell loop, and pipe read/write.
* **Remedy**: Use a stack-allocated buffer (e.g., `[u8; 4096]`) for I/O requests where `count <= 4096`. For larger requests, process the data in chunks of 4096 bytes using the stack buffer. This avoids heap allocations entirely, keeping memory safety and TOCTOU protection intact.

---

## 5. Security & Boundary Integrity Constraints

Any performance optimization must strictly conform to KontsnorOS's security boundaries:

1. **TOCTOU Protection**: Stack buffers (reusable/stack-buffered I/O) must be filled from user space once using a validated pointer before the data is processed. This prevents user-space threads from modifying arguments (such as pathname strings or parameters) after validation but before use.
2. **Register Cleansing**: Before returning from a context switch or exiting via `sysretq`, registers not used for return values must be cleared to `0` to prevent leaking Ring 0 kernel state to user-space Ring 3.
3. **Interrupt Safety**: All spinlocks, including per-core local queue locks or cache locks, must remain interrupt-safe (disabling and restoring interrupts on acquisition and release) to prevent deadlocks from hardware interrupt handlers.
