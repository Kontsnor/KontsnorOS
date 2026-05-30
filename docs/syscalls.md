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

## File Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 0 | read | `read(fd, buf, count)` | Read from file descriptor |
| 1 | write | `write(fd, buf, count)` | Write to file descriptor |
| 2 | open | `open(path, flags, mode)` | Open a file |
| 3 | close | `close(fd)` | Close a file descriptor |
| 4 | stat | `stat(path, statbuf)` | Get file status |
| 5 | fstat | `fstat(fd, statbuf)` | Get file status by fd |
| 8 | lseek | `lseek(fd, offset, whence)` | Reposition file offset |

## Memory Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 9 | mmap | `mmap(addr, len, prot, flags, fd, off)` | Map memory |
| 11 | munmap | `munmap(addr, len)` | Unmap memory |
| 12 | brk | `brk(addr)` | Change data segment size |

## Process Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 39 | getpid | `getpid()` | Get process ID |
| 57 | fork | `fork()` | Create child process |
| 59 | execve | `execve(path, argv, envp)` | Execute program |
| 60 | exit | `exit(status)` | Terminate process |
| 61 | wait4 | `wait4(pid, status, opts, rusage)` | Wait for child |

## Signal Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 62 | kill | `kill(pid, sig)` | Send signal |

## I/O Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 16 | ioctl | `ioctl(fd, request, arg)` | Device control |
| 22 | pipe | `pipe(pipefd)` | Create pipe |
| 32 | dup | `dup(oldfd)` | Duplicate fd |
| 33 | dup2 | `dup2(oldfd, newfd)` | Duplicate fd to specific number |

## Directory Operations

| Number | Name | Signature | Description |
|--------|------|-----------|-------------|
| 79 | getcwd | `getcwd(buf, size)` | Get working directory |
| 80 | chdir | `chdir(path)` | Change directory |
| 83 | mkdir | `mkdir(path, mode)` | Create directory |
| 84 | rmdir | `rmdir(path)` | Remove directory |

## Error Codes (errno)

| Code | Name | Description |
|------|------|-------------|
| -1 | EPERM | Operation not permitted |
| -2 | ENOENT | No such file or directory |
| -3 | ESRCH | No such process |
| -9 | EBADF | Bad file descriptor |
| -12 | ENOMEM | Cannot allocate memory |
| -13 | EACCES | Permission denied |
| -14 | EFAULT | Bad address |
| -22 | EINVAL | Invalid argument |
| -38 | ENOSYS | Function not implemented |
