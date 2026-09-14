# KontsnorOS

<p align="center">
  <strong>A Unix & Linux ABI-Compatible Operating System Kernel Written in Rust</strong>
</p>

<p align="center">
  <em>Safe • 100% Self-Hosting • Linux ABI Compatible • Multi-Core SMP • Container-Ready</em>
</p>

---

[![License: GPLv3](https://img.shields.io/badge/license-GPLv3-blue.svg)](LICENSE)
[![Language: Rust](https://img.shields.io/badge/language-Rust-orange.svg)](https://www.rust-lang.org/)
[![Architecture: x86_64](https://img.shields.io/badge/arch-x86__64-green.svg)]()
[![SMP: Parallel Cores](https://img.shields.io/badge/SMP-8%2B%20Cores-brightgreen.svg)]()
[![Self--Hosting: 100% Native](https://img.shields.io/badge/Self--Hosting-100%25%20Achieved-success.svg)]()
[![Linux ABI: Arch & Alpine](https://img.shields.io/badge/Linux%20ABI-Arch%20%7C%20Alpine-blueviolet.svg)]()

> [!WARNING]
> **DISCLAIMER, DANGER, & UNCONTROLLED ESCALATION**
>
> ⚠️ **HIGHLY EXPERIMENTAL**: This project is highly experimental. **NO WARRANTY IS GIVEN.** If your machine starts playing Doom, becomes sentient, or attempts to rewrite reality, that is on you.
>
> 🚀 **HOW IT ESCALATED (A 3-MONTH ACCIDENT)**:
> This project was only supposed to be a quick test. The creator wanted to evaluate the capabilities of the **Antigravity IDE** paired with the **Gemini 3.5 Flash** model. The goal was simple: write a basic script or two and check out the new tooling.
>
> Somehow, due to a severe lack of impulse control and a hyper-collaborative AI, things escalated astronomically. Several months later, we have:
> - A custom **POSIX & Linux ABI-compatible hybrid OS kernel** with over 150+ implemented system calls.
> - **Full Native Self-Hosting**: The native Rust toolchain (`cargo`, `rustc`, `rust-lld`) compiles KontsnorOS inside KontsnorOS on top of our own custom syscall layer.
> - **Runs Unmodified Linux Userland**: Boots dynamic `glibc` & `musl` user-spaces, runs stock Arch Linux and Alpine Linux root filesystems, GNU Bash, Python, Coreutils, and our custom container runtime (`ctr_run`).
> - True Symmetric Multiprocessing (SMP) across 8+ parallel CPU cores with Local APIC timers, IPI preemption, and batched TLB shootdowns.
> - Full TCP/IP networking stack with Intel `e1000` Gigabit Ethernet DMA ring-buffers and BSD sockets.
> - Modern persistent Ext4 filesystem with extent trees, dirty page cache writeback, and mount-time self-healing FSCK.
> - Oh, and it also runs Doom. Because of course it does.
>
> *"I run Arch btw (in a namespace with my own kernel in QEMU on Ubuntu on WSL2 on Windows 11)."*

---

## Vision & Capabilities

KontsnorOS is a **hybrid kernel** combining the direct-hardware performance of a monolithic kernel with the modularity, memory safety, and concurrency guarantees of Rust. Its primary objective is to deliver **uncompromising, drop-in compatibility with the Linux Application Binary Interface (ABI)**, allowing unmodified Linux software and container distributions to run seamlessly bare-metal.

### 🌟 Key Highlights

- 🦀 **100% Rust-Powered**: Memory-safe kernel architecture with zero-cost abstractions, fine-grained locking, and zero compiler warnings.
- 🚀 **100% Native Self-Hosting Achieved**: The native toolchain (`cargo build --release`, `rustc`, `rust-lld` on `musl`) compiles all 18 dependency crates and links `kontsnor-kernel` from scratch natively within QEMU on KontsnorOS!
- 🐧 **Comprehensive Linux ABI (150+ Syscalls)**: Standard and advanced Linux syscalls implemented (`clone3`, `epoll`, `timerfd`, `signalfd4`, `eventfd2`, `inotify`, `pidfd`, `futex`, `sigaltstack`, `arch_prctl`, `pivot_root`, `unshare`, and full System V IPC / POSIX MQ).
- 📦 **Container Runtime & Linux Namespaces**: Native containerization engine (`ctr_run`) leveraging PID isolation, mount namespaces, `pivot_root`, and pseudo-filesystems (`devpts`, `procfs`, `sysfs`, `tmpfs`).
- 🍷 **Wine Compatibility Primitives**: Kernel primitives enabling Windows PE binary execution through Wine (`ARCH_SET_GS`/`ARCH_GET_GS`, `MAP_FIXED_NOREPLACE`, Linux `ucontext_t` / `siginfo_t` stack frame construction, and alternate signal stacks).
- ⚡ **True Parallel SMP Scheduling**: Dynamic ACPI core detection, Local APIC periodic timers, Inter-Processor Interrupts (IPIs) for preemption, lock-free CPU-local scratch registers (`gs:[16]`), and batched TLB shootdowns (Vector 36).
- 💾 **Persistent Ext4 Storage**: Native Ext4 extents support (`EXT4_FEATURE_INCOMPAT_EXTENTS`), 64-bit block numbers, physical sector allocation, persistent file writes across reboots, dirty page cache writeback, hard links, symlinks, fast atomic rename, and mount-time FSCK self-healing.
- 🌐 **DMA Gigabit Networking & TCP/IP Stack**: Intel `82540EM` (`e1000`) PCI driver with ring-buffer DMA, zero-delay interrupt scheduling, 512KB dynamic receive window scaling, out-of-order TCP segment reassembly, and full BSD socket API.
- 🖥️ **Interactive Terminal & PTY Subsystem**: Complete pseudo-terminal (`devpts`) driver, cooked (`ICANON`, `ECHO`, `ISIG`) and raw line discipline, process group signal routing (Ctrl+C / `SIGINT`), and session/job control (`TIOCSCTTY`, `TIOCSPGRP`).
- 🔌 **Driver SDK**: Safe, trait-based driver development framework for character, block, network, and graphics devices under GPLv3.
- 🎮 **Oh, It Also Runs Doom**: Naturally. With Bochs VBE modesetting, `/dev/fb0` Linux framebuffer support, and direct physical VRAM `mmap` backing, stock `fbdoom` runs right inside an Arch Linux container.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                             USER SPACE (Ring 3)                              │
│  ┌───────────────────────┐  ┌───────────────────────┐  ┌──────────────────┐ │
│  │   Arch Linux Rootfs   │  │   Native Rust Tools   │  │ GNU Bash / Shell │ │
│  │ glibc, pacman, python │  │  cargo, rustc, rust-lld│  │ coreutils, grep  │ │
│  └───────────┬───────────┘  └───────────┬───────────┘  └────────┬─────────┘ │
│              │                          │                       │           │
│              ▼                          ▼                       ▼           │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │             Linux Container Runtime (ctr_run & Namespaces)             │ │
│  └───────────────────────────────────┬────────────────────────────────────┘ │
├──────────────────────────────────────┼──────────────────────────────────────┤
│                                      ▼                                      │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                  POSIX & Linux Syscall Interface (150+)                │ │
│  │   Fast Path via gs:[16] │ Argument Validation │ ucontext_t Signal Frame │ │
│  └───────────────────────────────────┬────────────────────────────────────┘ │
│                                      │                                      │
│  ┌───────────────┬───────────────────┼───────────────────┬───────────────┐  │
│  │ SMP Scheduler │ Virtual Memory    │ Virtual File      │ Network Stack │  │
│  │ Multi-Core    │ 4-Level PML4      │ System (VFS)      │ e1000 Gigabit │  │
│  │ MLFQ Queues   │ MAP_SHARED / COW  │ Ext4 Extents/VFS  │ TCP/IP Engine │  │
│  │ Robust Futex  │ Page Cache & msync│ devpts / procfs   │ BSD Sockets   │  │
│  │ APIC & IPI    │ Batched Shootdown │ Bulk Slice Pipes  │ 512KB Window  │  │
│  └───────────────┴───────────────────┴───────────────────┴───────────────┘  │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                        Driver Framework (SDK)                          │ │
│  │     PCI Bus Enumeration │ Serial / TTY │ IDE/ATA Block │ Intel e1000   │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                             KERNEL SPACE (Ring 0)                           │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Getting Started

### Prerequisites

* **Rust** (nightly toolchain — automatically selected via `rust-toolchain.toml`)
* **QEMU** for emulation:
  ```bash
  sudo apt install qemu-system-x86
  ```
* **e2fsprogs** (provides `debugfs` and `mkfs.ext4` / `mke2fs` tools):
  ```bash
  sudo apt install e2fsprogs
  ```
* **musl-tools** (optional, for compiling static C helper binaries):
  ```bash
  sudo apt install musl-tools
  ```

### Building the Kernel

```bash
# Clone the repository
git clone https://github.com/kontsnor/KontsnorOS.git
cd KontsnorOS

# Build in debug mode
cargo build

# Build in release mode (recommended for SMP performance)
cargo build --release
```

### Running in QEMU

#### 1. Standard Kernel Boot (Release)
Launch the kernel natively in QEMU with full SMP and disk support:
```bash
./tools/run-qemu.sh --release
```

#### 2. Interactive Arch Linux Container
Experience unmodified Arch Linux running natively on top of KontsnorOS:
```bash
# Launches an interactive Arch Linux bash shell inside a container
./tools/run-arch-container.sh -i
```

#### 3. Automated Kernel & Syscall Test Suite
Run the automated integration and kernel test suite:
```bash
./tools/run-tests.sh
```

#### 4. Reproducing 100% Native Self-Hosting
Format the 6GB self-hosting disk containing the native Rust toolchain (`cargo`, `rustc`, `rust-lld`, musl headers, and kernel source tree):
```bash
./tools/format-disk.sh
./tools/run-qemu.sh --release
```
Inside the guest, run `cargo build --release` to compile KontsnorOS inside KontsnorOS!

### Kernel-Level Debugging with GDB

1. Start QEMU in GDB listening mode (freezes execution at the reset vector and listens on port `1234`):
   ```bash
   ./tools/run-qemu.sh --debug
   ```
2. In another terminal, connect with GDB:
   ```bash
   rust-gdb -ex "target remote :1234"
   ```

---

## Writing Drivers

KontsnorOS makes hardware driver development **safe, modular, and idiomatic**. The `driver-sdk` crate exposes versioned Rust traits for device categories:

```rust
use kontsnor_driver_sdk::*;

pub struct MyNetworkDriver {
    base_addr: u64,
}

impl NetDevice for MyNetworkDriver {
    fn send(&self, data: &[u8]) -> Result<(), DriverError> {
        // Transmit packet via hardware DMA registers
        Ok(())
    }

    fn recv(&self, buf: &mut [u8]) -> Result<usize, DriverError> {
        // Read received packet into buffer
        Ok(0)
    }

    fn mac_address(&self) -> [u8; 6] {
        [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]
    }

    fn info(&self) -> DriverInfo {
        DriverInfo {
            name: "my-net-driver".into(),
            version: "1.0.0".into(),
            author: "Contributor".into(),
            license: "GPL-3.0-only".into(),
            description: "Custom Network Driver".into(),
        }
    }
}
```

See [docs/driver-guide.md](docs/driver-guide.md) for the complete driver development guide.

---

## Project Structure

```
KontsnorOS/
├── kernel/                 # Ring 0 Kernel Crate
│   └── src/
│       ├── arch/           # x86_64 architecture (GDT, IDT, APIC, SMP trampoline, interrupts)
│       ├── memory/         # Virtual memory (PML4, COW, mmap, heap allocator, frame bitmap)
│       ├── process/        # Scheduler (MLFQ, tasks, contexts, futex, credentials, lifecycle)
│       ├── syscall/        # Linux ABI syscall dispatcher & 150+ system call handlers
│       │   ├── fs.rs       # File operations, stat, mount, xattr, epoll, inotify
│       │   ├── memory.rs   # mmap, mprotect, msync, mincore, mlock
│       │   ├── process/    # clone, execve, wait4, futex, info, creds
│       │   ├── signal.rs   # rt_sigaction, ucontext_t, sigaltstack, kill
│       │   ├── net.rs      # BSD socket syscall routing
│       │   └── ipc.rs      # System V IPC (shm, sem, msg) & POSIX MQ
│       ├── fs/             # Virtual File System (Ext4, devpts, procfs, sysfs, tmpfs, pipe)
│       ├── net/            # TCP/IP network stack (e1000 driver, ARP, IPv4, TCP, UDP, ICMP)
│       ├── drivers/        # Built-in drivers (serial, ATA/IDE, keyboard, PCI)
│       ├── sync/           # TicketLock, SpinMutex, WaitQueue synchronization primitives
│       └── ipc/            # Bulk ring-buffer pipe transfers
├── driver-sdk/             # Public freestanding driver development SDK
├── tools/                  # Launchers, disk builders, container runtime, and test harness
│   ├── ctr_run.c           # Container execution runtime (namespaces & pivot_root)
│   ├── init-arch.c         # Dedicated Arch Linux PID 1 container init daemon
│   ├── format-disk.sh      # Self-hosting disk builder (cargo, rustc, musl toolchain)
│   ├── run-arch-container.sh# Arch Linux container launcher
│   ├── run-qemu.sh         # QEMU emulator launch script
│   └── run-tests.sh        # Automated integration test runner
└── docs/                   # Technical documentation
    ├── architecture.md     # In-depth subsystem architecture & memory model
    ├── roadmap.md          # Strategic roadmap & ABI compatibility tracking
    ├── syscalls.md         # Exhaustive 150+ Linux syscall reference
    └── driver-guide.md     # Driver authoring and SDK tutorial
```

---

## Contributing

We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md) for coding guidelines, recursive lock safety rules, and pull request procedures.

### Active Development Areas

- 🖥️ **Native GUI & Display**: Bochs / VBE PCI framebuffer driver and desktop compositor.
- ⚡ **High-Speed Storage**: Direct NVMe and AHCI PCIe controller drivers.
- 🍷 **Wine Execution**: Validating Windows x86_64 PE applications running on top of Wine in KontsnorOS.
- 🛡️ **Syscall Hardening**: Continuous POSIX / Linux Test Project (LTP) conformance testing.

---

## License

KontsnorOS is licensed under the [GNU General Public License v3.0 (GPLv3 only)](LICENSE).

---

## Acknowledgments

- [Writing an OS in Rust](https://os.phil-opp.com/) by Philipp Oppermann
- [Rust OSDev](https://rust-osdev.com/) Community
- The Rust Programming Language Team
- The Linux Kernel & GNU Project
