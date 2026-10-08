# POSIX Conformance Journal

## 2026-03-30 - Enforcement of ESPIPE on Pipes and Sockets in lseek

**Learning:** POSIX.1-2017 and Linux `lseek(2)` specifications require `lseek` on non-seekable file descriptions (pipes, FIFOs, and sockets) to return `-ESPIPE` (`-29`). Previously, `FileDescription::seek` allowed seeks on `FileType::Pipe` and `FileType::Socket` to modify file offsets and return success instead of returning `-ESPIPE`.
**Action:** Always check `file_type` in `FileDescription::seek` and return `Err(-29)` (`-ESPIPE`) for non-seekable streams (`FileType::Pipe` and `FileType::Socket`).

## 2026-03-30 - Permitting Signal Disposition Querying for SIGKILL and SIGSTOP in sigaction

**Learning:** POSIX.1-2017 and Linux `sigaction(2)` specifications state that `SIGKILL` (9) and `SIGSTOP` (19) cannot be caught or ignored, but querying their current signal disposition (`act == NULL`) is valid and allowed. Previously, `sys_rt_sigaction` unconditionally returned `-EINVAL` whenever `signum == 9 || signum == 19`, preventing applications from reading signal dispositions via `oldact`.
**Action:** In `sys_rt_sigaction`, check `!act.is_null() && (signum == 9 || signum == 19)` to only reject attempts to change signal handlers for `SIGKILL` or `SIGSTOP`.
