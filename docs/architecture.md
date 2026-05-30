# KontsnorOS Architecture

## Overview

KontsnorOS is a **hybrid kernel** operating system written entirely in Rust.
It combines the performance characteristics of a monolithic kernel with the
modularity and fault isolation benefits of a microkernel.

## Kernel Architecture

### Core Subsystems (Ring 0)

These run with full kernel privileges for maximum performance:

- **Process Scheduler** — Multi-Level Feedback Queue (MLFQ) with 5 priority levels
- **Virtual Memory Manager** — 4-level page table management, demand paging
- **Interrupt Handler** — IDT-based interrupt dispatching with IST stacks
- **Core IPC** — Pipes, signals, Unix domain sockets

### Modular Components

These are loadable and can be replaced or extended:

- **File Systems** — VFS layer with pluggable filesystem drivers
- **Device Drivers** — Trait-based driver model with stable SDK
- **Network Stack** — (planned) TCP/IP stack with modular protocols

## Memory Model

```
Virtual Address Space (48-bit, 256 TiB):

0xFFFF_FFFF_FFFF_FFFF ┌─────────────────────┐
                       │   Kernel Code/Data   │
0xFFFF_FFFF_8000_0000  ├─────────────────────┤
                       │    Kernel Heap       │
0xFFFF_8000_0000_0000  ├─────────────────────┤
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

## Syscall Interface

KontsnorOS implements the POSIX syscall interface using the `syscall` instruction:

- **Registers**: rdi, rsi, rdx, r10, r8, r9 for arguments
- **Return**: rax for result
- **Numbering**: Linux-compatible syscall numbers

## Driver Model

The driver framework uses Rust traits for type-safe hardware abstraction:

```
             ┌─────────────────────────────┐
             │       Driver SDK (public)    │
             │  CharDevice │ BlockDevice    │
             │  NetDevice  │ GpuDevice      │
             └──────────────┬──────────────┘
                            │
             ┌──────────────┼──────────────┐
             │       Driver Framework       │
             │  Registration │ Lifecycle    │
             │  Bus Matching │ IRQ Routing  │
             └──────────────┬──────────────┘
                            │
             ┌──────────────┼──────────────┐
             │       Bus Abstraction        │
             │    PCI │ USB │ Platform      │
             └─────────────────────────────┘
```
