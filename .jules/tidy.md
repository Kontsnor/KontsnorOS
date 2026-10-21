## 2026-03-30 - Clean Cargo workspace dependencies and inherited fields
**Learning:** Cargo manifests in workspace crates can emit `unused_dependencies` and `unused_workspace_package_fields` lints if workspace metadata fields are defined in root `Cargo.toml` but not referenced as `.workspace = true` in member crates, or if unused external crates (like `volatile` or `bitflags`) are declared when standard library modules (`core::ptr::{read_volatile, write_volatile}`) or existing imports are used instead.
**Action:** Always inherit workspace package metadata fields via `.workspace = true` in member crates and prune unused dependencies from `Cargo.toml` to maintain zero-warning builds across cargo check/clippy.

## 2026-03-30 - Safe MaybeUninit array initialization and UnsafeCell interior mutability in no_std
**Learning:** Initializing a `[MaybeUninit<T>; N]` array using `MaybeUninit::uninit().assume_init()` triggers immediate Undefined Behavior (UB) in Rust. Furthermore, mutating buffer slots through shared `&self` references requires wrapping the storage buffer in `core::cell::UnsafeCell` to respect Rust's aliasing rules and Stacked Borrows model.
**Action:** Always initialize `[MaybeUninit<T>; N]` using const block evaluation `[const { core::mem::MaybeUninit::uninit() }; N]`, wrap shared-mutable raw buffers in `UnsafeCell`, and provide explicit `Send`/`Sync` implementations for lock-free datastructures.

## 2026-10-01 - Standard core::ptr memory stubs and lint suppression
**Learning:** In freestanding memory stubs (`self_host_stubs.rs`), replacing manual byte-level pointer arithmetic loops with standard `core::ptr::copy_nonoverlapping`, `core::ptr::write_bytes`, `core::ptr::copy`, and slice `cmp` produces cleaner, idiomatic Rust that LLVM can optimize heavily. In addition, annotating C runtime memory symbols with `#[allow(suspicious_runtime_symbol_definitions)]` prevents compiler lints during workspace host compilation target checks (`cargo check --workspace --all-targets`).
**Action:** Use standard `core::ptr` functions for raw memory operations and suppress runtime symbol definition lints on host-compiled self-host stubs.
