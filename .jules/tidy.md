## 2026-03-30 - Clean Cargo workspace dependencies and inherited fields
**Learning:** Cargo manifests in workspace crates can emit `unused_dependencies` and `unused_workspace_package_fields` lints if workspace metadata fields are defined in root `Cargo.toml` but not referenced as `.workspace = true` in member crates, or if unused external crates (like `volatile` or `bitflags`) are declared when standard library modules (`core::ptr::{read_volatile, write_volatile}`) or existing imports are used instead.
**Action:** Always inherit workspace package metadata fields via `.workspace = true` in member crates and prune unused dependencies from `Cargo.toml` to maintain zero-warning builds across cargo check/clippy.

## 2026-03-30 - Bitwise page alignment checks on power-of-two constants
**Learning:** Checking page alignment on power-of-two boundaries (e.g. `PAGE_SIZE = 4096`) using modulo arithmetic (`addr % 4096 == 0`) emits integer division (`div`/`rem`) instructions, whereas using bitwise masking (`(addr & (PAGE_SIZE - 1)) == 0`) is idiomatic, faster on hot kernel address translation paths, and matches `align_down`/`align_up` helper implementations.
**Action:** Use `(addr & (PAGE_SIZE as u64 - 1)) == 0` for power-of-two alignment checks across memory primitives.
