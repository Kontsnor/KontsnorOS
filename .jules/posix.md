# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-31 - Alignment of fcntl Error Handling and File Descriptor Bounds

**Learning:** POSIX.1-2017 and Linux `fcntl(2)` specifications require `fcntl` to validate descriptor `fd` up front (returning `-EBADF` if invalid) and return `-EINVAL` (`-22`) for unrecognized or unsupported `cmd` values rather than `-ENOSYS`. Furthermore, `F_DUPFD` and `F_DUPFD_CLOEXEC` must check `start_fd` against `RLIMIT_NOFILE` returning `-EINVAL` if `start_fd >= rlimit_nofile_cur`, and returning `-EMFILE` if no descriptor slot `< rlimit_nofile_cur` is available.
**Action:** Always validate `fd` up front in `sys_fcntl`, check `start_fd` against `rlimit_nofile_cur`, and fallback to `-EINVAL` instead of `-ENOSYS` for unhandled `cmd` numbers.
