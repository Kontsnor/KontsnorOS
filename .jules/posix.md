# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Rejection of Negative PID/PGID Arguments in setpgid

**Learning:** POSIX.1-2017 and Linux `setpgid(2)` specifications require `setpgid` to return `-EINVAL` (`-22`) if either `pid` or `pgid` is negative (< 0). Previously, `sys_setpgid` cast negative `pid`/`pgid` integers directly to `u64`, which resulted in invalid PID lookups returning `-ESRCH` (`-3`) instead of `-EINVAL`.
**Action:** Validate `pid < 0 || pgid < 0` at the start of `sys_setpgid` and return `Errno::EINVAL` (`-EINVAL`).
