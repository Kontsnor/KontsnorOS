# Bridge's Journal

## 2026-03-31 - mknod / mknodat POSIX S_IFMT mode bit decoding and directory handling
**Learning:** `mknod` (#133) and `mknodat` (#259) require parsing POSIX file type bitmask `S_IFMT` (`0o170000`) from the `mode` parameter. POSIX and Linux specifications explicitly require returning `-EINVAL` if `S_IFDIR` (`0o040000`) is supplied, as directory creation must go through `mkdir`/`mkdirat`.
**Action:** When implementing filesystem creation syscalls, strictly enforce mode bit validation and ensure directory creation paths return `-EINVAL` when invoked through `mknod`/`mknodat`.

## 2026-03-30 - prctl PR_SET_NAME / PR_GET_NAME user pointer validation
**Learning:** `prctl(PR_SET_NAME)` expects a pointer to a string up to 16 bytes (including null byte) while `prctl(PR_GET_NAME)` requires writing up to 16 bytes into user memory. Using explicit `validate_user_ptr_read` and `validate_user_ptr_write` checks on incoming buffers ensures memory safety and returns `-EFAULT` cleanly if invalid pointers are supplied by userspace.
**Action:** Always wrap input/output pointers in read/write validation functions before dereferencing or copying bytes in syscall handlers.


## 2026-03-29 - x86_64 Descriptor Syscalls and Timerfd Gettime Dispatch
**Learning:** In x86_64 Linux ABI, legacy single-parameter or default-flag descriptor creation system calls like `eventfd` (#284) and `signalfd` (#282) are unhandled when runtimes invoke non-flagged variants, and `timerfd_gettime` (#287) was previously misrouted to `timerfd_settime` with `arg1 = 0`.
**Action:** When implementing or fixing descriptor syscalls, delegate single-parameter syscalls (`eventfd`, `signalfd`) directly to their 4-parameter variants with default flags, and verify the x86_64 uapi syscall dispatch mapping table against standard Linux definitions.
