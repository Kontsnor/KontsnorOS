// tools/init-ubuntu.c
// Freestanding Init daemon (PID 1) specifically for Ubuntu on KontsnorOS.

#define NULL ((void*)0)

// System Call Wrappers
static inline long syscall0(long num) {
    long ret;
    __asm__ __volatile__(
        "syscall"
        : "=a"(ret)
        : "a"(num)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static inline long syscall1(long num, long arg1) {
    long ret;
    __asm__ __volatile__(
        "syscall"
        : "=a"(ret)
        : "a"(num), "D"(arg1)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static inline long syscall2(long num, long arg1, long arg2) {
    long ret;
    __asm__ __volatile__(
        "syscall"
        : "=a"(ret)
        : "a"(num), "D"(arg1), "S"(arg2)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static inline long syscall3(long num, long arg1, long arg2, long arg3) {
    long ret;
    __asm__ __volatile__(
        "syscall"
        : "=a"(ret)
        : "a"(num), "D"(arg1), "S"(arg2), "d"(arg3)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static inline long syscall4(long num, long arg1, long arg2, long arg3, long arg4) {
    long ret;
    register long r10 __asm("r10") = arg4;
    __asm__ __volatile__(
        "syscall"
        : "=a"(ret)
        : "a"(num), "D"(arg1), "S"(arg2), "d"(arg3), "r"(r10)
        : "rcx", "r11", "memory"
    );
    return ret;
}

// Robust sys_fork clobbering all scratch registers
static inline long sys_fork(void) {
    long ret;
    __asm__ __volatile__(
        "mov $57, %%rax\n"
        "syscall\n"
        : "=a"(ret)
        :
        : "rcx", "r11", "rbx", "rdi", "rsi", "rdx", "r8", "r9", "r10", "r12", "r13", "r14", "r15", "memory"
    );
    return ret;
}

// Minimal String Utilities
static inline unsigned long my_strlen(const char *s) {
    unsigned long len = 0;
    while (s[len]) len++;
    return len;
}

static inline void print(const char *s) {
    syscall3(1, 1, (long)s, my_strlen(s)); // write(1, s, len)
}

static inline void print_err(const char *s) {
    syscall3(1, 2, (long)s, my_strlen(s)); // write(2, s, len)
}

static inline void print_hex(unsigned long val) {
    char buf[32];
    int idx = 30;
    buf[31] = '\0';
    if (val == 0) {
        buf[idx--] = '0';
    } else {
        while (val > 0) {
            int digit = val % 16;
            if (digit < 10) {
                buf[idx--] = '0' + digit;
            } else {
                buf[idx--] = 'a' + (digit - 10);
            }
            val /= 16;
        }
    }
    buf[idx--] = 'x';
    buf[idx] = '0';
    print(&buf[idx]);
}

// Entry Point
void _start(void) {
    print("\n\033[1;36m");
    print("╔═══════════════════════════════════════════════════════════╗\n");
    print("║          Welcome to Ubuntu 24.04 LTS on KontsnorOS        ║\n");
    print("║     Native Hybrid Kernel Architecture & Linux Emulation   ║\n");
    print("╚═══════════════════════════════════════════════════════════╝\n");
    print("\033[0m\n");
    print("[init-ubuntu] PID 1 initialized. Preparing Ubuntu user-space...\n");

    while (1) {
        print("[init-ubuntu] Spawning interactive Ubuntu shell...\n");

        long pid = sys_fork();
        if (pid < 0) {
            print_err("[init-ubuntu] Error: fork failed\n");
            for (volatile int i = 0; i < 50000000; i++);
            continue;
        }

        if (pid == 0) {
            // Child process: setup tty slave
            long fd = syscall3(2, (long)"/dev/pts/0", 2, 0); // open(..., O_RDWR)
            if (fd >= 0) {
                syscall2(33, fd, 0); // dup2(fd, 0)
                syscall2(33, fd, 1); // dup2(fd, 1)
                syscall2(33, fd, 2); // dup2(fd, 2)
                if (fd > 2) {
                    syscall1(3, fd); // close(fd)
                }
            }

            // Setup process group, session, and terminal foreground
            syscall0(112); // setsid()
            long child_pid = syscall0(39); // getpid()
            syscall2(109, 0, child_pid); // setpgid(0, child_pid)
            syscall3(16, 0, 0x5410, (long)&child_pid); // ioctl(0, TIOCSPGRP, &child_pid)

            char *argv_bash[] = { "/bin/bash", "-i", NULL };
            char *argv_sh[] = { "/bin/sh", NULL };
            char *envp[] = {
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "HOME=/root",
                "USER=root",
                "LOGNAME=root",
                "TERM=xterm-256color",
                "SHELL=/bin/bash",
                NULL
            };

            // Try executing Ubuntu bash
            syscall3(59, (long)"/bin/bash", (long)argv_bash, (long)envp);
            syscall3(59, (long)"/usr/bin/bash", (long)argv_bash, (long)envp);

            // Fallback to dash / sh
            syscall3(59, (long)"/bin/sh", (long)argv_sh, (long)envp);
            syscall3(59, (long)"/usr/bin/sh", (long)argv_sh, (long)envp);

            print_err("[init-ubuntu] Error: Failed to execute shell!\n");
            syscall1(60, 127); // exit(127)
        }

        // Parent process (PID 1): harvest zombie processes
        while (1) {
            int wstatus = 0;
            // wait4(-1, &wstatus, 0, NULL)
            long reaped = syscall4(61, -1, (long)&wstatus, 0, 0);

            if (reaped < 0) {
                for (volatile int i = 0; i < 100000; i++);
                continue;
            }

            // If main shell process exited, respawn
            if (reaped == pid) {
                print("[init-ubuntu] Shell exited. Respawning...\n");
                break;
            }
        }
    }
}
