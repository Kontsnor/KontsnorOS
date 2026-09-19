## 2026-03-30 - Clean Cargo workspace dependencies and inherited fields
**Learning:** Cargo manifests in workspace crates can emit `unused_dependencies` and `unused_workspace_package_fields` lints if workspace metadata fields are defined in root `Cargo.toml` but not referenced as `.workspace = true` in member crates, or if unused external crates (like `volatile` or `bitflags`) are declared when standard library modules (`core::ptr::{read_volatile, write_volatile}`) or existing imports are used instead.
**Action:** Always inherit workspace package metadata fields via `.workspace = true` in member crates and prune unused dependencies from `Cargo.toml` to maintain zero-warning builds across cargo check/clippy.

## 2026-03-31 - Redundant user-string copying API wrapper removal
**Learning:** `copy_string_from_user_pub` was a redundant public wrapper around `copy_string_from_user` in `kernel/src/syscall/validation.rs`. Consolidating user string copying to `copy_string_from_user` removes redundant wrapper boilerplate and unifies API usage across `syscall` and `process` modules.
**Action:** Prefer directly exposing public helper functions instead of creating `_pub` wrapper functions.
