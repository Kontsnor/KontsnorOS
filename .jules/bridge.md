# Bridge's Journal

## 2026-03-30 - sys_mknodat and sys_mknod POSIX file mode decoding and VFS node creation
**Learning:** Modern Linux runtimes and shell utilities (e.g. `mknod`, `mkfifo`) invoke `mknodat` (#259) or `mknod` (#133) expecting POSIX `S_IFMT` mode decoding (`S_IFREG`, `S_IFIFO`, `S_IFCHR`, `S_IFBLK`, `S_IFSOCK`), task umask masking on file permissions (`mode & 0o7777 & !umask`), and VFS dentry cache invalidation. In-memory filesystems like `tmpfs` must support non-regular file types in `InodeOps::create`.
**Action:** When implementing node creation syscalls, map POSIX mode bitmasks directly to VFS `FileType` variants, apply process umask, set process EUID/EGID ownership, and invalidate VFS dentries on success.

## 2026-03-30 - prctl PR_SET_NAME / PR_GET_NAME user pointer validation
**Learning:** `prctl(PR_SET_NAME)` expects a pointer to a string up to 16 bytes (including null byte) while `prctl(PR_GET_NAME)` requires writing up to 16 bytes into user memory. Using explicit `validate_user_ptr_read` and `validate_user_ptr_write` checks on incoming buffers ensures memory safety and returns `-EFAULT` cleanly if invalid pointers are supplied by userspace.
**Action:** Always wrap input/output pointers in read/write validation functions before dereferencing or copying bytes in syscall handlers.


## 2026-03-29 - x86_64 Descriptor Syscalls and Timerfd Gettime Dispatch
**Learning:** In x86_64 Linux ABI, legacy single-parameter or default-flag descriptor creation system calls like `eventfd` (#284) and `signalfd` (#282) are unhandled when runtimes invoke non-flagged variants, and `timerfd_gettime` (#287) was previously misrouted to `timerfd_settime` with `arg1 = 0`.
**Action:** When implementing or fixing descriptor syscalls, delegate single-parameter syscalls (`eventfd`, `signalfd`) directly to their 4-parameter variants with default flags, and verify the x86_64 uapi syscall dispatch mapping table against standard Linux definitions.
