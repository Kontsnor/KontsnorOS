# KontsnorOS Syscall Reference

## Overview

KontsnorOS implements a POSIX-compatible syscall interface. Syscalls are
invoked via the `syscall` instruction on x86_64.

### Calling Convention

| Register | Purpose |
|----------|---------|
| rax | Syscall number |
| rdi | Argument 1 |
| rsi | Argument 2 |
| rdx | Argument 3 |
| r10 | Argument 4 |
| r8 | Argument 5 |
| r9 | Argument 6 |
| rax | Return value |

---

## System Call Tables

### File Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 0 | read | `read(fd, buf, count)` | Read from file descriptor |
| 1 | write | `write(fd, buf, count)` | Write to file descriptor |
| 2 | open | `open(path, flags, mode)` | Open a file |
| 3 | close | `close(fd)` | Close a file descriptor |
| 4 | stat | `stat(pathname, statbuf)` | Get file status |
| 5 | fstat | `fstat(fd, statbuf)` | Get file status by fd |
| 6 | lstat | `lstat(pathname, statbuf)` | Get file status (no symlink trace) |
| 8 | lseek | `lseek(fd, offset, whence)` | Reposition file offset |
| 17 | pread64 | `pread64(fd, buf, count, offset)` | Read at offset without changing fd seek offset |
| 21 | access | `access(pathname, mode)` | Check file accessibility |
| 82 | rename | `rename(oldpath, newpath)` | Rename a file |
| 86 | link | `link(oldpath, newpath)` | Create a hard link (stub) |
| 89 | readlink | `readlink(pathname, buf, bufsize)` | Read symbolic link value (stub) |
| 267 | readlinkat | `readlinkat(dirfd, pathname, buf, bufsize)` | Read symbolic link relative to fd (stub) |

### Memory Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 9 | mmap | `mmap(addr, len, prot, flags, fd, off)` | Map memory |
| 11 | munmap | `munmap(addr, len)` | Unmap memory |
| 12 | brk | `brk(addr)` | Change data segment size |

### Process Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 39 | getpid | `getpid()` | Get process ID |
| 56 | clone | `clone(flags, child_stack, parent_tidptr, child_tidptr, newtls)` | Create a child process/thread |
| 57 | fork | `fork()` | Create child process |
| 59 | execve | `execve(path, argv, envp)` | Execute program |
| 60 | exit | `exit(status)` | Terminate process |
| 61 | wait4 | `wait4(pid, status, opts, rusage)` | Wait for child |
| 109 | setpgid | `setpgid(pid, pgid)` | Set process group ID |
| 110 | getppid | `getppid()` | Get parent process ID |
| 112 | setsid | `setsid()` | Create session and set PGID |
| 121 | getpgid | `getpgid(pid)` | Get process group ID |
| 231 | exit_group | `exit_group(status)` | Terminate all threads in thread group |

### Signal Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 62 | kill | `kill(pid, sig)` | Send signal |

### I/O & Device Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 7 | poll | `poll(fds, nfds, timeout)` | Wait for events on file descriptors (stub) |
| 16 | ioctl | `ioctl(fd, request, arg)` | Device-specific control |
| 22 | pipe | `pipe(pipefd)` | Create pipe |
| 32 | dup | `dup(oldfd)` | Duplicate fd |
| 33 | dup2 | `dup2(oldfd, newfd)` | Duplicate fd to specific number |

### Directory Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 79 | getcwd | `getcwd(buf, size)` | Get working directory |
| 80 | chdir | `chdir(path)` | Change directory |
| 83 | mkdir | `mkdir(path, mode)` | Create directory |
| 84 | rmdir | `rmdir(path)` | Remove directory |

### Time & Limits Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 96 | gettimeofday | `gettimeofday(tv, tz)` | Get system time (stub) |
| 97 | getrlimit | `getrlimit(resource, rlim)` | Get resource limits (stub) |
| 99 | sysinfo | `sysinfo(info)` | Get system information (stub) |
| 100 | times | `times(buf)` | Get CPU times (stub) |
| 228 | clock_gettime | `clock_gettime(clockid, tp)` | Get clock time (stub) |

---

## Detailed System Call Reference

This section details the newly implemented Linux-compatible system calls.

### Process Lifecycle & Control

#### 1. `sys_clone` (56)
*   **Signature**: `sys_clone(flags: u64, child_stack: u64, parent_tidptr: *mut i32, child_tidptr: *mut i32, newtls: u64) -> SyscallResult`
*   **Description**: Creates a child process or thread. Clones the task, clones page tables, duplicates the file descriptor table (incrementing reference counts on active descriptors), replicates signal actions/masks, allocates a kernel stack, and sets up registers. If TLS creation flags are present, registers the TLS base.
*   **Arguments**:
    *   `flags`: Bitmask of creation options (e.g. `CLONE_VM`, `CLONE_FS`, etc.). If `CLONE_SETTLS` (`0x00080000`) is set, TLS base register FS_BASE is configured to `newtls`.
    *   `child_stack`: The stack pointer to assign to the child process.
    *   `parent_tidptr`: Address in the parent's user-space to write the child's TID (if `CLONE_PARENT_SETTID` `0x00100000` is set).
    *   `child_tidptr`: Address in the child's user-space to write the child's TID (if `CLONE_CHILD_SETTID` `0x01000000` is set).
    *   `newtls`: TLS descriptor base address.
*   **Return Value**:
    *   To parent: The child's process ID (PID) on success.
    *   To child: `0` on success.
*   **Error Codes**:
    *   `ESRCH`: The calling process task entry was not found in the scheduler.
    *   `ENOMEM`: Unable to clone parent page tables or allocate the kernel stack.
*   **Implementation State**: **Real** (supports custom task generation and context switching).

#### 2. `sys_exit_group` (231)
*   **Signature**: `sys_exit_group(status: i32) -> SyscallResult`
*   **Description**: Terminates all threads in the calling thread group.
*   **Arguments**:
    *   `status`: Exit code returned to the parent.
*   **Return Value**: Does not return.
*   **Implementation State**: **Real** (delegates to exiting the current active thread).

#### 3. `sys_getppid` (110)
*   **Signature**: `sys_getppid() -> SyscallResult`
*   **Description**: Returns the process ID of the parent of the calling process.
*   **Arguments**: None.
*   **Return Value**: The parent PID (or `0` if not found).
*   **Implementation State**: **Real**.

#### 4. `sys_setpgid` (109)
*   **Signature**: `sys_setpgid(pid: i32, pgid: i32) -> SyscallResult`
*   **Description**: Sets the process group ID of the process specified by `pid`. If `pid` is `0`, the process ID of the calling process is used. If `pgid` is `0`, the process group ID of the process specified by `pid` is set to the process ID.
*   **Arguments**:
    *   `pid`: Target process ID (0 for caller).
    *   `pgid`: Target process group ID (0 to match `pid`).
*   **Return Value**: `0` on success.
*   **Error Codes**:
    *   `ESRCH`: The target process specified by `pid` could not be found.
*   **Implementation State**: **Real**.

#### 5. `sys_getpgid` (121)
*   **Signature**: `sys_getpgid(pid: i32) -> SyscallResult`
*   **Description**: Returns the process group ID of the process specified by `pid`. If `pid` is `0`, returns the process group ID of the calling process.
*   **Arguments**:
    *   `pid`: Target process ID (0 for caller).
*   **Return Value**: The process group ID of the target process.
*   **Error Codes**:
    *   `ESRCH`: The process specified by `pid` could not be found.
*   **Implementation State**: **Real**.

#### 6. `sys_setsid` (112)
*   **Signature**: `sys_setsid() -> SyscallResult`
*   **Description**: Creates a new session if the calling process is not a process group leader. The calling process becomes the leader of the new session and the process group leader of the new process group. The process group ID and session ID are set to the PID of the calling process.
*   **Arguments**: None.
*   **Return Value**: The new session ID (equal to calling PID).
*   **Error Codes**:
    *   `ESRCH`: Calling process task was not found.
*   **Implementation State**: **Real**.

---

### Filesystem Operations

#### 7. `sys_stat` (4)
*   **Signature**: `sys_stat(pathname: *const u8, statbuf: *mut LinuxStat) -> SyscallResult`
*   **Description**: Returns information about a file pointed to by `pathname`.
*   **Arguments**:
    *   `pathname`: User-space pointer to null-terminated path string.
    *   `statbuf`: User-space pointer to `LinuxStat` structure.
*   **Return Value**: `0` on success.
*   **Error Codes**:
    *   `EFAULT`: Path or stat buffer pointer is invalid/unmapped.
    *   `ENOENT`: File does not exist.
*   **Implementation State**: **Real** (resolves path via VFS and populates standard ABI fields).

#### 8. `sys_lstat` (6)
*   **Signature**: `sys_lstat(pathname: *const u8, statbuf: *mut LinuxStat) -> SyscallResult`
*   **Description**: Identical to `sys_stat` but does not follow symbolic links.
*   **Implementation State**: **Real** (because symbolic links are currently unsupported in KontsnorOS, it behaves identically to `sys_stat`).

#### 9. `sys_access` (21)
*   **Signature**: `sys_access(pathname: *const u8, mode: i32) -> SyscallResult`
*   **Description**: Checks if the calling process can access the file `pathname` using the permissions in `mode`.
*   **Arguments**:
    *   `pathname`: User-space pointer to null-terminated path string.
    *   `mode`: Access mode bitmask: `F_OK` (0, existence), `R_OK` (4, read), `W_OK` (2, write), `X_OK` (1, execute).
*   **Return Value**: `0` on success.
*   **Error Codes**:
    *   `EFAULT`: pathname pointer is invalid/unmapped.
    *   `ENOENT`: File does not exist.
    *   `EACCES`: Permissions check failed.
*   **Implementation State**: **Real** (delegates to `sys_faccessat` using `AT_FDCWD` / `-100`).

#### 10. `sys_rename` (82)
*   **Signature**: `sys_rename(oldpath: *const u8, newpath: *const u8) -> SyscallResult`
*   **Description**: Renames a file or moves it within the filesystem.
*   **Arguments**:
    *   `oldpath`: User-space pointer to current path.
    *   `newpath`: User-space pointer to target path.
*   **Return Value**: `0` on success.
*   **Error Codes**:
    *   `EFAULT`: Pointers are invalid.
    *   `ENOENT`: Source file or destination parent directory not found.
    *   `ENOSPC`: No space left to create the new node.
*   **Implementation State**: **Real** (in `tmpfs`, moves files by copying contents to a new file and unlinking the source; directory renaming is not supported).

#### 11. `sys_link` (86)
*   **Signature**: `sys_link(oldpath: *const u8, newpath: *const u8) -> SyscallResult`
*   **Description**: Creates a new hard link.
*   **Return Value**: Returns error.
*   **Error Codes**:
    *   `EPERM`: Operation not permitted (hard links are unsupported).
*   **Implementation State**: **Stub**.

#### 12. `sys_readlink` (89)
*   **Signature**: `sys_readlink(pathname: *const u8, buf: *mut u8, bufsize: usize) -> SyscallResult`
*   **Description**: Reads the target value of a symbolic link.
*   **Return Value**: Returns error.
*   **Error Codes**:
    *   `EINVAL`: File is not a symbolic link (symlinks are unsupported).
*   **Implementation State**: **Stub**.

#### 13. `sys_readlinkat` (267)
*   **Signature**: `sys_readlinkat(dirfd: i32, pathname: *const u8, buf: *mut u8, bufsize: usize) -> SyscallResult`
*   **Description**: Reads symbolic link relative to directory file descriptor.
*   **Return Value**: Returns error.
*   **Error Codes**:
    *   `EINVAL`: Symlinks are unsupported.
*   **Implementation State**: **Stub**.

#### 14. `sys_pread64` (17)
*   **Signature**: `sys_pread64(fd: i32, buf: *mut u8, count: usize, offset: i64) -> SyscallResult`
*   **Description**: Reads up to `count` bytes from file descriptor `fd` starting at `offset`. The current file seek offset is not modified.
*   **Arguments**:
    *   `fd`: File descriptor.
    *   `buf`: User-space target buffer.
    *   `count`: Maximum bytes to read.
    *   `offset`: Starting offset in the file.
*   **Return Value**: Number of bytes read.
*   **Error Codes**:
    *   `EFAULT`: Target buffer is invalid/unmapped.
    *   `EBADF`: Descriptor `fd` is invalid or not open.
*   **Implementation State**: **Real**.

---

### Time, Resources & Limits

#### 15. `sys_gettimeofday` (96)
*   **Signature**: `sys_gettimeofday(tv: *mut TimeVal, tz: *mut TimeZone) -> SyscallResult`
*   **Description**: Obtains system time-of-day.
*   **Arguments**:
    *   `tv`: Pointer to `TimeVal` struct.
    *   `tz`: Pointer to `TimeZone` struct.
*   **Return Value**: `0` on success.
*   **Implementation State**: **Stub** (populates structures with 0; real wall-clock time requires RTC hardware driver integration).

#### 16. `sys_clock_gettime` (228)
*   **Signature**: `sys_clock_gettime(clockid: i32, tp: *mut TimeSpec) -> SyscallResult`
*   **Description**: Obtains specific clock time.
*   **Arguments**:
    *   `clockid`: Clock identifier.
    *   `tp`: Pointer to `TimeSpec` struct.
*   **Return Value**: `0` on success.
*   **Error Codes**:
    *   `EFAULT`: `tp` pointer is null.
*   **Implementation State**: **Stub** (populates structure with 0).

#### 17. `sys_times` (100)
*   **Signature**: `sys_times(buf: *mut Tms) -> SyscallResult`
*   **Description**: Obtains process and children CPU execution times.
*   **Arguments**:
    *   `buf`: Pointer to `Tms` struct.
*   **Return Value**: `0` on success.
*   **Implementation State**: **Stub** (populates structure with 0).

#### 18. `sys_getrlimit` (97)
*   **Signature**: `sys_getrlimit(resource: i32, rlim: *mut RLimit) -> SyscallResult`
*   **Description**: Retrieves limits on system resources.
*   **Arguments**:
    *   `resource`: Resource constant (e.g. `RLIMIT_STACK`).
    *   `rlim`: Pointer to `RLimit` struct.
*   **Return Value**: `0` on success.
*   **Error Codes**:
    *   `EFAULT`: `rlim` pointer is null.
*   **Implementation State**: **Stub** (returns default/unlimited resources; limits are not actively enforced by the scheduler).

#### 19. `sys_sysinfo` (99)
*   **Signature**: `sys_sysinfo(info: *mut SysInfo) -> SyscallResult`
*   **Description**: Obtains system statistics (load average, memory size, running processes).
*   **Arguments**:
    *   `info`: Pointer to `SysInfo` struct.
*   **Return Value**: `0` on success.
*   **Error Codes**:
    *   `EFAULT`: `info` pointer is null.
*   **Implementation State**: **Stub** (returns generous mocked hardware limits: 128MB RAM, 64MB free).

---

### Input/Output

#### 20. `sys_poll` (7)
*   **Signature**: `sys_poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> SyscallResult`
*   **Description**: Monitors file descriptors for I/O events.
*   **Arguments**:
    *   `fds`: Array of `PollFd` structures.
    *   `nfds`: Number of descriptors.
    *   `timeout`: Timeout in milliseconds.
*   **Return Value**: Number of descriptors ready for I/O.
*   **Implementation State**: **Stub** (immediately sets all `revents = events` for active descriptors and returns control).

#### 21. TTY `ioctl` Sub-commands
Invoked through the generic `sys_ioctl(fd, request, arg)` interface when the descriptor refers to a TTY character device:
*   `TCGETS` (`0x5401`): Reads current `Termios` settings into the user-space buffer `arg`.
*   `TCSETS` / `TCSETSW` / `TCSETSF` (`0x5402` / `0x5403` / `0x5404`): Sets the global `Termios` settings from the user-space buffer `arg`.
*   `TIOCGWINSZ` (`0x5413`): Reads current window size settings (hardcoded to 80 columns by 24 rows) into a `Winsize` structure at `arg`.
*   `TIOCGPGRP` (`0x540F`): Gets the foreground process group ID associated with the terminal (stubs to write `1` to `arg`).
*   `TIOCSPGRP` (`0x5410`): Sets the foreground process group ID associated with the terminal (stub, returns `0`).
*   **Implementation State**: **Real** (manages global terminal configurations via `TTY_TERMIOS`).

---

## Error Codes (errno)

System calls return negative values corresponding to the following standard POSIX error numbers:

| Code | Name | Description |
|------|------|-------------|
| -1 | EPERM | Operation not permitted |
| -2 | ENOENT | No such file or directory |
| -3 | ESRCH | No such process |
| -4 | EINTR | Interrupted system call |
| -9 | EBADF | Bad file descriptor |
| -12 | ENOMEM | Out of memory |
| -13 | EACCES | Permission denied |
| -14 | EFAULT | Bad address |
| -17 | EEXIST | File exists |
| -20 | ENOTDIR | Not a directory |
| -22 | EINVAL | Invalid argument |
| -28 | ENOSPC | No space left on device |
| -38 | ENOSYS | Function not implemented |
