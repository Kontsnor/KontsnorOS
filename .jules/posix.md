# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Alignment of fcntl Error Semantics and Bounds Checks

**Learning:** POSIX.1-2017/2024 and Linux `fcntl(2)` require early `fd` descriptor validation returning `-EBADF`, bounds checking `start_fd < 0 || start_fd >= 1024` returning `-EINVAL`, table allocation limits returning `-EMFILE`, and unhandled/unknown `cmd` values returning `-EINVAL` (rather than `-ENOSYS`).
**Action:** In `sys_fcntl`, validate `fd` up front with `proc_fd::current_task_get_file_desc(fd)` and map unknown commands to `-EINVAL`.
