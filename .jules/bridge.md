# Bridge's Journal - Critical Learnings

## 2026-03-31 - timerfd_gettime (syscall #284)
**Learning:** `timerfd_gettime` retrieves the current interval and remaining time for an active `TimerFd`. It shares internal tick calculation logic (`ticks_to_timespec`) with `timerfd_settime`'s `old_value` query, ensuring consistency in Linux ABI behavior across timer descriptor queries.
**Action:** When implementing remaining-time queries for timer syscalls, reuse `without_interrupts` blocks to atomically read `expiration_ticks` and `interval_ticks` against `timer_ticks()`, and validate output pointers via `validate_user_ptr_write`.
