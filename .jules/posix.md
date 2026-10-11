# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Alignment of fcntl Error Semantics for Descriptors and Unknown Commands

**Learning:** POSIX.1-2017 and Linux `fcntl(2)` specifications dictate that passing an invalid file descriptor or a negative descriptor must return `-EBADF` (`-9`), and passing an unrecognized or unsupported `cmd` parameter must return `-EINVAL` (`-22`). Previously, `sys_fcntl` did not validate `fd` upfront across all branches and returned `-ENOSYS` (`-38`) for unknown `cmd` values.
**Action:** Always validate `fd` upfront using `proc_fd::current_task_get_file_desc(fd)` returning `-EBADF`, and return `-EINVAL` for unsupported `fcntl` commands.
