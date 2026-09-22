## 2026-09-22 - Align `wait4` and `waitid` options and pointer validation with POSIX.1-2017 & Linux specs

**Learning:** In POSIX.1-2017 and Linux `wait4(2)` / `waitid(2)`, passing invalid `options` bitmasks must return `-EINVAL`. Furthermore, `waitid` requires that `options` specifies at least one of `WEXITED` (4), `WSTOPPED` (2), or `WCONTINUED` (8). User pointers `rusage` (in `wait4`) and `infop` (in `waitid`) must be validated up front before wait processing, returning `-EFAULT` on invalid user memory addresses.

**Action:** Always validate syscall `options` masks against standard bitfield sets before entering scheduling wait loops, and validate output user structure pointers before wait operations.
