# Bridge's Journal

## 2026-03-31 - Legacy x86_64 EventFd and SignalFd Syscalls
**Learning:** Legacy x86_64 Linux system calls `sys_eventfd` (syscall #284) and `sys_signalfd` (syscall #282) are 1-argument (`initval`) and 3-argument (`fd`, `mask`, `sizemask`) calls respectively, which default `flags = 0` compared to `sys_eventfd2` (#290) and `sys_signalfd4` (#289). Standard C runtimes (glibc/musl) invoke these legacy syscall variants when no flags (e.g., `EFD_CLOEXEC`, `SFD_NONBLOCK`) are specified by application code.
**Action:** Always map legacy 3-arg or 1-arg syscall variants directly to their flags-accepting modern counterparts with `flags = 0` when wiring syscall numbers in `kernel/src/syscall/mod.rs`.
