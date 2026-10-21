# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - POSIX/Linux rt_sigaction Permissiveness for Querying SIGKILL/SIGSTOP

**Learning:** POSIX.1-2017 and Linux `sigaction(2)` state that setting a new signal action (`!act.is_null()`) for `SIGKILL` (9) or `SIGSTOP` (19) must return `-EINVAL`. However, querying the current handler (`act.is_null() && !oldact.is_null()`) for `SIGKILL` or `SIGSTOP` is permitted and must return `0`.
**Action:** In `sys_rt_sigaction`, only return `Errno::EINVAL` for `SIGKILL` and `SIGSTOP` when `!act.is_null()`.
