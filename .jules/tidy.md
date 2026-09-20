## 2026-03-30 - Clean Cargo workspace dependencies and inherited fields
**Learning:** Cargo manifests in workspace crates can emit `unused_dependencies` and `unused_workspace_package_fields` lints if workspace metadata fields are defined in root `Cargo.toml` but not referenced as `.workspace = true` in member crates, or if unused external crates (like `volatile` or `bitflags`) are declared when standard library modules (`core::ptr::{read_volatile, write_volatile}`) or existing imports are used instead.
**Action:** Always inherit workspace package metadata fields via `.workspace = true` in member crates and prune unused dependencies from `Cargo.toml` to maintain zero-warning builds across cargo check/clippy.

## 2026-03-30 - Direct UART byte transmission in serial console driver
**Learning:** `uart_16550::SerialPort::send(byte)` directly transmits bytes to the UART hardware register without allocating `format_args!` string formatting infrastructure. Calling `arch::x86_64::serial::write_byte` from `SerialConsole::write` avoids per-byte `_print` lock re-entrancy and string formatting overhead.
**Action:** Use `port.send(byte)` for single-byte serial I/O and delegate `CharDevice` serial output to `write_byte`.
