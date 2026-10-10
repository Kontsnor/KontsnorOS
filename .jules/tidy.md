## 2026-03-30 - Clean Cargo workspace dependencies and inherited fields
**Learning:** Cargo manifests in workspace crates can emit `unused_dependencies` and `unused_workspace_package_fields` lints if workspace metadata fields are defined in root `Cargo.toml` but not referenced as `.workspace = true` in member crates, or if unused external crates (like `volatile` or `bitflags`) are declared when standard library modules (`core::ptr::{read_volatile, write_volatile}`) or existing imports are used instead.
**Action:** Always inherit workspace package metadata fields via `.workspace = true` in member crates and prune unused dependencies from `Cargo.toml` to maintain zero-warning builds across cargo check/clippy.

## 2026-03-30 - Safe MaybeUninit array initialization and UnsafeCell interior mutability in no_std
**Learning:** Initializing a `[MaybeUninit<T>; N]` array using `MaybeUninit::uninit().assume_init()` triggers immediate Undefined Behavior (UB) in Rust. Furthermore, mutating buffer slots through shared `&self` references requires wrapping the storage buffer in `core::cell::UnsafeCell` to respect Rust's aliasing rules and Stacked Borrows model.
**Action:** Always initialize `[MaybeUninit<T>; N]` using const block evaluation `[const { core::mem::MaybeUninit::uninit() }; N]`, wrap shared-mutable raw buffers in `UnsafeCell`, and provide explicit `Send`/`Sync` implementations for lock-free datastructures.

## 2026-03-31 - Safe pre-clamped framebuffer pointer arithmetic and loop consolidation
**Learning:** Software framebuffer rendering methods (`set_pixel`, `fill_rect`, `clear`) can eliminate per-pixel boundary checks and division arithmetic inside nested rendering loops by pre-calculating the 32-bit row stride (`stride / 4`), pre-clamping coordinates via `saturating_add` and `.min()`, and using direct pointer offsets (`.add(offset)`). Delegating `clear` to `fill_rect` avoids loop duplication while keeping `no_std` pointer writes memory-safe.
**Action:** When working on pixel buffers or multi-dimensional raw array rendering in `no_std`, pre-calculate row strides, clamp loop ranges upfront, and delegate full-buffer clears to pre-clamped rect routines.
