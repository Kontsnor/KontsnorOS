# KontsnorOS Architecture & System Internals

## Overview

KontsnorOS is an advanced **hybrid kernel** operating system written entirely in Rust. It combines the performance characteristics and direct-hardware access of a monolithic kernel with the memory safety, modularity, and fault isolation guarantees of modern systems programming. The kernel provides strict **Linux Application Binary Interface (ABI) compatibility**, executing unmodified dynamically linked Linux distributions (such as Arch Linux and Alpine Linux), standard shells, and self-hosted compiler toolchains.

---

## Subsystem Architecture Map

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                             USER SPACE (Ring 3)                              │
│  ┌────────────────────────┐  ┌──────────────────────┐  ┌──────────────────┐ │
│  │   Arch Linux Rootfs    │  │  Self-Hosted Rustc   │  │ GNU Bash / Shell │ │
│  │ glibc, pacman, python  │  │ cargo, rustc, musl   │  │ coreutils, grep  │ │
│  └───────────┬────────────┘  └──────────┬───────────┘  └────────┬─────────┘ │
│              │                          │                       │           │
│              ▼                          ▼                       ▼           │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │          Container Runtime (ctr_run: namespaces, pivot_root)          │ │
│  └───────────────────────────────────┬────────────────────────────────────┘ │
├──────────────────────────────────────┼──────────────────────────────────────┤
│                                      ▼                                      │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                  POSIX & Linux Syscall Interface (150+)                │ │
│  │    Lock-Free Fast Path (gs:[16]) │ Argument Auditing │ ucontext_t Frame │ │
│  └───────┬───────────────────┬───────────────────┬───────────────────┬────┘ │
│          │                   │                   │                   │      │
│          ▼                   ▼                   ▼                   ▼      │
│  ┌───────────────┐   ┌───────────────┐   ┌───────────────┐   ┌────────────┐ │
│  │ SMP Scheduler │   │ Virtual Memory│   │ Virtual File  │   │ Network    │ │
│  │ 5-Level MLFQ  │   │ 4-Level PML4  │   │ System (VFS)  │   │ Stack      │ │
│  │ Multi-Core AP │   │ MAP_SHARED    │   │ Ext4 Extents  │   │ e1000 DMA  │ │
│  │ Robust Futex  │   │ Page Cache    │   │ Persistent FS │   │ TCP/IP     │ │
│  │ APIC Timers   │   │ Batched TLB   │   │ devpts / PTY  │   │ 512KB Win  │ │
│  │ IPI Shootdown │   │ COW Faults    │   │ Bulk Pipes    │   │ BSD Sockets│ │
│  └───────┬───────┘   └───────┬───────┘   └───────┬───────┘   └─────┬──────┘ │
│          │                   │                   │                 │        │
│          ▼                   ▼                   ▼                 ▼        │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                     Hardware Abstraction & Drivers                     │ │
│  │   PCI Enumerator │ Local APIC/IOAPIC │ IDE/ATA PIO │ Serial UART 16550 │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                             KERNEL SPACE (Ring 0)                           │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 1. Memory Model & Virtual Memory Manager (VMM)

### 1.1 Virtual Address Space Layout (x86_64 48-bit, 256 TiB)

```
0xFFFF_FFFF_FFFF_FFFF ┌─────────────────────────────────────────┐
                       │ Kernel Code & Data Segments             │
0xFFFF_FFFF_8000_0000  ├─────────────────────────────────────────┤
                       │ Kernel Dynamic Heap Allocator           │
0xFFFF_A000_0000_0000  ├─────────────────────────────────────────┤
                       │ Physical Memory Direct Map (PhysOffset) │
0xFFFF_0000_0000_0000  ├─────────────────────────────────────────┤
                       │ Non-canonical guard region              │
0x0000_8000_0000_0000  ├─────────────────────────────────────────┤
                       │ User Space (per-task address space)     │
                       │ - User Stack (grows down from 0x7FFF_*) │
                       │ - Dynamic Linker & Libraries (mmap)     │
                       │ - Process Heap (brk)                    │
                       │ - Program Text & Data (ELF segments)    │
0x0000_0000_0000_0000  └─────────────────────────────────────────┘
```

### 1.2 Higher-Half Physical Mapping
The kernel consolidates physical memory access at a fixed base:
`Mapping::FixedAddress(0xffff_a000_0000_0000)`. Any physical frame at address `P` is directly accessible in Ring 0 at `0xffff_a000_0000_0000 + P`, ensuring zero-overhead page table walks and DMA memory access.

### 1.3 Shared Memory & Page Cache Backing
- **`MAP_SHARED` Mappings**: Shared file-backed and anonymous mappings link multiple processes to identical physical memory frames. Modifications to shared pages are immediately visible across all mapping processes.
- **Page Cache & Dirty Sync**: File reads populate the unified page cache. File writes mark cache frames dirty, which are committed back to storage blocks during `msync`, `fsync`, `sync_all`, or kernel background flushing.
- **Copy-on-Write (COW)**: Pages shared across `fork` or dynamic library text are mapped read-only. A page fault (vector 14) with write violation triggers the COW handler, which allocates a new physical frame, copies the 4096-byte contents, and remaps it writable for the faulting task.

### 1.4 Batched TLB Shootdowns (Vector 36)
To avoid Inter-Processor Interrupt (IPI) storms when mapping or unmapping large multi-page buffers (such as dynamic shared libraries or 100MB toolchain heaps), KontsnorOS uses a batched protocol:
- Page table manipulations inside loops execute via `_no_shootdown` primitives (e.g., `map_user_page_no_shootdown`).
- At the syscall exit boundary, the core executes a single `shootdown_tlb()`, broadcasting an IPI (vector 36) across all other active cores and waiting synchronously on the APIC completion mask.

---

## 2. Processor Management, Concurrency & SMP

### 2.1 Multi-Core Bootstrapping
- **ACPI Table Parsing**: The bootstrap processor (BSP) parses ACPI MADT tables to identify all available LAPIC IDs.
- **Real-Mode AP Trampoline**: Secondary application processors (APs) are booted via the standard INIT-SIPI-SIPI sequence directed to real-mode trampoline code staged at physical address `0x8000`. The AP transitions through 16-bit real mode -> 32-bit protected mode -> 64-bit long mode before joining the scheduler.

### 2.2 CPU-Local Scratch Space (`CpuScratch`) & Fast Path
Each CPU core configures its `GS_BASE` Model-Specific Register (`0xC0000101`) to point to its dedicated slot in `CPU_SCRATCHES: [CpuScratch; 32]`:
```rust
#[repr(C, align(16))]
pub struct CpuScratch {
    pub user_rsp: u64,         // Offset 0
    pub kernel_rsp: u64,       // Offset 8
    pub current_pid: u64,      // Offset 16
    pub signals_pending: u64,  // Offset 24
}
```
**Lock-Free Fast Path**: Non-yielding syscalls like `sys_getpid`, `sys_getuid`, `sys_getppid`, and `sys_gettid` are dispatched directly in assembly or via `syscall_fast_dispatch`. The caller's active PID is fetched in a single clock cycle from `gs:[16]` without acquiring the global scheduler lock.

### 2.3 MLFQ Scheduler & Thread Model
- **5-Priority MLFQ**: Processes are organized across 5 dynamic priority queues. Interactive and I/O-bound tasks retain higher priority, while compute-intensive tasks are dynamically demoted.
- **Inter-Processor Interrupts (IPIs)**: Scheduler preemption and thread migration across SMP cores use Local APIC timer interrupts and scheduler IPIs (vector 35).
- **Thread Groups (`tgid`)**: Threads created via `sys_clone` sharing `CLONE_THREAD` belong to the same thread group. Terminating a process via `sys_exit_group` (231) cleans up all sibling threads.

### 2.4 Robust Futex Subsystem
KontsnorOS implements a rock-solid, production-grade futex engine supporting multi-threaded synchronization:
- Supported operations: `FUTEX_WAIT`, `FUTEX_WAKE`, `FUTEX_FD`, `FUTEX_REQUEUE`, `FUTEX_CMP_REQUEUE`, `FUTEX_WAKE_OP`, `FUTEX_WAIT_BITSET`, `FUTEX_WAKE_BITSET`.
- Robust list tracking via `sys_set_robust_list` and `sys_get_robust_list`, ensuring deadlocks are averted if a thread terminates while holding a user-space mutex.
- Modern vector wait: `sys_futex_waitv` (syscall 449) allowing tasks to wait concurrently on multiple futex words.

---

## 3. Syscall Interface & Signal Delivery

### 3.1 Linux ABI Calling Convention
Syscalls are invoked via the `syscall` instruction:
* **Registers**: `rax` (syscall number), `rdi` (arg0), `rsi` (arg1), `rdx` (arg2), `r10` (arg3), `r8` (arg4), `r9` (arg5).
* **Return**: `rax` returns values, with negative numbers in `[-4095, -1]` representing `-errno`.
* **State Preservation**: Callee-saved registers (`rbx`, `rbp`, `r12`-`r15`) are strictly preserved.

### 3.2 Linux Signal Frame Delivery (`ucontext_t`)
Signal handling matches the standard Linux x86_64 ABI:
1. When a signal is delivered to a task that has registered a custom handler via `sys_rt_sigaction`:
2. The kernel verifies stack bounds (using an alternate signal stack configured via `sys_sigaltstack` if enabled).
3. The kernel constructs a compliant `ucontext_t` and `siginfo_t` stack frame on the user stack, writing saved general-purpose registers, RFLAGS, RIP, and signal mask.
4. The kernel sets the user RIP to the signal handler function, points `rdi` to the signal number, `rsi` to `siginfo_t`, and `rdx` to `ucontext_t`, returning to Ring 3 via `sysretq`.
5. Upon handler completion, user-space invokes `sys_rt_sigreturn` (syscall 15), which restores the saved register context from the stack.

---

## 4. Virtual File System (VFS) & Persistent Storage

### 4.1 Ext4 Storage Architecture
- **Extents (`EXT4_FEATURE_INCOMPAT_EXTENTS`)**: Filesystem driver supports Ext4 extent trees, replacing indirect blocks with contiguous physical sector extents.
- **Persistent Writes**: File writes allocate physical data blocks via block bitmaps, update inode size, `mtime`, and `ctime`, and commit group descriptors to persistent disk blocks.
- **Hard Links & Fast Atomic Rename**: Supports multi-directory hard linking (`sys_linkat`) and POSIX atomic renames (`sys_renameat2`).
- **FSCK Self-Healing**: On volume mount, the filesystem driver performs validation of superblock magic (`0xEF53`), group descriptor bounds, and bitmap counts, repairing inconsistencies automatically.

### 4.2 Pseudo-Filesystems
- **`devpts`**: Dynamic virtual slave pseudo-terminal nodes (`/dev/pts/N`) created on demand by `/dev/ptmx`.
- **`procfs`**: Exposes `/proc/mounts`, `/proc/self`, `/proc/cpuinfo`, `/proc/meminfo`, and `/proc/stat`.
- **`sysfs`**: Exposes `/sys/class/net`, `/sys/devices`, and `/sys/fs/cgroup`.
- **`tmpfs`**: In-memory filesystem supporting directories, files, unlinking, and renaming.

### 4.3 High-Performance Bulk IPC Pipes
The kernel's `PipeBuffer` uses a ring-buffer design optimized with bulk slice copies (`push_slice` / `pop_slice` using `copy_from_slice`). This converts byte-by-byte loops into $O(1)$ SIMD/rep-movsb memory copies, dramatically boosting IPC throughput between piped utilities.

### 4.4 Event Subsystems
- **`epoll`**: `epoll_create1`, `epoll_ctl`, `epoll_wait`, `epoll_pwait`, `epoll_pwait2`. Wait queues allow cooperative task wake-ups across pipes, sockets, and character devices.
- **`timerfd`**: High-precision timers exposed as file descriptors.
- **`signalfd4`**: Signal event reception through standard read loops.
- **`eventfd2`**: Lightweight event counters.
- **`inotify`**: Directory and file watch event notifications.

---

## 5. High-Performance Network Stack

### 5.1 Intel e1000 Driver (82540EM)
- PCI device discovery and DMA initialization.
- Circular DMA ring-buffers for transmission (TX) and reception (RX).
- Zero-delay interrupt scheduling: Network packet arrival interrupts trigger an immediate scheduler rescheduling check, eliminating scheduling delays for networking tasks.

### 5.2 TCP/IP Protocol Pipeline
- **ARP & IPv4**: Resolution tables, routing, and checksum verification.
- **Dynamic 512KB Receive Window**: The TCP engine dynamically advertises up to 512KB receive windows, preventing flow control stalls during large file downloads.
- **Out-of-Order Segment Reassembly**: In-order, out-of-order, and retransmitted sequence segments are processed, buffered, and reassembled with sequence overlap trimming.
- **Proactive Window Updates**: When user-space drains bytes from socket receive buffers, immediate window-update ACKs are dispatched to remote peers.
- **BSD Socket API**: Complete syscall implementation: `socket`, `bind`, `connect`, `listen`, `accept`, `accept4`, `sendto`, `recvfrom`, `sendmsg`, `recvmsg`, `sendmmsg`, `recvmmsg`, `shutdown`, `getsockname`, `getpeername`, `socketpair`, `setsockopt`, `getsockopt`.

---

## 6. Terminal Subsystem & Interactive Job Control

### 6.1 PTY Master/Slave Architecture
- Master devices (`/dev/ptmx`) allocate slave terminal endpoints (`/dev/pts/N`).
- Input written to the master appears on slave read, and output written to the slave appears on master read.

### 6.2 Line Discipline & Termios
- **36-Byte Linux ABI Termios**: Strictly matches the standard Linux `Termios` size (`NCCS = 19`) to prevent user-space stack corruption.
- **Canonical vs. Raw**: Supports line buffering with editing (backspace) and echo, as well as raw character-by-character pass-through.
- **Job Control & Signals**: Processes manage sessions via `setsid` and acquire controlling terminals via `ioctl(TIOCSCTTY)`. Ctrl+C (0x03) translates to `SIGINT` broadcast to the controlling process group (`TIOCSPGRP`), deduplicated by thread group (`tgid`) to eliminate sibling teardown races.

---

## 7. Containerization & Linux Namespaces

KontsnorOS features a native container execution engine (`ctr_run`):
- **Mount Namespace & `pivot_root`**: Shifts the active root filesystem to container images (e.g. `/containers/arch`), hiding host filesystems.
- **PID Virtualization**: Provides container-scoped PID tracking and init process behavior.
- **Container Init Daemon (`init-arch`)**: Dedicated PID 1 init daemon managing child processes, reaping zombies, mounting pseudo-filesystems, and launching shells.

---

## 8. Wine Compatibility Primitives

To enable running Windows x86_64 binaries through Wine:
- **Segment Register Manipulation**: Full support for `ARCH_SET_GS` and `ARCH_GET_GS` in `sys_arch_prctl`, allowing Wine's Thread Information Block (TIB) to reside in `GS_BASE`.
- **Collision-Free Address Allocation**: `MAP_FIXED_NOREPLACE` prevents memory collisions during Windows PE image base relocations.
- **Alternate Signal Stacks**: `sys_sigaltstack` allows Wine exception handlers to run even after user stack exhaustion.
- **Full `ucontext_t` Frames**: Wine's structured exception handling (SEH) relies on inspecting and modifying saved processor registers in `ucontext_t`.
