# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Alignment of File Descriptor Duplication Error Semantics (dup, dup3, fcntl)

**Learning:** POSIX.1-2017 and Linux `dup(2)`, `dup3(2)`, and `fcntl(2)` require strict error status return codes: `sys_dup` must return `-EMFILE` when table allocation fails for valid `fd`s; `sys_dup3` must return `-EBADF` for negative `oldfd`/`newfd` while reserving `-EINVAL` for `oldfd == newfd` and invalid flags; `sys_fcntl` (`F_DUPFD`/`F_DUPFD_CLOEXEC`) must return `-EINVAL` when `start_fd >= RLIMIT_NOFILE` and `-EMFILE` when no available descriptor slot `< RLIMIT_NOFILE` exists.
**Action:** Always validate `start_fd` against `RLIMIT_NOFILE` and ensure `dup` returns `EMFILE` when table limit is hit rather than `EBADF`.
