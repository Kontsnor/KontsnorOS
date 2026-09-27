## 2026-09-27 - Implement `faccessat2` (syscall #439) with strict flag and mode validation

**Learning:** Standard C runtimes (Musl, Glibc) invoke `faccessat2` (x86_64 syscall #439) instead of `faccessat` (#269). Linux `faccessat2(2)` specifications require strict bitmask validation on both `mode` (`F_OK` = 0, `R_OK` = 4, `W_OK` = 2, `X_OK` = 1; returning `-EINVAL` if `mode & !0x7 != 0`) and `flags` (`AT_SYMLINK_NOFOLLOW` = 0x100, `AT_EACCESS` = 0x200, `AT_EMPTY_PATH` = 0x1000; returning `-EINVAL` on unsupported bits). Furthermore, when `raw_path` is empty and `AT_EMPTY_PATH` is not set, `faccessat2` returns `-ENOENT`.

**Action:** Whenever expanding Linux file accessibility system calls, ensure flag and mode validation bitmasks explicitly reject unsupported bits with `-EINVAL` before path lookup or permission checks, and delegate `sys_faccessat` directly to `sys_faccessat2`.
