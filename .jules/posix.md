# POSIX Conformance Journal

## 2026-03-31 - Relative Path Resolution `dfd` Validation

**Learning:** In standard POSIX.1-2017 `openat` and Linux `openat(2)` specs, when a relative path is passed to path resolution functions like `resolve_relative_path_at`, any negative directory file descriptor other than `AT_FDCWD` (`-100`) must immediately fail with `-EBADF`. Previously, invalid negative file descriptors passed to `resolve_relative_path_at` were not explicitly checked before descriptor table lookup.

**Action:** Ensured `dfd < 0 && dfd != -100` returns `Err(Errno::EBADF)` upfront in `resolve_relative_path_at` (`kernel/src/fs/vfs.rs`).
