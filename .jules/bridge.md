# Bridge's Journal

## 2026-03-30 - prctl PR_SET_NAME / PR_GET_NAME Support
**Learning:** In KontsnorOS `sys_prctl` (`kernel/src/syscall/process/lifecycle.rs`), `PR_SET_NAME` (15) validates user string memory (`arg2`) for 16 bytes using `validate_user_ptr_read` before updating task `name`, while `PR_GET_NAME` (16) validates output memory for 16 bytes using `validate_user_ptr_write` and copies the task name formatted as a 16-byte null-terminated string (max 15 chars + null byte). Unsupported options return `-EINVAL` (`Errno::EINVAL`).
**Action:** Always validate input and output user memory pointers using `validate_user_ptr_read` and `validate_user_ptr_write` in system calls, and return `-EINVAL` for unsupported sub-operations.
