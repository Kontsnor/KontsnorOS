# Bridge's Journal

## 2026-03-30 - prctl PR_SET_NAME / PR_GET_NAME user pointer validation
**Learning:** `prctl(PR_SET_NAME)` expects a pointer to a string up to 16 bytes (including null byte) while `prctl(PR_GET_NAME)` requires writing up to 16 bytes into user memory. Using explicit `validate_user_ptr_read` and `validate_user_ptr_write` checks on incoming buffers ensures memory safety and returns `-EFAULT` cleanly if invalid pointers are supplied by userspace.
**Action:** Always wrap input/output pointers in read/write validation functions before dereferencing or copying bytes in syscall handlers.


## 2026-03-29 - x86_64 Descriptor Syscalls and Timerfd Gettime Dispatch
**Learning:** In x86_64 Linux ABI, legacy single-parameter or default-flag descriptor creation system calls like `eventfd` (#284) and `signalfd` (#282) are unhandled when runtimes invoke non-flagged variants, and `timerfd_gettime` (#287) was previously misrouted to `timerfd_settime` with `arg1 = 0`.
**Action:** When implementing or fixing descriptor syscalls, delegate single-parameter syscalls (`eventfd`, `signalfd`) directly to their 4-parameter variants with default flags, and verify the x86_64 uapi syscall dispatch mapping table against standard Linux definitions.


## 2026-03-31 - mknod and mknodat Mode Bitmask Decoding and File Creation
**Learning:** `mknod` (#133) and `mknodat` (#259) receive mode bitmasks combining POSIX file type mask `S_IFMT` (`0o170000`) and permission bits. Decoding `mode & 0o170000` accurately maps user requests to VFS `FileType` (`S_IFREG`/0 for Regular, `S_IFCHR` for CharDevice, `S_IFBLK` for BlockDevice, `S_IFIFO` for Pipe/FIFO, `S_IFSOCK` for Socket), while rejecting directory creation (`S_IFDIR`) with `-EINVAL` per Linux kernel specifications.
**Action:** When implementing node creation syscalls, mask off `S_IFMT` bits to determine `FileType`, apply current process `umask` to the permission bits, and delegate `sys_mknod` directly to `sys_mknodat(AT_FDCWD, pathname, mode, dev)`.
