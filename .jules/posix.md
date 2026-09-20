# POSIX's Journal - Critical Learnings & Conformance Insights

## 2026-09-20 - Unlinking directories via `unlink`/`unlinkat` returns `EISDIR`
**Learning:** In POSIX.1-2017 and Linux `unlink(2)` / `unlinkat(2)` specifications, calling `unlink` or `unlinkat` (without `AT_REMOVEDIR`) on a directory path must fail and set `errno` to `EISDIR` (`-21`). Returning `EPERM` or attempting directory unlinking causes test failures and breaks standard C libraries (Musl, glibc) and system utilities (`rm`).
**Action:** When handling `sys_unlink` / `sys_unlinkat`, check whether the resolved target inode is a directory before parent directory lookup and return `Errno::EISDIR` immediately.
