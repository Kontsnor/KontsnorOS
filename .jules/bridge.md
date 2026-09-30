# Bridge's Journal - Critical Learnings Only

## 2026-03-31 - Legacy x86_64 Syscall Delegation for EventFd and SignalFd
**Learning:** Legacy x86_64 syscalls like `sys_eventfd` (syscall #284) and `sys_signalfd` (syscall #282) are 1-arg and 3-arg variants added in early Linux 2.6 kernels before `eventfd2` (#290) and `signalfd4` (#289) introduced flag arguments (such as `EFD_CLOEXEC` and `SFD_NONBLOCK`). Runtimes targeting older Linux ABIs invoke `#284` or `#282` directly.
**Action:** When implementing legacy syscall variants that lack flags, delegate directly to their updated 4-arg or 2-arg counterparts passing `flags = 0`.
