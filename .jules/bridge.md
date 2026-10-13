# Bridge's Journal

## 2026-03-31 - sys_syslog (klogctl) implementation and pointer bounds validation
**Learning:** `syslog(2)` / `klogctl` (x86_64 syscall #103) is invoked by standard userspace logging utilities (`dmesg`, `syslogd`, musl `syslog(3)`). Enforcing pointer write validation (`validate_user_ptr_write`) on buffer read actions (commands 2, 3, 4) and level bounds checking on `SYSLOG_ACTION_CONSOLE_LEVEL` (command 8, bounds `1..=8`) ensures safe ABI conformance while returning `-EFAULT` or `-EINVAL` on invalid requests.
**Action:** When adding kernel system calls for message buffers or control operations, validate write permissions on output buffers and check parameter ranges before executing state operations.

## 2026-03-30 - prctl PR_SET_NAME / PR_GET_NAME user pointer validation
**Learning:** `prctl(PR_SET_NAME)` expects a pointer to a string up to 16 bytes (including null byte) while `prctl(PR_GET_NAME)` requires writing up to 16 bytes into user memory. Using explicit `validate_user_ptr_read` and `validate_user_ptr_write` checks on incoming buffers ensures memory safety and returns `-EFAULT` cleanly if invalid pointers are supplied by userspace.
**Action:** Always wrap input/output pointers in read/write validation functions before dereferencing or copying bytes in syscall handlers.


## 2026-03-29 - x86_64 Descriptor Syscalls and Timerfd Gettime Dispatch
**Learning:** In x86_64 Linux ABI, legacy single-parameter or default-flag descriptor creation system calls like `eventfd` (#284) and `signalfd` (#282) are unhandled when runtimes invoke non-flagged variants, and `timerfd_gettime` (#287) was previously misrouted to `timerfd_settime` with `arg1 = 0`.
**Action:** When implementing or fixing descriptor syscalls, delegate single-parameter syscalls (`eventfd`, `signalfd`) directly to their 4-parameter variants with default flags, and verify the x86_64 uapi syscall dispatch mapping table against standard Linux definitions.
