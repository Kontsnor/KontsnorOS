# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Enforcement of Mapping Type and Offset Validation in sys_mmap

**Learning:** POSIX.1-2017 and Linux `mmap(2)` specifications require `flags` to specify exactly one mapping type (`MAP_SHARED` `0x01`, `MAP_PRIVATE` `0x02`, or `MAP_SHARED_VALIDATE` `0x03`) and `offset` to be non-negative and page-aligned (multiple of 4096), returning `-EINVAL` (`-22`) if invalid.
**Action:** Validate `flags & 0x0F` and `offset` at the start of `sys_mmap` in `kernel/src/syscall/memory.rs` and return `Errno::EINVAL.into()`.
