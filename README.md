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

> [!WARNING]
> **DISCLAIMER & EXPERIMENTAL STATUS**
>
> ⚠️ **HIGHLY EXPERIMENTAL & UNTESTED**: This project is highly experimental and is not properly tested whatsoever. **NO WARRANTY WILL BE GIVEN WHATSOEVER.**
>
> 🚀 **ORIGIN STORY**: The creator of this project wanted to test the limits of the new **Antigravity IDE** with the new **Gemini 3.5 Flash** (and now **Gemini 3.6**), and accidentally built a kernel that appears to be (kind of) working!

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

*   **Rust** (nightly toolchain — automatically selected via `rust-toolchain.toml`)
*   **QEMU** for emulation: `sudo apt install qemu-system-x86`
*   **e2fsprogs** (provides `debugfs` and `mkfs.ext2` tools): `sudo apt install e2fsprogs`

### Building

To compile the kernel:

```bash
# Clone the repository
git clone https://github.com/kontsnor/KontsnorOS.git
cd KontsnorOS

# Build the kernel in debug mode
cargo build

# Build the kernel in release mode (recommended)
cargo build --release
```

### Formatting the Persistent Disk & Importing Binaries

KontsnorOS mounts a persistent Ext2 disk image (`disk.img`) at boot. You can create this image, format it, and copy binaries (such as static GNU Bash or custom shells) directly from your host system using `debugfs` — without requiring host-level loopback mounts or `root`/`sudo` privileges:

```bash
# 1. Create a blank 16MB file for the disk image
dd if=/dev/zero of=disk.img bs=1M count=16

# 2. Format the disk image with an Ext2 filesystem (1024-byte block size)
mkfs.ext2 -b 1024 -F disk.img

# 3. Create folders and write host binaries to the image using debugfs
# Import GNU Bash and standard shell executable to /bin/
debugfs -w disk.img -R "mkdir /bin"
debugfs -w disk.img -R "write /path/to/host/static/bash /bin/bash"
debugfs -w disk.img -R "write ./tools/sh /bin/sh"

# 4. Import configuration files or text assets
debugfs -w disk.img -R "write /path/to/host/hello.txt /hello.txt"
```

A reference helper script is available at `./tools/format-disk.sh` which automates this layout preparation.

### Running in QEMU

Launch the compiled kernel natively within the QEMU emulator:

```bash
# Run using the release build (default)
./tools/run-qemu.sh

# Run and force specific options
./tools/run-qemu.sh --release
```

### Kernel-Level Debugging with GDB

You can connect GDB to inspect kernel state and step through execution:

1.  Start the emulator in GDB listening mode. This freezes execution at the first instruction and listens on local TCP port `1234`:
    ```bash
    ./tools/run-qemu.sh --debug
    ```
2.  From another terminal, connect with GDB:
    ```bash
    gdb -ex "target remote :1234"
    ```
    *(Or `rust-gdb -ex "target remote :1234"` to leverage Rust type formatting).*

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
