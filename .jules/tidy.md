## 2026-03-30 - Clean Cargo workspace dependencies and inherited fields
**Learning:** Cargo manifests in workspace crates can emit `unused_dependencies` and `unused_workspace_package_fields` lints if workspace metadata fields are defined in root `Cargo.toml` but not referenced as `.workspace = true` in member crates, or if unused external crates (like `volatile` or `bitflags`) are declared when standard library modules (`core::ptr::{read_volatile, write_volatile}`) or existing imports are used instead.
**Action:** Always inherit workspace package metadata fields via `.workspace = true` in member crates and prune unused dependencies from `Cargo.toml` to maintain zero-warning builds across cargo check/clippy.

## 2026-03-30 - Safe MaybeUninit array initialization and UnsafeCell interior mutability in no_std
**Learning:** Initializing a `[MaybeUninit<T>; N]` array using `MaybeUninit::uninit().assume_init()` triggers immediate Undefined Behavior (UB) in Rust. Furthermore, mutating buffer slots through shared `&self` references requires wrapping the storage buffer in `core::cell::UnsafeCell` to respect Rust's aliasing rules and Stacked Borrows model.
**Action:** Always initialize `[MaybeUninit<T>; N]` using const block evaluation `[const { core::mem::MaybeUninit::uninit() }; N]`, wrap shared-mutable raw buffers in `UnsafeCell`, and provide explicit `Send`/`Sync` implementations for lock-free datastructures.

## 2026-03-30 - Bitwise signal scanning and core array indexing with core combinators
**Learning:** Manual bit-scanning loops (e.g. `for i in 1..=64` over a `u64` bitmask) in `no_std` kernel mode can be replaced with `(unblocked.trailing_zeros() + 1) as i32` for non-zero masks to achieve O(1) single-instruction bit scanning. Note that `trailing_zeros()` returns `u32`, so explicit casting to `i32` is required when interfacing with 1-indexed signal numbers or syscall APIs expecting `i32`.
**Action:** Always verify primitive integer type compatibility when replacing loops with `core` bitwise methods like `trailing_zeros()`.
