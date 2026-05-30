# KontsnorOS

<p align="center">
  <strong>A Unix-Compatible Operating System Kernel Written in Rust</strong>
</p>

<p align="center">
  <em>Safe • Fast • Modern • Driver-Friendly</em>
</p>

---

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)]()
[![Language: Rust](https://img.shields.io/badge/language-Rust-orange.svg)]()
[![Architecture: x86_64](https://img.shields.io/badge/arch-x86__64-green.svg)]()

## Vision

KontsnorOS is a **hybrid kernel** that combines the performance of a monolithic kernel with the modularity and safety of a microkernel — all written in Rust.

### Key Features

- 🦀 **100% Rust** — Memory-safe kernel with zero-cost abstractions
- 🐧 **Unix/POSIX Compatible** — Standard syscall interface (fork, exec, open, read, write, ...)
- 🔌 **Driver-Friendly SDK** — Permissively licensed SDK inviting companies like NVIDIA and AMD
- ⚡ **Hybrid Architecture** — Monolithic performance with modular driver loading
- 🔒 **Safe by Default** — Rust's borrow checker prevents data races and memory bugs
- 📦 **Modern Tooling** — Cargo-based build system, integrated testing

## Architecture

```
┌─────────────────────────────────────────────┐
│              User Space                      │
│  ┌──────┐  ┌──────┐  ┌──────────────────┐  │
│  │ Apps │  │Shell │  │ System Daemons   │  │
│  └──┬───┘  └──┬───┘  └──────┬───────────┘  │
│     │         │              │               │
├─────┼─────────┼──────────────┼───────────────┤
│     ▼         ▼              ▼               │
│  ┌─────────────────────────────────────────┐ │
│  │        POSIX Syscall Interface          │ │
│  └─────────────────────────────────────────┘ │
│  ┌──────────┐ ┌──────┐ ┌────────┐ ┌──────┐ │
│  │Scheduler │ │ VMM  │ │  VFS   │ │ IPC  │ │
│  └──────────┘ └──────┘ └────────┘ └──────┘ │
│  ┌─────────────────────────────────────────┐ │
│  │         Driver Framework (SDK)          │ │
│  │   PCI Bus │ GPU │ Block │ Net │ Char   │ │
│  └─────────────────────────────────────────┘ │
│              Kernel Space                    │
└─────────────────────────────────────────────┘
```

## Getting Started

### Prerequisites

- **Rust** (nightly toolchain — automatically selected via `rust-toolchain.toml`)
- **QEMU** for testing: `sudo apt install qemu-system-x86`

### Building

```bash
# Clone the repository
git clone https://github.com/kontsnor/KontsnorOS.git
cd KontsnorOS

# Build the kernel
cargo build

# Build in release mode
cargo build --release
```

### Running in QEMU

```bash
# Run with serial output to terminal
./tools/run-qemu.sh
```

## Writing Drivers

KontsnorOS is designed to make driver development **easy and safe**. The `driver-sdk` crate provides stable, well-documented APIs:

```rust
use kontsnor_driver_sdk::*;

pub struct MyGpuDriver;

impl GpuDevice for MyGpuDriver {
    fn init_hw(&self) -> Result<(), DriverError> {
        // Initialize your GPU hardware
        Ok(())
    }

    fn get_display_info(&self) -> Vec<DisplayInfo> {
        // Return connected displays
        vec![]
    }

    // ... implement other required methods
}
```

See [docs/driver-guide.md](docs/driver-guide.md) for the full driver development guide.

## Project Structure

```
KontsnorOS/
├── kernel/          # The kernel crate
│   └── src/
│       ├── arch/    # Architecture-specific code (x86_64)
│       ├── memory/  # Memory management (physical, virtual, heap)
│       ├── process/ # Process management & scheduling
│       ├── syscall/ # POSIX syscall interface
│       ├── fs/      # Virtual File System
│       ├── drivers/ # Driver framework & built-in drivers
│       ├── sync/    # Synchronization primitives
│       └── ipc/     # Inter-Process Communication
├── driver-sdk/      # Public driver development SDK
├── tools/           # Build & test utilities
└── docs/            # Documentation
```

## Contributing

We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

### Areas Where We Need Help

- 🎮 **GPU drivers** — NVIDIA, AMD, Intel
- 🌐 **Network drivers** — Ethernet, WiFi
- 💾 **Filesystem implementations** — ext4, btrfs, FAT32
- 🖥️ **User-space** — Shell, coreutils, package manager
- 📚 **Documentation** — Tutorials, architecture docs

## License

KontsnorOS is dual-licensed under:

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

You may choose either license. This permissive licensing is intentional — we want companies to be able to contribute drivers without copyleft concerns.

## Acknowledgments

- [Writing an OS in Rust](https://os.phil-opp.com/) by Philipp Oppermann
- [Rust OSDev](https://rust-osdev.com/) community
- The Rust programming language team
