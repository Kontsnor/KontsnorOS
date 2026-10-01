## 2026-03-31 - Implementation of `sys_timerfd_gettime` (syscall #284)
**Learning:** `timerfd_gettime` (syscall #284) is required by runtimes like Musl/Busybox to inspect armed or disarmed timer state. It requires validating the output pointer `curr_value` with `validate_user_ptr_write` and calculating the remaining ticks and interval in an interrupt-disabled section (`without_interrupts`).
**Action:** Always validate user-space output pointers up front, verify file descriptor types via `as_timerfd()`, and lock timer state atomically without interrupts to avoid race conditions.
