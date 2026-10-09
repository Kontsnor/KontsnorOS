# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Return EBADF on ftruncate for Non-Writable File Descriptors

**Learning:** POSIX.1-2017/2024 and Linux `ftruncate(2)` specifications require that calling `ftruncate` on a file descriptor that is not open for writing returns `-EBADF` (`Errno::EBADF`). Previously, `sys_ftruncate` returned `-EINVAL` when `!flags.is_writable()`.
**Action:** Check file descriptor open flags in `sys_ftruncate` and return `Errno::EBADF` when not open for writing.
