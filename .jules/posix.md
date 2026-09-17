## 2026-03-31 - Align fcntl(2) Error Codes and Argument Validation with POSIX/Linux Specs

**Learning:** Linux and POSIX.1-2017 specifications require `fcntl(2)` to return `-EBADF` if `fd` is not a valid open file descriptor, `-EINVAL` if `cmd` is unrecognized or if `F_DUPFD` / `F_DUPFD_CLOEXEC` request a target file descriptor exceeding system limits or negative. KontsnorOS previously returned `-ENOSYS` on unknown commands and missed `fd` validation before command dispatch.

**Action:** Ensure `sys_fcntl` validates `fd` upfront, returns `-EINVAL` for unsupported commands (instead of `-ENOSYS`), and validates `F_DUPFD` start bounds.
