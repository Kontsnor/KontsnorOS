// tools/init-container.c
// Automated Init daemon for KontsnorOS Container Test.
// Spawns the container runtime, monitors execution, and halts QEMU on completion.

#define NULL ((void*)0)

// System Call Wrappers
static inline long syscall0(long num) {
    long ret;
    __asm__ __volatile__("syscall" : "=a"(ret) : "a"(num) : "rcx", "r11", "memory");
    return ret;
}

static inline long syscall1(long num, long a1) {
    long ret;
    __asm__ __volatile__("syscall" : "=a"(ret) : "a"(num), "D"(a1) : "rcx", "r11", "memory");
    return ret;
}

static inline long syscall2(long num, long a1, long a2) {
    long ret;
    __asm__ __volatile__("syscall" : "=a"(ret) : "a"(num), "D"(a1), "S"(a2) : "rcx", "r11", "memory");
    return ret;
}

static inline long syscall3(long num, long a1, long a2, long a3) {
    long ret;
    __asm__ __volatile__("syscall" : "=a"(ret) : "a"(num), "D"(a1), "S"(a2), "d"(a3) : "rcx", "r11", "memory");
    return ret;
}

static inline long syscall4(long num, long a1, long a2, long a3, long a4) {
    long ret;
    register long r10 __asm("r10") = a4;
    __asm__ __volatile__("syscall" : "=a"(ret) : "a"(num), "D"(a1), "S"(a2), "d"(a3), "r"(r10) : "rcx", "r11", "memory");
    return ret;
}

static inline long sys_fork(void) {
    long ret;
    __asm__ __volatile__(
        "mov $57, %%rax\n"
        "syscall\n"
        : "=a"(ret) : : "rcx", "r11", "rbx", "rdi", "rsi", "rdx", "r8", "r9", "r10", "r12", "r13", "r14", "r15", "memory"
    );
    return ret;
}

static inline unsigned long strlen(const char *s) {
    unsigned long len = 0;
    while (s && s[len]) len++;
    return len;
}

static inline void print(const char *s) {
    syscall3(1, 1, (long)s, strlen(s)); // write(1, s, len)
}

static inline void print_num(long val) {
    char buf[32];
    int idx = 30;
    buf[31] = '\0';
    if (val == 0) {
        buf[idx--] = '0';
    } else {
        long v = val < 0 ? -val : val;
        while (v > 0) {
            buf[idx--] = '0' + (v % 10);
            v /= 10;
        }
        if (val < 0) buf[idx--] = '-';
    }
    print(&buf[idx + 1]);
}

void _start(void) {
    print("\n[init-container] KontsnorOS Automated Container Test Harness (PID 1)\n");

    long pid = sys_fork();
    if (pid < 0) {
        print("[init-container] Error: fork failed\n");
        syscall1(60, 1);
    }

    if (pid == 0) {
        // Child: route stdio to /dev/pts/0
        long fd = syscall3(2, (long)"/dev/pts/0", 2, 0); // open(..., O_RDWR)
        if (fd >= 0) {
            syscall2(33, fd, 0); // dup2(fd, 0)
            syscall2(33, fd, 1); // dup2(fd, 1)
            syscall2(33, fd, 2); // dup2(fd, 2)
            if (fd > 2) syscall1(3, fd);
        }

        char *argv[] = {
            "/bin/ctr_run",
            "/containers/alpine",
            "/bin/sh",
            "/test_inside.sh",
            NULL
        };
        char *envp[] = {
            "PATH=/bin:/usr/bin:/sbin:/usr/sbin",
            "HOME=/root",
            NULL
        };

        print("[init-container] Spawning container runtime (/bin/ctr_run)...\n");
        syscall3(59, (long)"/bin/ctr_run", (long)argv, (long)envp);

        print("[init-container] Error: execve(/bin/ctr_run) failed!\n");
        syscall1(60, 127);
    }

    // Parent (PID 1): wait for container runner to exit
    int wstatus = 0;
    long reaped = syscall4(61, pid, (long)&wstatus, 0, 0); // wait4
    print("[init-container] Container runtime exited with status ");
    print_num(wstatus);
    print("\n");

    if (wstatus == 0) {
        print("\n=========================================\n");
        print("  [CONTAINER_TEST_SUCCESS] ALL PASSED!\n");
        print("=========================================\n\n");
    } else {
        print("\n=========================================\n");
        print("  [CONTAINER_TEST_FAILED] Non-zero status\n");
        print("=========================================\n\n");
    }

    // Allow output queues (PTY/serial) to flush cleanly before triggering poweroff
    for (volatile int i = 0; i < 50; i++) {
        syscall0(24); // sched_yield
    }

    // Power off QEMU cleanly via reboot(LINUX_REBOOT_MAGIC1, LINUX_REBOOT_MAGIC2, LINUX_REBOOT_CMD_POWER_OFF)
    print("[init-container] Powering off system...\n");
    for (volatile int i = 0; i < 10; i++) {
        syscall0(24);
    }
    syscall4(169, 0xfee1dead, 672274793, 0x4321fedc, 0);

    // Fallback infinite loop if poweroff fails
    while (1) {
        syscall0(24); // sched_yield
    }
}
