# POSIX's Journal - Conformance Insights

## 2026-03-31 - sys_mmap Flag, Offset, and File Descriptor Validation
**Learning:** In POSIX.1-2017 and Linux `mmap(2)` specifications, `sys_mmap` must strictly validate `flags` (requiring exactly one of `MAP_SHARED` 0x01, `MAP_PRIVATE` 0x02, or `MAP_SHARED_VALIDATE` 0x03), `offset` (must be non-negative and page-aligned), and `fd` (when `MAP_ANONYMOUS` 0x20 is not set and `fd < 0`, returning `-EBADF`).
**Action:** Always enforce POSIX parameter bounds and mapping flag combinations in memory management syscall handlers before allocating virtual space or checking file descriptor tables.
