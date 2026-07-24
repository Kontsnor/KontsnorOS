// tools/init.c
// Freestanding Init daemon (PID 1) for KontsnorOS.

#define NULL ((void*)0)

// System Call Wrappers
long syscall0(long num) {
    long ret;
    __asm__ __volatile__(
        "syscall"
        : "=a"(ret)
        : "a"(num)
        : "rcx", "r11", "memory"
    );
    return ret;
}

long syscall1(long num, long arg1) {
    long ret;
    __asm__ __volatile__(
        "syscall"
        : "=a"(ret)
        : "a"(num), "D"(arg1)
        : "rcx", "r11", "memory"
    );
    return ret;
}

long syscall2(long num, long arg1, long arg2) {
    long ret;
    __asm__ __volatile__(
        "syscall"
        : "=a"(ret)
        : "a"(num), "D"(arg1), "S"(arg2)
        : "rcx", "r11", "memory"
    );
    return ret;
}

long syscall3(long num, long arg1, long arg2, long arg3) {
    long ret;
    __asm__ __volatile__(
        "syscall"
        : "=a"(ret)
        : "a"(num), "D"(arg1), "S"(arg2), "d"(arg3)
        : "rcx", "r11", "memory"
    );
    return ret;
}

long syscall4(long num, long arg1, long arg2, long arg3, long arg4) {
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

// Robust sys_fork to clobber all general-purpose registers
long sys_fork() {
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
int strlen(const char *s) {
    int len = 0;
    while (s[len]) len++;
    return len;
}

void print(const char *s) {
    syscall3(1, 1, (long)s, strlen(s)); // write(1, s, len)
}

void print_err(const char *s) {
    syscall3(1, 2, (long)s, strlen(s)); // write(2, s, len)
}

void print_hex(unsigned long val) {
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
void _start() {
    print("\n[init] Proper Init System (PID 1) started.\n");

    while (1) {
        print("[init] Spawning interactive shell...\n");

        long pid = sys_fork();
        if (pid < 0) {
            print_err("[init] Error: fork failed\n");
            // Delay before retrying
            for (volatile int i = 0; i < 50000000; i++);
            continue;
        }

        if (pid == 0) {
            // Child process: execute shell
            // Redirect stdin/stdout/stderr to /dev/pts/0 just in case
            long fd = syscall3(2, (long)"/dev/pts/0", 2, 0); // open(..., O_RDWR)
            if (fd >= 0) {
                syscall2(33, fd, 0); // dup2(fd, 0)
                syscall2(33, fd, 1); // dup2(fd, 1)
                syscall2(33, fd, 2); // dup2(fd, 2)
                if (fd > 2) {
                    syscall1(3, fd); // close(fd)
                }
            } else {
                print_err("[init] Warning: Failed to open /dev/pts/0 in child\n");
            }

            char *argv_bash[] = { "/bin/bash", "/build_cargo.sh", NULL };
            char *argv_sh[] = { "/bin/sh", "/build_cargo.sh", NULL };
            char *envp[] = { "LD_PRELOAD=/lib/libstubs.so", NULL };

            // Try execve("/bin/bash")
            syscall3(59, (long)"/bin/bash", (long)argv_bash, (long)envp);

            // Fallback to execve("/bin/sh")
            syscall3(59, (long)"/bin/sh", (long)argv_sh, (long)envp);

            print_err("[init] Error: Failed to execute shell!\n");
            syscall1(60, 127); // exit(127)
        }

        // Parent process (PID 1): harvest zombie children
        print("[init] Adopted and harvesting zombie loop started.\n");
        while (1) {
            int wstatus = 0;
            // wait4(-1, &wstatus, 0, NULL) -> wait for any child
            long reaped = syscall4(61, -1, (long)&wstatus, 0, 0);
            
            if (reaped < 0) {
                // If no children are left, wait4 returns ECHILD (-10)
                if (reaped == -10) {
                    break;
                }
                // Yield or pause briefly if error to avoid pegging CPU
                for (volatile int i = 0; i < 100000; i++);
                continue;
            }

            print("[init] Reaped child PID ");
            print_hex(reaped);
            print("\n");

            // If the reaped child was our main interactive shell, respawn it
            if (reaped == pid) {
                print("[init] Interactive shell process exited. Respawning shell...\n");
                break;
            }
        }
    }
}
