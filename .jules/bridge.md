# Bridge's Journal

## 2026-03-31 - mknodat / mknod File Creation and Mode Bits
**Learning:** `sys_mknodat` (x86_64 syscall #259) and `sys_mknod` (x86_64 syscall #133) validate user string pointers via `copy_string_from_user`, resolve target paths relative to directory file descriptors (`dfd`), check parent directory permissions (`MAY_WRITE`, `MAY_EXEC`), decode POSIX `S_IFMT` mode bits into VFS `FileType` variants, apply task `umask`, and invalidate VFS dentries, while `TmpFsDir::create` (`kernel/src/fs/tmpfs.rs`) supports creating `Pipe`, `CharDevice`, `BlockDevice`, and `Socket` file types.
**Action:** When implementing path creation system calls, route path resolution through `resolve_relative_path_at(dfd, path)`, validate parent permissions, and ensure VFS inode creation handles all POSIX `FileType` variants.

## 2026-03-30 - prctl PR_SET_NAME / PR_GET_NAME user pointer validation
**Learning:** `prctl(PR_SET_NAME)` expects a pointer to a string up to 16 bytes (including null byte) while `prctl(PR_GET_NAME)` requires writing up to 16 bytes into user memory. Using explicit `validate_user_ptr_read` and `validate_user_ptr_write` checks on incoming buffers ensures memory safety and returns `-EFAULT` cleanly if invalid pointers are supplied by userspace.
**Action:** Always wrap input/output pointers in read/write validation functions before dereferencing or copying bytes in syscall handlers.


## 2026-03-29 - x86_64 Descriptor Syscalls and Timerfd Gettime Dispatch
**Learning:** In x86_64 Linux ABI, legacy single-parameter or default-flag descriptor creation system calls like `eventfd` (#284) and `signalfd` (#282) are unhandled when runtimes invoke non-flagged variants, and `timerfd_gettime` (#287) was previously misrouted to `timerfd_settime` with `arg1 = 0`.
**Action:** When implementing or fixing descriptor syscalls, delegate single-parameter syscalls (`eventfd`, `signalfd`) directly to their 4-parameter variants with default flags, and verify the x86_64 uapi syscall dispatch mapping table against standard Linux definitions.
