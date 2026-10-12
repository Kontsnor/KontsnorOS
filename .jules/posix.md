# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Enforcement of EISDIR on Directory File Descriptions in read

**Learning:** POSIX.1-2017/2024 and Linux `read(2)` specifications require calling `read` on a file description referring to a directory (`FileType::Directory`) to fail and return `-EISDIR` (`-21`). `FileDescription::read` in `kernel/src/fs/file.rs` previously fell through to calling `read(0, buf)` on directory inodes instead of checking `FileType::Directory` and returning `Err(-21)`.
**Action:** In `FileDescription::read`, check `file_type == FileType::Directory` up front and return `Err(-21)` (`-EISDIR`).
