# KontsnorOS Syscall Reference (Linux ABI Compatible)

## Overview

KontsnorOS implements a standard **Linux Application Binary Interface (ABI)** system call interface on `x86_64`. Applications compiled for Linux (against either `glibc` or `musl`) execute their system calls natively without emulation wrappers or binary translation.

### System V ABI Calling Convention

| Register | Direction | Purpose |
|:---|:---|:---|
| **`rax`** | Input | Syscall Number |
| **`rdi`** | Input | Argument 1 |
| **`rsi`** | Input | Argument 2 |
| **`rdx`** | Input | Argument 3 |
| **`r10`** | Input | Argument 4 *(Kernel syscall convention: `r10` instead of `rcx`)* |
| **`r8`**  | Input | Argument 5 |
| **`r9`**  | Input | Argument 6 |
| **`rax`** | Output | Return value (`>= 0` on success; `[-4095, -1]` corresponds to `-errno`) |

### Fast Path Execution (`gs:[16]`)
Simple non-yielding inquiries (`getpid`, `getuid`, `getgid`, `geteuid`, `getegid`, `getppid`, `getpgrp`, `gettid`, `set_tid_address`) execute via the assembly fast path (`syscall_fast_dispatch`). The active PID/TID is resolved in a single CPU cycle directly from the core's local scratch register (`gs:[16]`), bypassing the scheduler lock and preserving user execution momentum.

---

## Implemented System Call Reference

The following tables document the 150+ actively handled Linux system calls in KontsnorOS.

### 1. File & Directory Operations

| Number | Name | Implementation | Description |
|:---|:---|:---|:---|
| 0 | `read` | **Real** | Read bytes from file descriptor into user buffer |
| 1 | `write` | **Real** | Write bytes from buffer to file descriptor |
| 2 | `open` | **Real** | Open file with flags (`O_RDONLY`, `O_WRONLY`, `O_RDWR`, `O_CREAT`, etc.) |
| 3 | `close` | **Real** | Close file descriptor and release table entry |
| 4 | `stat` | **Real** | Retrieve file metadata by pathname (`LinuxStat`) |
| 5 | `fstat` | **Real** | Retrieve file metadata by open file descriptor |
| 6 | `lstat` | **Real** | Retrieve symbolic link or target metadata |
| 7 | `poll` | **Real** | Wait for file descriptor events with millisecond timeout |
| 8 | `lseek` | **Real** | Reposition file read/write offset (`SEEK_SET`, `CUR`, `END`) |
| 16 | `ioctl` | **Real** | Device control (`TIOCGWINSZ`, `TIOCSCTTY`, `TIOCSPGRP`, etc.) |
| 17 | `pread64` | **Real** | Read from specific offset without changing fd file pointer |
| 18 | `pwrite64` | **Real** | Write to specific offset without changing fd file pointer |
| 19 | `readv` | **Real** | Scatter-gather read into vector array |
| 20 | `writev` | **Real** | Scatter-gather write from vector array |
| 21 | `access` | **Real** | Check file accessibility (`R_OK`, `W_OK`, `X_OK`, `F_OK`) |
| 22 | `pipe` | **Real** | Create unidirectional IPC pipe channel pair |
| 32 | `dup` | **Real** | Duplicate file descriptor to lowest available slot |
| 33 | `dup2` | **Real** | Duplicate file descriptor to target number |
| 40 | `sendfile` | **Real** | Zero-copy data transfer between file descriptors |
| 72 | `fcntl` | **Real** | Manipulate file descriptor flags (`F_GETFD`, `F_SETFD`, `O_NONBLOCK`) |
| 73 | `flock` | **Real** | Apply or remove advisory file lock |
| 74 | `fsync` | **Real** | Synchronize file dirty page cache and blocks to disk |
| 75 | `fdatasync` | **Real** | Synchronize file data blocks to disk |
| 76 | `truncate` | **Real** | Truncate file to specified length by path |
| 77 | `ftruncate` | **Real** | Truncate file to specified length by descriptor |
| 79 | `getcwd` | **Real** | Retrieve current working directory path |
| 80 | `chdir` | **Real** | Change process current working directory |
| 81 | `fchdir` | **Real** | Change current working directory via open descriptor |
| 82 | `rename` | **Real** | Rename or move file atomically |
| 83 | `mkdir` | **Real** | Create directory with specified permissions |
| 84 | `rmdir` | **Real** | Remove empty directory |
| 85 | `creat` | **Real** | Create and open file (`O_CREAT | O_WRONLY | O_TRUNC`) |
| 86 | `link` | **Real** | Create hard link to existing file |
| 87 | `unlink` | **Real** | Remove directory entry and decrement inode link count |
| 88 | `symlink` | **Real** | Create symbolic link |
| 89 | `readlink` | **Real** | Read symbolic link target path |
| 90 | `chmod` | **Real** | Modify file mode / permissions |
| 91 | `fchmod` | **Real** | Modify file mode by descriptor |
| 92 | `chown` | **Real** | Change file owner and group |
| 93 | `fchown` | **Real** | Change file owner by descriptor |
| 94 | `lchown` | **Real** | Change symbolic link owner |
| 95 | `umask` | **Real** | Set and retrieve file creation mask |
| 132 | `utime` | **Real** | Modify inode access and modification times |
| 137 | `statfs` | **Real** | Retrieve filesystem statistics by pathname |
| 138 | `fstatfs` | **Real** | Retrieve filesystem statistics by descriptor |
| 155 | `pivot_root`| **Real** | Change root filesystem (used by container runtime) |
| 161 | `chroot` | **Real** | Change process root directory |
| 162 | `sync` | **Real** | Commit all dirty filesystem buffers to disk |
| 165 | `mount` | **Real** | Mount filesystem (`procfs`, `sysfs`, `devpts`, `tmpfs`, `ext2`) |
| 166 | `umount2` | **Real** | Unmount mounted filesystem |
| 187 | `readahead`| **Real** | Initiate page cache prefetching |
| 188..199| `*xattr` | **Real** | Extended attributes get, set, list, and remove |
| 213 | `epoll_create`| **Real** | Create epoll instance |
| 217 | `getdents64`| **Real** | Read directory entries in 64-bit Linux dirent format |
| 221 | `fadvise64`| **Compat** | File access pattern advisory (returns success) |
| 232 | `epoll_wait`| **Real** | Wait for I/O events on epoll descriptor |
| 233 | `epoll_ctl` | **Real** | Control epoll interest list (`EPOLL_CTL_ADD`, `MOD`, `DEL`) |
| 235 | `utimes` | **Real** | Modify file timestamps with microsecond resolution |
| 253 | `inotify_init`| **Real**| Initialize inotify instance |
| 254 | `inotify_add_watch`| **Real**| Add watch for filesystem events |
| 255 | `inotify_rm_watch`| **Real**| Remove watch descriptor |
| 257 | `openat` | **Real** | Open file relative to directory descriptor |
| 258 | `mkdirat` | **Real** | Create directory relative to directory descriptor |
| 260 | `fchownat` | **Real** | Change ownership relative to directory descriptor |
| 262 | `newfstatat`| **Real** | Stat file relative to directory descriptor |
| 263 | `unlinkat` | **Real** | Remove directory entry relative to directory descriptor |
| 264 | `renameat` | **Real** | Rename file relative to directory descriptors |
| 265 | `linkat` | **Real** | Create hard link relative to directory descriptors |
| 266 | `symlinkat`| **Real** | Create symbolic link relative to directory descriptor |
| 267 | `readlinkat`| **Real** | Read link target relative to directory descriptor |
| 268 | `fchmodat` | **Real** | Modify permissions relative to directory descriptor |
| 269 | `faccessat`| **Real** | Check accessibility relative to directory descriptor |
| 271 | `ppoll` | **Real** | Precision poll with signal mask |
| 275 | `splice` | **Real** | Zero-copy data pipe splice |
| 276 | `tee` | **Real** | Duplicate pipe content |
| 277 | `sync_file_range`| **Real**| Sync file sector ranges |
| 278 | `vmsplice` | **Real** | Splice user pages into pipe |
| 280 | `utimensat`| **Real** | Modify timestamps with nanosecond resolution |
| 281 | `epoll_pwait`| **Real**| Epoll wait with signal mask |
| 283 | `timerfd_create`| **Real**| Create timer file descriptor |
| 285 | `fallocate`| **Real** | Manipulate file space allocation |
| 286,287| `timerfd_settime`| **Real**| Arm or disarm timerfd |
| 289 | `signalfd4`| **Real** | Create signal-receiving file descriptor |
| 290 | `eventfd2` | **Real** | Create waitable counter file descriptor |
| 291 | `epoll_create1`| **Real**| Create epoll instance with flags (`EPOLL_CLOEXEC`) |
| 292 | `dup3` | **Real** | Duplicate descriptor with `O_CLOEXEC` support |
| 293 | `pipe2` | **Real** | Create pipe with `O_NONBLOCK` / `O_CLOEXEC` flags |
| 294 | `inotify_init1`| **Real**| Inotify init with flags |
| 295 | `preadv` | **Real** | Vector read at offset |
| 296 | `pwritev` | **Real** | Vector write at offset |
| 306 | `syncfs` | **Real** | Synchronize filesystem containing descriptor |
| 316 | `renameat2`| **Real** | Atomic rename with flags (`RENAME_NOREPLACE`, `EXCHANGE`) |
| 319 | `memfd_create`| **Real**| Create anonymous memory file |
| 326 | `copy_file_range`| **Real**| Fast kernel-space file range copy |
| 327 | `preadv2` | **Real** | Vector read with flags |
| 328 | `pwritev2` | **Real** | Vector write with flags |
| 332 | `statx` | **Real** | Extended file status with attribute masks |
| 424 | `pidfd_send_signal`| **Real**| Send signal to process via pidfd |
| 434 | `pidfd_open`| **Real**| Obtain file descriptor for process |
| 436 | `close_range`| **Real**| Close range of file descriptors |
| 437 | `openat2` | **Real** | Extended openat with resolve flags |
| 438 | `pidfd_getfd`| **Real**| Duplicate descriptor from another task |
| 439 | `faccessat2`| **Real**| Extended faccessat |
| 441 | `epoll_pwait2`| **Real**| Epoll wait with nanosecond timeout |
| 452 | `fchmodat2` | **Real** | Modify permissions with flags (`AT_SYMLINK_NOFOLLOW`) |

---

### 2. Memory Operations

| Number | Name | Implementation | Description |
|:---|:---|:---|:---|
| 9 | `mmap` | **Real** | Memory map (`MAP_SHARED`, `MAP_PRIVATE`, `MAP_ANONYMOUS`, `MAP_FIXED`, `MAP_FIXED_NOREPLACE`) |
| 10 | `mprotect` | **Real** | Change memory protection flags (`PROT_READ`, `PROT_WRITE`, `PROT_EXEC`) |
| 11 | `munmap` | **Real** | Unmap virtual address space range and shoot down TLB |
| 12 | `brk` | **Real** | Expand or contract process heap data segment |
| 25 | `mremap` | **Real** | Remap and resize virtual memory mapping |
| 26 | `msync` | **Real** | Synchronize shared memory mapping changes to disk file |
| 27 | `mincore` | **Real** | Probe whether pages reside in physical RAM (returns ENOMEM for unmapped) |
| 28 | `madvise` | **Compat** | Advisory memory usage flags |
| 149 | `mlock` | **Real** | Pin memory pages in physical RAM |
| 150 | `munlock` | **Real** | Unpin memory pages |
| 151 | `mlockall` | **Real** | Pin entire process address space |
| 152 | `munlockall`| **Real** | Unpin process address space |
| 324 | `mprotect` | **Real** | Secondary mprotect mapping |
| 325 | `mlock2` | **Real** | Lock memory with flags |
| 329 | `pkey_mprotect`| **Real**| Memory protection with protection keys |

---

### 3. Process, Scheduling & Credential Operations

| Number | Name | Implementation | Description |
|:---|:---|:---|:---|
| 24 | `sched_yield`| **Real** | Cooperatively yield current CPU timeslice |
| 35 | `nanosleep`| **Real** | High-resolution sleep with interruptible EINTR wake-up |
| 36 | `getitimer`| **Real** | Get interval timer value |
| 37 | `alarm` | **Real** | Set SIGALRM delivery timer |
| 38 | `setitimer`| **Real** | Configure interval timer |
| 39 | `getpid` | **Fast Path** | Get current process ID from `gs:[16]` |
| 56 | `clone` | **Real** | Thread and process creation with stack, TLS, and TID pointers |
| 57 | `fork` | **Real** | Duplicate process address space with Copy-on-Write |
| 58 | `vfork` | **Real** | Create child process borrowing parent address space |
| 59 | `execve` | **Real** | Execute ELF binary with arguments, envp, and auxiliary vectors |
| 60 | `exit` | **Real** | Terminate active thread and record exit status |
| 61 | `wait4` | **Real** | Wait for child state change with options (`WNOHANG`, `WUNTRACED`) |
| 63 | `uname` | **Real** | Get kernel name ("KontsnorOS"), release, and machine info |
| 96 | `gettimeofday`| **Real**| Retrieve current wall-clock time |
| 97 | `getrlimit`| **Real** | Get resource limits |
| 98 | `getrusage`| **Real** | Get process resource usage statistics |
| 99 | `sysinfo` | **Real** | Get system memory and load statistics |
| 100 | `times` | **Real** | Get process user and system CPU times |
| 102 | `getuid` | **Fast Path** | Get real user ID |
| 104 | `getgid` | **Fast Path** | Get real group ID |
| 105 | `setuid` | **Real** | Set user ID |
| 106 | `setgid` | **Real** | Set group ID |
| 107 | `geteuid` | **Fast Path** | Get effective user ID |
| 108 | `getegid` | **Fast Path** | Get effective group ID |
| 109 | `setpgid` | **Real** | Set process group ID |
| 110 | `getppid` | **Fast Path** | Get parent process ID |
| 111 | `getpgrp` | **Fast Path** | Get process group ID |
| 112 | `setsid` | **Real** | Create new session and become group leader |
| 113 | `setreuid` | **Real** | Set real and effective user ID |
| 114 | `setregid` | **Real** | Set real and effective group ID |
| 115 | `getgroups`| **Real** | Get supplementary group list |
| 116 | `setgroups`| **Real** | Set supplementary group list |
| 117 | `setresuid`| **Real** | Set real, effective, and saved user IDs |
| 118 | `getresuid`| **Real** | Get real, effective, and saved user IDs |
| 119 | `setresgid`| **Real** | Set real, effective, and saved group IDs |
| 120 | `getresgid`| **Real** | Get real, effective, and saved group IDs |
| 121 | `getpgid` | **Real** | Get process group ID of target PID |
| 122 | `setfsuid` | **Real** | Set filesystem UID |
| 123 | `setfsgid` | **Real** | Set filesystem GID |
| 124 | `getsid` | **Real** | Get session ID |
| 125 | `capget` | **Real** | Get capability sets |
| 126 | `capset` | **Real** | Set capability sets |
| 131 | `sigaltstack`| **Real**| Configure alternate signal stack for Wine and exceptions |
| 135 | `personality`| **Real**| Set process execution domain |
| 140 | `getpriority`| **Real**| Get program scheduling priority |
| 141 | `setpriority`| **Real**| Set program scheduling priority |
| 142..148| `sched_*` | **Real**| Scheduler parameter and policy manipulation |
| 157 | `prctl` | **Real** | Process operations (`PR_SET_NAME`, `PR_GET_NAME`, `PR_SET_PDEATHSIG`) |
| 158 | `arch_prctl`| **Real**| Set/get architecture registers (`ARCH_SET_FS`, `ARCH_SET_GS`, etc.) |
| 160 | `setrlimit`| **Real** | Set resource limits |
| 169 | `reboot` | **Real** | System restart, poweroff, or sync |
| 170 | `sethostname`| **Real**| Set system hostname |
| 171 | `setdomainname`| **Real**| Set system domainname |
| 186 | `gettid` | **Fast Path** | Get thread ID |
| 201 | `time` | **Real** | Get time in seconds since Unix epoch |
| 202 | `futex` | **Real** | Fast userspace locking engine (`WAIT`, `WAKE`, `WAIT_BITSET`) |
| 203 | `sched_setaffinity`| **Real**| Set CPU execution affinity mask |
| 204 | `sched_getaffinity`| **Real**| Get CPU execution affinity mask |
| 218 | `set_tid_address`| **Fast Path**| Register user-space TID clearance address |
| 227 | `clock_settime`| **Real**| Set system clock time |
| 228 | `clock_gettime`| **Real**| Get high-precision clock time (`CLOCK_REALTIME`, `MONOTONIC`) |
| 229 | `clock_getres`| **Real**| Get clock resolution |
| 230 | `clock_nanosleep`| **Real**| Sleep on specific clock |
| 231 | `exit_group`| **Real** | Terminate all threads in thread group |
| 247 | `waitid` | **Real** | Extended wait for child process state change |
| 272 | `unshare` | **Real** | Disassociate process execution contexts (namespaces) |
| 273 | `set_robust_list`| **Real**| Register robust futex linked list head |
| 274 | `get_robust_list`| **Real**| Retrieve robust futex linked list head |
| 302 | `prlimit64`| **Real** | Get and set 64-bit resource limits |
| 309 | `getcpu` | **Real** | Determine current CPU core and NUMA node |
| 318 | `getrandom`| **Real** | Obtain cryptographically secure entropy bytes |
| 322 | `execveat` | **Real** | Execute program relative to directory descriptor |
| 334 | `rseq` | **Real** | Restartable sequences registration |
| 435 | `clone3` | **Real** | Extensible clone with `CloneArgs` |
| 449 | `futex_waitv`| **Real**| Wait simultaneously on multiple futex words |

---

### 4. Signal Operations

| Number | Name | Implementation | Description |
|:---|:---|:---|:---|
| 13 | `rt_sigaction` | **Real** | Register custom signal handler and flags (`SA_SIGINFO`, `SA_ONSTACK`) |
| 14 | `rt_sigprocmask`| **Real** | Block, unblock, or query signal masks |
| 15 | `rt_sigreturn` | **Real** | Restore user context from stack after signal handling |
| 34 | `pause` | **Real** | Suspend execution until signal delivery |
| 62 | `kill` | **Real** | Send signal to target process or process group |
| 127 | `rt_sigpending`| **Real** | Check for signals pending delivery |
| 128 | `rt_sigtimedwait`| **Real**| Synchronously accept signals with timeout |
| 129 | `rt_sigqueueinfo`| **Real**| Queue signal with custom siginfo payload |
| 130 | `rt_sigsuspend`| **Real** | Temporarily replace signal mask and wait for signal |
| 200 | `tkill` | **Real** | Send signal to specific thread |
| 234 | `tgkill` | **Real** | Send signal to specific thread within thread group |
| 297 | `rt_tgsigqueueinfo`| **Real**| Queue signal with siginfo to target thread group |

---

### 5. Network & Socket Operations

| Number | Name | Implementation | Description |
|:---|:---|:---|:---|
| 41 | `socket` | **Real** | Create communications endpoint (`AF_INET`, `SOCK_STREAM`, `SOCK_DGRAM`) |
| 42 | `connect` | **Real** | Initiate TCP handshake or bind UDP remote endpoint |
| 43 | `accept` | **Real** | Accept connection from passive listening queue |
| 44 | `sendto` | **Real** | Transmit packet data to destination address |
| 45 | `recvfrom` | **Real** | Receive packet data and capture remote address |
| 46 | `sendmsg` | **Real** | Transmit message with scatter-gather and ancillary data |
| 47 | `recvmsg` | **Real** | Receive message with ancillary data |
| 48 | `shutdown` | **Real** | Shut down transmission or reception on socket |
| 49 | `bind` | **Real** | Bind local IP address and port to socket |
| 50 | `listen` | **Real** | Place socket into passive listening state with backlog |
| 51 | `getsockname`| **Real** | Retrieve locally bound address of socket |
| 52 | `getpeername`| **Real** | Retrieve remote peer address of socket |
| 53 | `socketpair`| **Real** | Create pair of connected anonymous sockets |
| 54 | `setsockopt`| **Real** | Configure socket options (`SO_REUSEADDR`, `SO_RCVBUF`, `TCP_NODELAY`) |
| 55 | `getsockopt`| **Real** | Query socket options |
| 288 | `accept4` | **Real** | Accept connection with flags (`SOCK_NONBLOCK`, `SOCK_CLOEXEC`) |
| 299 | `recvmmsg` | **Real** | Receive multiple network messages in single call |
| 307 | `sendmmsg` | **Real** | Send multiple network messages in single call |

---

### 6. System V IPC & POSIX Message Queues

| Number | Name | Implementation | Description |
|:---|:---|:---|:---|
| 23 | `select` | **Real** | Synchronous I/O multiplexing with `fd_set` bitmasks |
| 29 | `shmget` | **Real** | Allocate System V shared memory segment |
| 30 | `shmat` | **Real** | Attach shared memory segment to process address space |
| 31 | `shmctl` | **Real** | Control shared memory segment |
| 64 | `semget` | **Real** | Allocate semaphore set |
| 65 | `semop` | **Real** | Perform semaphore operations atomically |
| 66 | `semctl` | **Real** | Semaphore control operations |
| 67 | `shmdt` | **Real** | Detach shared memory segment |
| 68 | `msgget` | **Real** | Open or create message queue |
| 69 | `msgsnd` | **Real** | Send message to message queue |
| 70 | `msgrcv` | **Real** | Receive message from message queue |
| 71 | `msgctl` | **Real** | Message queue control |
| 220 | `semtimedop`| **Real** | Semaphore operations with timeout |
| 240 | `mq_open` | **Real** | Open POSIX message queue |
| 241 | `mq_unlink`| **Real** | Remove POSIX message queue |
| 242 | `mq_timedsend`| **Real**| Send message with timeout |
| 243 | `mq_timedreceive`| **Real**| Receive message with timeout |
| 244 | `mq_notify`| **Real** | Register notification for message arrival |
| 245 | `mq_getsetattr`| **Real**| Query or set message queue attributes |
