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

### Kernel Safety & ABI Rules

When writing kernel-level code, system call handlers, or driver components, developers must strictly adhere to the following safety guidelines:

#### 1. Lock Safety & Deadlocks (Recursive Lock Prevention)
*   **No Recursive Spinlocks**: Spinlocks (e.g. `TicketLock`) in KontsnorOS are non-recursive. Attempting to acquire a lock on a CPU core that already holds it will result in an immediate deadlock.
*   **Scheduler Lock Hazard**: A common deadlock occurs when calling `scheduler::current_pid()` while holding the `SCHEDULER` TicketLock. Because `current_pid()` attempts to acquire the `SCHEDULER` lock internally, this creates a recursive lock cycle.
*   **Mitigation**: If you already hold the lock guard to a synchronized resource (such as the scheduler), retrieve the required state (e.g. the active PID) from the held lock guard instead of invoking global utility helpers that re-acquire the lock:
    ```rust
    // BAD: Deadlock hazard if SCHEDULER is already locked in this scope
    let current_pid = scheduler::current_pid();

    // GOOD: Reuse the existing lock guard to query the scheduler state
    let sched_lock = scheduler::SCHEDULER.lock();
    let current_pid = sched_lock.as_ref().unwrap().current_pid();
    ```

#### 2. User Space Memory Safety
*   **Pointer Validation**: All raw pointers passed from Ring 3 (user-space) must be validated before being read from or written to inside system call handlers. Never assume user-space pointers are well-formed, non-null, or mapped.
*   **Rules for Handlers**:
    1.  Ensure the pointer range is strictly below the user-space virtual memory boundary (`0x0000_7FFF_FFFF_FFFF`).
    2.  Verify that the range does not integer wrap.
    3.  Ensure every virtual page in the range is mapped in the active page directory.
    4.  For write targets, ensure that the mapped pages are writable.
*   **Use Helper Functions**: Do not perform manual pointer dereferences. Instead, use the validation and copying APIs in `kernel/src/syscall/fs.rs`:
    *   `validate_user_ptr(ptr, size)`: Checks if a read pointer range is mapped and in bounds.
    *   `validate_user_ptr_write(ptr, size)`: Checks if a write target range is mapped, writable, and in bounds.
    *   `copy_string_from_user(ptr)`: Securely copies a null-terminated string from user space while validating pages to avoid page faults.

#### 3. ABI Structure Matching
*   **C representation**: Any struct exposed to or populated by user-space must be annotated with `#[repr(C)]` to disable Rust's compiler-dependent field reordering.
*   **ABI Mappings**: The sizes, offsets, and field types must strictly conform to the expected Linux kernel ABI layouts for x86_64:
    *   *Example*: The kernel's `Termios` structure must be exactly 36 bytes (`NCCS = 19`). A mismatch will lead to stack corruption and segfaults in user-space libraries (like musl-libc or glibc) that allocate a 36-byte stack frame.

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
