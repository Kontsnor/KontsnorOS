# POSIX Conformance Journal

## 2026-03-30 - Flag Validation and EINVAL in epoll_create1

**Learning:** Linux `epoll_create1(2)` specifications require validating the `flags` parameter and returning `-EINVAL` (`-22`) if invalid flag bits (bits other than `EPOLL_CLOEXEC` = 0x80000) are set. Previously, `sys_epoll_create1` ignored invalid flag bits and created an epoll instance regardless.
**Action:** In `sys_epoll_create1`, check `(flags & !EPOLL_CLOEXEC) != 0` and return `-EINVAL` if non-zero.

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).
