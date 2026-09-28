# POSIX Conformance Journal

## 2026-03-30 - Return EISDIR when Reading Directory File Descriptors

**Learning:** POSIX.1-2017 / POSIX.1-2024 and Linux `read(2)` specifications mandate that calling `read` on a file descriptor referring to a directory shall fail with `EISDIR` (`-21`). Previously, `FileDescription::read` forwarded the read request down to directory inodes instead of immediately returning `-21` (`-EISDIR`).
**Action:** In `FileDescription::read`, check `self.inode.inode().file_type` and return `Err(-21)` (`-EISDIR`) if `file_type == FileType::Directory`.

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).
