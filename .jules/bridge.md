## 2026-03-31 - Timerfd Gettime Implementation

**Learning:** `timerfd_gettime` (x86_64 syscall #284) requires validating the output pointer `curr_value` using `validate_user_ptr_write` for `size_of::<Itimerspec>()`, returning `-EFAULT` if invalid/null, `-EBADF` if the file descriptor is not open, and `-EINVAL` if the file descriptor is not a `TimerFd`. Interrupts must be disabled (`without_interrupts`) when reading timer ticks to ensure consistent snapshot readings of `expiration_ticks` and `interval_ticks`.

**Action:** When implementing clock/timer query syscalls, ensure output user-space pointers are validated using `validate_user_ptr_write` before taking lock guards or interrupt-disabled state snapshots.
