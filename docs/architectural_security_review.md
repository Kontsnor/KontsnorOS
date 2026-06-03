# Architectural and Security Review of KontsnorOS

This document presents a rigorous and detailed architectural and security review of the **KontsnorOS** bare-metal `x86_64` POSIX/UNIX-compatible kernel written in Rust.

---

## 1. Executive Summary & Architecture Map

### 1.1 Executive Summary
KontsnorOS is a bare-metal, Unix-compatible hybrid kernel written in Rust. The kernel operates in Ring 0, while supporting loadable drivers and Ring 3 user-space processes. 

The bootstrapping process utilizes the modern `bootloader_api` crate (which handles UEFI/BIOS boots, sets up an early GDT, identity maps the kernel, and passes a `BootInfo` memory map). The kernel initializes basic architecture setups (GDT, IDT, PIC, APIC, SMP), sets up physical and virtual memory managers, establishes a global dynamic heap allocator, mounts VFS filesystems (including a block-backed writable `ext2` implementation), and enters a Multi-Level Feedback Queue (MLFQ) scheduler to run kernel and Ring 3 user-space threads.

While the kernel demonstrates high maturity in its filesystem implementation (a complete writable `ext2` driver with mount-time FSCK self-healing consistency checks) and robust syscall argument validation, it suffers from several severe architectural gaps and resource leaks. Specifically:
1. **Physical Memory Leakage:** Page tables and kernel stacks are never reclaimed when processes exit or undergo `execve`.
2. **Incomplete SMP Bootstrapping:** Although ACPI tables are parsed and the CPU list is enumerated, the secondary cores (APs) are never booted.
3. **Concurrency Race Hazards:** The `CPU_SCRATCH` base and `CORE_GDT` TSS mappings are designed globally, which would cause catastrophic stack corruption and data leaks under multi-core execution.

### 1.2 Subsystem Interaction Map
The following text-based block diagram illustrates how the core subsystems of the KontsnorOS kernel interact:

```text
                 +--------------------------------------------+
                 |          Ring 3 User Applications          |
                 |       (e.g., kontsnorsh shell in C)        |
                 +----------------------+---------------------+
                                        |
                            [syscall / sysretq]
                                        v
                 +----------------------+---------------------+
                 |           Syscall Dispatch Layer            |
                 |      (Validated User Pointers & Copy)      |
                 +----+-----------------+----------------+----+
                      |                 |                |
                      v                 v                v
           +----------+-------+  +------+------+  +------+----------+
           |   Process/Sched  |  | Memory (VM) |  |   VFS Interface  |
           | (MLFQ Scheduler) |  | (COW Page   |  | (Inodes, Fds,   |
           |  (TSS Stack Set) |  |   Faults)   |  |  Pipe Buffers)  |
           +----------+-------+  +------+------+  +------+----------+
                      |                 |                |
                      |                 v                v
                      |          +------+------+  +------+----------+
                      |          |   Physical  |  |  FS Drivers &   |
                      |          |  Allocator  |  |  Block Devices  |
                      |          |  (Bitmaps)  |  | (ext2, ATA PIO) |
                      |          +-------------+  +------+----------+
                      |                                  |
                      +-----------------+----------------+
                                        v
                 +--------------------------------------------+
                 |            x86_64 Hardware/CPU             |
                 |    (GDT, IDT, APIC Timer, LAPIC, Serial)   |
                 +--------------------------------------------+
```

---

## 2. Low-Level Component Audit

### 2.1 Memory Management (MM)

*   **Current Implementation:**
    *   **Physical Allocator (`memory::physical`):** Uses a bitmap-based allocation strategy. A static array of `AtomicU8` values (`FRAME_REFS`) tracks physical page frame reference counts (supporting up to 4 GiB of memory). Local core caches (`CORE_CACHES`) act as thread-local / core-local frame caches (up to 16 frames per core) to reduce lock contention on the global `FRAME_ALLOCATOR` (protected by a `TicketLock`).
    *   **Virtual Memory (`memory::virtual`):** Uses the `x86_64` crate to manipulate 4-level page tables via `OffsetPageTable`. Page mappings and flag updates are flushed and broadcasted via TLB shootdown interrupts.
    *   **Heap Allocator (`memory::heap`):** Wraps `linked_list_allocator::LockedHeap` over a pre-mapped 64 MiB virtual address range starting at `0xFFFF_8000_0000_0000`.
*   **Rust Idiom & Safety Score: 7.5 / 10**
    *   *Strengths:* Solid use of `no_std` structures, ticket locks, and atomics for reference counting. Unsafe blocks are isolated and logical invariants are generally checked.
    *   *Weaknesses:* `active_page_table()` returns an `OffsetPageTable<'static>`. This represents a static mutable reference to the kernel page table, which violates Rust's unique access rules if multiple references are held concurrently or across different cores.
*   **POSIX Compliance Gaps:**
    *   `sys_mmap` supports basic anonymous private and private file-backed mappings. However, shared mappings (`MAP_SHARED`) are not supported.
    *   `sys_mprotect` is implemented, but there is no verification of memory boundary overlaps or page permissions against POSIX standard limits.

### 2.2 Scheduling & Process Management

*   **Current Implementation:**
    *   **Task State & TCB (`process::task`):** A `Task` struct represents a thread of execution, containing the saved `CpuContext` (general-purpose registers, RFLAGS, CR3, and MSR `FS_BASE`), page table root, kernel stack bounds, file descriptor table, program break (`brk`), and signal sets.
    *   **MLFQ Scheduler (`process::scheduler`):** Features 5 priority levels (`RealTime`, `High`, `Normal`, `Low`, `Idle`). Tasks are preempted when their time slice (`TIME_QUANTUM = 10` ticks) expires, resulting in priority demotion. Starvation is prevented by a periodic priority boost (`BOOST_INTERVAL = 1000` ticks).
    *   **Context Switch (`process::context`):** Naked function `switch_context` saves callee-saved registers into the old task context and loads them from the new task context, updating CR3 and `FS_BASE` MSR (`0xC0000100`).
*   **Rust Idiom & Safety Score: 6.8 / 10**
    *   *Strengths:* Dynamic context switching via naked assembly and System V ABI register saving. Tasks are boxed, ensuring stable context pointers on the heap.
    *   *Weaknesses:* Serious memory safety design issues regarding resource cleanup. The scheduler simply takes a task out of the `tasks` vector and drops it. Since the kernel stack (allocated via raw `alloc`) and PML4 page directory pages are raw integers, they are completely leaked.
*   **POSIX Compliance Gaps:**
    *   Process lifecycles (`sys_fork`, `sys_execve`, `sys_exit`, `sys_wait4`) are supported but with shortcuts:
        *   `sys_wait4` blocks parent tasks cooperatively by scanning all tasks in a loop, rather than sleeping on a dedicated condition variable or non-polling wait queue.
        *   `sys_exit` does not properly re-parent orphan processes to PID 1 (init), creating potential zombie memory leak pools.

### 2.3 Interrupts & Exception Handling

*   **Current Implementation:**
    *   **IDT Setup (`arch::x86_64::interrupts`):** Configures CPU exceptions and hardware interrupts. Double fault and Page fault handlers use dedicated Interrupt Stack Tables (ISTs) to avoid kernel stack overflows.
    *   **Page Fault Handler:** Implements Copy-on-Write (COW) logic. If a write exception occurs on a page with `BIT_9` (COW flag) set:
        *   If the physical frame reference count is 1, it is marked writable directly.
        *   If the reference count is > 1, a new physical frame is allocated, the page contents are copied, the reference count of the old page is decremented, and the mapping is updated to writable.
    *   **Timer & Preemption:** The periodic LAPIC timer fires Vector 32. The timer handler calls `scheduler::tick()` and triggers cooperative preemption by calling `scheduler::schedule()`.
*   **Rust Idiom & Safety Score: 8.5 / 10**
    *   *Strengths:* Solid interrupt gates, ticket locks wrapping critical sections, and proper interrupt disabling inside locks to prevent deadlocks.
    *   *Weaknesses:* The COW page fault handler modifies page tables directly without acquiring a virtual memory manager lock. While currently running on a single core, this would cause race conditions and page table corruption on multi-core configurations.
*   **POSIX Compliance Gaps:**
    *   Signals are delivered to processes on syscall return or rescheduling. Standard POSIX signal actions are supported via `rt_sigaction` and `rt_sigprocmask`, but there is no concept of process groups or terminal job control signal delivery.

### 2.4 The VFS & POSIX Abstraction Layer

*   **Current Implementation:**
    *   **VFS (`fs::vfs`):** Resolves absolute and relative paths, matches the longest mount point prefix, and routes operations to filesystem-specific `InodeOps` implementations (`devfs`, `tmpfs`, `procfs`, `ext2`).
    *   **File Descriptors (`process::fd`):** Open files are tracked in `Task::fd_table` as `Vec<Option<Arc<FileDescription>>>`. Multiple descriptors can point to the same `FileDescription` (sharing seek offset and flags) to satisfy POSIX offset sharing requirements across `dup2` and `fork`.
    *   **Syscall Dispatch (`syscall::mod`):** Uses the `syscall` CPU instruction. User-space pointers are validated against user-space memory limits (`0x0000_7FFF_FFFF_FFFF`) and verified for valid mapping in the active page directory before copy operations.
*   **Rust Idiom & Safety Score: 8.8 / 10**
    *   *Strengths:* Excellent separation of file descriptors from file descriptions. Path validation and user pointer validation are outstandingly secure and robust against buffer overflows and memory read attacks.
    *   *Weaknesses:* The `FdTable` struct defined in `fs/file.rs` is completely bypassed. `Task` implements its own raw vector of descriptors, representing redundant architectural debt.
*   **POSIX Compliance Gaps:**
    *   Writing to a pipe with no readers (`EPIPE` error) does not deliver the `SIGPIPE` signal to the process.
    *   Directory structures are stored as simple files in the VFS layer, lacking directory caching or `dentry` state structures.

---

## 3. Critical Vulnerability & Debt Log

| ID | Category | Subsystem | Description & Technical Debt | Severity |
| :--- | :--- | :--- | :--- | :--- |
| **V-01** | Resource Leak | Process / VM | **Page Table & Stack Leakage on Exit/Execve:** When a process exits (`sys_exit`/`wait4`) or performs `execve`, its kernel stack (allocated via raw `alloc`) and its 4-level page table frames (PML4, PDPT, PD, PT) are never deallocated. This leads to a severe physical memory leak on every process lifecycle. | **CRITICAL** |
| **V-02** | Concurrency | Boot / SMP | **APs (Application Processors) are Not Booted:** Although `CpuManager` parses the ACPI MADT table and lists all logical cores, the secondary cores (APs) are never booted. No INIT or STARTUP IPI sequence is broadcasted. The kernel executes entirely in single-core mode. | **HIGH** |
| **V-03** | Concurrency | Syscall / SMP | **Global `CPU_SCRATCH` Race Condition:** The `CPU_SCRATCH` structure used to swap stack pointers during fast syscall transitions (`syscall_entry` assembly) is a single global static variable. If APs were booted, concurrent syscalls on separate cores would overwrite each other's saved `user_rsp`/`kernel_rsp`, corrupting stacks. | **HIGH** |
| **V-04** | Concurrency | GDT / SMP | **Global `CORE_GDT` Race Condition:** `CORE_GDT` is a single global lock storing only one `CoreGdt` instance (and one TSS). If multiple cores switch contexts, they will overwrite the same TSS's `privilege_stack_table[0]` field. Under multi-core execution, this will cause page faults during Ring 3 to Ring 0 transitions. | **HIGH** |
| **V-05** | POSIX Gap | Syscall / IPC | **Missing `SIGPIPE` Delivery:** Writing to a pipe with all readers closed returns `EPIPE` (-32), but does not raise the `SIGPIPE` signal. Standard shell pipelines depend on this signal to terminate upstream commands. | **MEDIUM** |
| **V-06** | Concurrency | FS / TTY | **Lockless Stdin Serial Port Reads:** `DevStdin::read` polls `serial::try_read_byte` without holding a device-level lock. If multiple processes open `/dev/stdin` separately, they will race to read character inputs, leading to corrupted text stream reads. | **MEDIUM** |
| **V-07** | Tech Debt | Syscall | **Redundant Syscall Placeholders:** `syscall/io.rs` contains stub implementations of `sys_pipe`, `sys_dup`, and `sys_dup2` that return `ENOSYS`. The actual working implementations reside in `syscall/fs.rs` and are routed by the dispatcher. | **LOW** |

---

## 4. Verification & Testing History

During the audit, we examined the serial console logs (`qemu_output.log`) from a test run. The kernel successfully booted and initialized the following subsystems:
1. **GDT, IDT, PIC, APIC, SMP:** remap/disable PIC, enable Local APIC ID 0, set up periodic timer interrupts.
2. **Memory:** Map physical memory, initialize the linked-list heap allocator (64 MiB).
3. **VFS:** Mount `devfs`, `tmpfs`, `procfs`, and the `ext2` RAM disk.
4. **Multitasking:** Successfully spawned two kernel threads (`demo_1`, `demo_2`) which cooperatively yielded execution slices.
5. **Ring 3 Launch:** Successfully cloned the page table and launched the freestanding C shell `/bin/sh` (PID 5).
6. **Shell Verification:** Interactive commands, pipes, and redirects successfully executed, but termination happened via `SIGTERM` (QEMU shutdown) rather than a clean native shutdown.

---
*End of Audit Report.*
