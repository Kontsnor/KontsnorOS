# Contributing to KontsnorOS

Thank you for your interest in contributing to KontsnorOS! This document provides
guidelines for contributing to the project.

## Code of Conduct

Be respectful, constructive, and inclusive. We're building something amazing together.

## Getting Started

1. **Fork** the repository
2. **Clone** your fork: `git clone https://github.com/YOUR_USERNAME/KontsnorOS.git`
3. **Create a branch**: `git checkout -b feature/my-feature`
4. **Make changes** and commit: `git commit -m "feat: add my feature"`
5. **Push** and open a Pull Request

## Development Setup

### Prerequisites

- Rust nightly (auto-installed via `rust-toolchain.toml`)
- QEMU: `sudo apt install qemu-system-x86`

### Building & Testing

```bash
# Build
cargo build

# Run in QEMU
./tools/run-qemu.sh

# Check formatting
cargo fmt --check

# Run clippy
cargo clippy
```

## Coding Guidelines

### Rust Style

- Follow standard Rust naming conventions
- Use `cargo fmt` for formatting
- Use `cargo clippy` and fix all warnings
- Document all public items with `///` doc comments
- Every `unsafe` block must have a `// SAFETY:` comment explaining why it's safe

### Commit Messages

We use [Conventional Commits](https://www.conventionalcommits.org/):

- `feat: add PCI bus enumeration`
- `fix: handle page fault in heap allocator`
- `docs: update driver development guide`
- `refactor: simplify VFS mount table`

### Pull Request Guidelines

- One PR per feature or fix
- Include tests if applicable
- Update documentation
- All CI checks must pass

## Writing Drivers

If you're contributing a hardware driver:

1. Use the `driver-sdk` crate for stable APIs
2. Implement the appropriate trait (`CharDevice`, `BlockDevice`, `NetDevice`, `GpuDevice`)
3. Add your driver to `kernel/src/drivers/`
4. Document hardware quirks and workarounds
5. All `unsafe` code must be justified and minimal

### Driver Licensing

Drivers may use any license compatible with MIT or Apache-2.0. Proprietary
drivers that use only the public `driver-sdk` API are permitted.

## Architecture Decisions

Major architectural changes should be discussed in an issue first. We follow
these principles:

- **Safety first** — Prefer safe Rust; minimize `unsafe`
- **Modularity** — Drivers and filesystems should be independent modules
- **POSIX compatibility** — Follow POSIX semantics for syscalls
- **Documentation** — Code is read more than written; document everything

## License

By contributing to KontsnorOS, you agree that your contributions will be
licensed under both the MIT License and the Apache License 2.0.
