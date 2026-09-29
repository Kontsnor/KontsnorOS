# Bridge's Journal - Critical Learnings

## 2026-03-31 - sys_timerfd_gettime (syscall #284)
**Learning:** `timerfd_gettime` (x86_64 syscall #284) requires checking user pointer validity (`validate_user_ptr_write`) for `curr_value: *mut Itimerspec`, verifying the descriptor points to a valid `TimerFd` (returning `-EBADF` or `-EINVAL`), and locking timer state in an interrupt-disabled critical section (`without_interrupts`) to convert remaining ticks and interval back to `Itimerspec`.
**Action:** Re-use `ticks_to_timespec` and `without_interrupts` when querying timerfd state to prevent race conditions with the timer interrupt handler.
