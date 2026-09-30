# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Empty Pathname Resolution Enforcement in open, openat, truncate, and chdir

**Learning:** POSIX.1-2017 Base Definitions Chapter 4.13 specifies that an empty pathname string shall fail with `ENOENT` and not resolve to any file. Previously, `sys_open`, `sys_truncate`, and `sys_chdir` resolved empty pathnames `""` to `"/"` via relative path normalization, while `sys_openat` lacked handling for `AT_EMPTY_PATH` (`0x1000`).
**Action:** Validate empty pathnames (`raw_path.is_empty()`) early in path-based syscall handlers, returning `-ENOENT` unless `sys_openat` receives `AT_EMPTY_PATH` (`0x1000`).
