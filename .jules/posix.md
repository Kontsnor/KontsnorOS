# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Enforce 4096-Byte Page Alignment on Offset in mmap

**Learning:** POSIX.1-2017/2024 and Linux `mmap(2)` specifications require returning `-EINVAL` (`Errno::EINVAL`) when `offset` is not a multiple of the system page size (4096 bytes). Previously, `sys_mmap` only validated `length == 0` without checking `offset` page alignment.
**Action:** Always validate `(offset as u64 & 4095) == 0` in `sys_mmap` and return `-EINVAL` when misaligned.
