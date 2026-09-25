# Bridge's Journal - Critical Learnings & Linux ABI Insights

## 2026-03-31 - prctl(PR_SET_NAME / PR_GET_NAME) Implementation & Task Naming
**Learning:** Linux `prctl(PR_SET_NAME, name)` sets the process name using a null-terminated buffer of up to 16 bytes (including the trailing null byte). `PR_GET_NAME` expects a 16-byte user buffer into which the kernel copies the task name, truncated to 15 characters plus a trailing null byte. Defensive pointer validation (`validate_user_ptr` and `validate_user_ptr_write`) must be enforced for exactly 16 bytes on both set and get operations.
**Action:** When expanding `prctl` sub-operations, always validate user memory pointers for the expected option buffer size and ensure task name strings are correctly null-terminated or zero-padded up to 16 bytes.
