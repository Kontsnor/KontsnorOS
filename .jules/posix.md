## 2026-03-31 - mmap Syscall Flag and Offset Validations

**Learning:** In POSIX.1-2017 / Linux `mmap(2)` specifications, `mmap` requires:
1. `flags` must contain exactly one mapping type (`MAP_SHARED`, `MAP_PRIVATE`, or `MAP_SHARED_VALIDATE`), returning `-EINVAL` if neither or multiple types are specified (`flags & 0x0F`).
2. `offset` must be page-aligned (multiple of 4096 on x86_64) and non-negative, returning `-EINVAL` if non-aligned.
3. For file-backed mappings (`MAP_ANONYMOUS` 0x20 flag omitted), negative file descriptors (`fd < 0`) must return `-EBADF` rather than implicitly defaulting to anonymous mappings.

**Action:** Always validate `flags & 0x0F`, `offset % 4096 == 0`, and `fd >= 0` upfront in `sys_mmap` handlers before attempting file descriptor lookups or page table allocations.
