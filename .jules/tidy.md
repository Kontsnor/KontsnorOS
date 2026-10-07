## 2026-03-30 - Clean Cargo workspace dependencies and inherited fields
**Learning:** Cargo manifests in workspace crates can emit `unused_dependencies` and `unused_workspace_package_fields` lints if workspace metadata fields are defined in root `Cargo.toml` but not referenced as `.workspace = true` in member crates, or if unused external crates (like `volatile` or `bitflags`) are declared when standard library modules (`core::ptr::{read_volatile, write_volatile}`) or existing imports are used instead.
**Action:** Always inherit workspace package metadata fields via `.workspace = true` in member crates and prune unused dependencies from `Cargo.toml` to maintain zero-warning builds across cargo check/clippy.

## 2026-03-30 - Safe MaybeUninit array initialization and UnsafeCell interior mutability in no_std
**Learning:** Initializing a `[MaybeUninit<T>; N]` array using `MaybeUninit::uninit().assume_init()` triggers immediate Undefined Behavior (UB) in Rust. Furthermore, mutating buffer slots through shared `&self` references requires wrapping the storage buffer in `core::cell::UnsafeCell` to respect Rust's aliasing rules and Stacked Borrows model.
**Action:** Always initialize `[MaybeUninit<T>; N]` using const block evaluation `[const { core::mem::MaybeUninit::uninit() }; N]`, wrap shared-mutable raw buffers in `UnsafeCell`, and provide explicit `Send`/`Sync` implementations for lock-free datastructures.

## 2026-03-30 - Direct UART byte transmission over format_args! string formatting
**Learning:** Formatting raw bytes as `char` through `format_args!("{}", byte as char)` in serial transmission loops introduces significant core formatting and macro overhead. `uart_16550::SerialPort` provides `send(byte: u8)` to transmit single bytes directly to the UART transmitter register. Furthermore, delegating `SerialConsole::write` to `arch::x86_64::serial::write_byte` ensures consistent graphics console mirroring without redundant `_print` macro invocations.
**Action:** Always prefer direct hardware register access methods (`port.send(byte)`) and helper functions (`write_byte`) over generic formatting macros (`format_args!`) for raw byte streams.
