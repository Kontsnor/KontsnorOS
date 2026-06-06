# KontsnorOS Architecture

## Overview

KontsnorOS is a **hybrid kernel** operating system written entirely in Rust. It combines the performance characteristics of a monolithic kernel with the modularity and fault isolation benefits of a microkernel. The system is designed to provide secure, POSIX-compatible multi-tasking for user space applications.

---

## Kernel Architecture

### Core Subsystems (Ring 0)

These run with full kernel privileges for maximum performance:

- **Process Scheduler** — Multi-Level Feedback Queue (MLFQ) with 5 priority levels, executing tasks across Symmetric Multiprocessing (SMP) cores.
- **Virtual Memory Manager** — 4-level page table management (PML4), demand paging, copy-on-write (COW) page translation, and remote TLB shootdowns.
- **Interrupt Handler** — IDT-based interrupt dispatching with dedicated Interrupt Stack Tables (ISTs) for fail-safe exception recovery.
- **Core IPC & Signal Engine** — Wait queues, POSIX signal delivery/masking, and pipes (SIGPIPE notifications).

### Modular Components

These are loadable and can be replaced or extended:

- **File Systems** — VFS layer with pluggable filesystem drivers (including a writable `ext2` implementation).
- **Device Drivers** — Trait-based driver model with stable SDK (bridging console, ATA drives, and peripherals).
- **Network Stack** — e1000 PCI driver, TCP/IP protocol stack, and BSD socket layer interface.

---

## Memory Model

```
Virtual Address Space (48-bit, 256 TiB):

0xFFFF_FFFF_FFFF_FFFF ┌─────────────────────┐
                       │   Kernel Code/Data   │
0xFFFF_FFFF_8000_0000  ├─────────────────────┤
                       │    Kernel Heap       │
0xFFFF_A000_0000_0000  ├─────────────────────┤
                       │  Physical Memory Map │
                       │  (direct mapping)    │
0xFFFF_0000_0000_0000  ├─────────────────────┤
                       │     (unused)         │
                       │                     │
0x0000_8000_0000_0000  ├─────────────────────┤
                       │   User Space         │
                       │  (per-process)       │
0x0000_0000_0000_0000  └─────────────────────┘
```

### Physical Memory Mapping

The kernel's physical memory mapping layout is consolidated at a fixed mapping base: `Mapping::FixedAddress(0xffff_a000_0000_0000)`. This transition from `Mapping::Dynamic` guarantees a stable higher-half direct mapping layout for physical frame translation in the Virtual Memory Manager.

### Thread Local Storage (TLS)
User-space thread-local storage is supported by saving and restoring the CPU's `FS_BASE` model-specific register (`0xC0000100`) during context switches. Child threads created via `sys_fork` and `sys_clone` inherit their parent's TLS register settings if not explicitly overridden by TLS configuration flags.

---

## Syscall Interface

KontsnorOS implements the POSIX system call interface using the `syscall` / `sysretq` instructions.

- **Registers**: `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9` for arguments.
- **Return**: `rax` for results (negative values indicate standard error codes).
- **Numbering**: Linux/x86_64 compatible syscall numbers (e.g. `clone` = 56, `fork` = 57, `execve` = 59, `exit_group` = 231).

### Syscall Memory Hardening

System calls are hardened at the kernel boundary to protect against unsafe user pointer dereferencing and TOCTOU vulnerabilities:
- **Buffer Hardening**: Core system calls (`sys_read`, `sys_write`, `sys_poll`, `sys_pread64`, and `sys_writev`) copy buffers into local kernel-allocated vectors (`alloc::vec![0u8; count]`) using `copy_nonoverlapping` before performing operations.
- **Pointer Validation**: Arguments containing user pointers (e.g., child stack and TID pointers in `sys_clone`, `wstatus` in `sys_wait4`) are validated via `validate_user_ptr` and `validate_user_ptr_write` before execution.

### Socket Layer System Calls

BSD socket layer routing is wired through the syscall interface under the `net` module:
- **`socket` (41)**: Creates a communications endpoint.
- **`connect` (42)**: Establishes a connection (initiates TCP handshake or binds UDP remote endpoints).
- **`accept` (43)**: Accepts a connection from a passive listening queue.
- **`sendto` (44)**: Transmits data to a destination address.
- **`recvfrom` (45)**: Receives data and records its source address.
- **`bind` (49)**: Binds a local IPv4 address and port.
- **`listen` (50)**: Places the socket in a passive listening state.

---

## Virtual File System (VFS)

The VFS layer manages filesystem dispatch and paths resolution.

### Filesystem Interface Consistency

Filesystem drivers (such as `ext2` and `devfs`) implement the pluggable `FileSystem` trait. The `FileSystem::root` signature returns `Option<Arc<dyn InodeOps>>`, ensuring that unmounted or uninitialized roots are gracefully handled:
```rust
pub trait FileSystem: Send + Sync {
    fn root(&self) -> Option<Arc<dyn InodeOps>>;
    fn name(&self) -> &str;
    // ...
}
```

### ext2 Metadata & Mount Safety

The `ext2` driver implements strict verification checks during volume mount to prevent integer overflows and directory index compromises:
- **Superblock Validation**: Asserts magic number (`0xEF53`), log block size, and verifies that the total inodes and blocks counts are non-zero.
- **GDT Boundary Checks**: Validates that Group Descriptor Table metadata blocks (block bitmap, inode bitmap, and inode table blocks) lie within physical filesystem boundaries.
- **Reserved Blocks Calculation**: Protects against integer overflows during calculations of reserved blocks count.
- **Self-Healing Checks (FSCK)**: Traces allocated blocks and inodes to compare actual bitmaps against metadata and dynamically resolves inconsistencies.

---

## Process & Thread Model

A `Task` struct represents a single execution context. The kernel groups tasks according to:
- **PID**: Unique process identifier.
- **PGID (Process Group ID)**: Shared by processes in the same pipeline or job session, allowing group-wide signal dispatching.
- **Forking/Cloning**: Created via `sys_fork` (duplicating address spaces with COW) or `sys_clone` (specifying custom stack boundaries and TLS contexts).

---

## Security & Multi-User Architecture (Roadmap)

To support a full distribution model, the security and credential architecture is planned to transition from a single-user model to:
- **Enforced File Credentials**: Inode properties (`uid`, `gid`, `mode`) will be checked at the VFS layer for permissions validation.
- **Process Credentials**: Proper implementation of real and effective IDs (UID/GID), restricting privileged system calls to processes running with effective user ID `0` (root).
- **Safe privilege transitions**: `setuid` binary execution flags on `execve` will permit standard user tasks to assume root privileges for specific operations.

---

## Shared Memory & Dynamic Linker (Roadmap)

To reduce disk and RAM footprints, the OS will evolve to support:
- **Dynamic ELF Loader**: Dynamic loading of shared object (`.so`) libraries via `/lib/ld-kontsnoros.so`.
- **Shared Memory Mappings**: Memory mappings initialized with `MAP_SHARED` will be tracked by the virtual memory manager, linking multiple processes to identical physical frames for efficient IPC and library sharing.
