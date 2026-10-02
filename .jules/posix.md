# POSIX Conformance Journal

## 2026-03-30 - Return EBADF on ftruncate for Non-Writable File Descriptors

**Learning:** POSIX.1-2017 and Linux `ftruncate(2)` specifications mandate returning `-EBADF` (`-9`) when `ftruncate` is called on a file descriptor that is not open for writing (`!flags.is_writable()`). Previously, `sys_ftruncate` returned `-EINVAL` (`-22`).
**Action:** When validating open flags in `sys_ftruncate` (`kernel/src/syscall/fs/io.rs`), return `Errno::EBADF.into()` instead of `Errno::EINVAL.into()`.

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).
