# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Enforcement of ERANGE on getcwd Small Buffer in sys_getcwd

**Learning:** POSIX.1-2017 and Linux `getcwd(2)` specifications require `sys_getcwd` to return `-ERANGE` (`-34`) when the provided buffer `size` is non-zero but smaller than the current working directory pathname length plus 1 byte for the null terminator. `sys_getcwd` previously returned `-EINVAL` (`-22`), causing dynamic buffer allocation logic in standard C libraries (such as glibc and musl) to fail instead of expanding the buffer and retrying.
**Action:** Validate `size == 0` for `-EINVAL`, `buf` validity for `-EFAULT`, and return `-ERANGE` when `cwd_bytes.len() + 1 > size`.
