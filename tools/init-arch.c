// tools/init-arch.c
// Dedicated Init daemon for KontsnorOS Arch Linux Container Test.
//
// Flex: "I run arch btw (in a namespace with my own kernel in Qemu on Ubuntu on WSL2 on Windows 11)"

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

__attribute__((always_inline)) static inline unsigned long strlen(const char *s) {
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
    print("\n[init-arch] KontsnorOS Arch Linux Container Init (PID 1)\n");

    long pid = sys_fork();
    if (pid < 0) {
        print("[init-arch] Error: fork failed\n");
        syscall1(60, 1);
    }

    if (pid == 0) {
        // Child: route stdio to /dev/pts/0 if available
        long fd = syscall3(2, (long)"/dev/pts/0", 2, 0); // open(..., O_RDWR)
        if (fd >= 0) {
            syscall2(33, fd, 0); // dup2(fd, 0)
            syscall2(33, fd, 1); // dup2(fd, 1)
            syscall2(33, fd, 2); // dup2(fd, 2)
            if (fd > 2) syscall1(3, fd);
        }

        long ifd = syscall3(2, (long)"/interactive", 0, 0); // open(..., O_RDONLY)
        char *argv_test[] = {
            "/bin/ctr_run",
            "/containers/arch",
            "/bin/sh",
            "/test_arch.sh",
            NULL
        };
        char *argv_interactive[] = {
            "/bin/ctr_run",
            "/containers/arch",
            "/bin/sh",
            NULL
        };
        char **argv = (ifd >= 0) ? argv_interactive : argv_test;
        if (ifd >= 0) {
            syscall1(3, ifd);
            print("[init-arch] Interactive mode requested: launching Arch Linux shell...\n");
        } else {
            print("[init-arch] Spawning Arch Linux container via /bin/ctr_run...\n");
        }
        char *envp[] = {
            "PATH=/bin:/usr/bin:/sbin:/usr/sbin",
            "HOME=/root",
            "TERM=xterm",
            "PS1=\\u@archlinux:\\w# ",
            NULL
        };

        syscall3(59, (long)"/bin/ctr_run", (long)argv, (long)envp);

        print("[init-arch] Error: execve(/bin/ctr_run) failed!\n");
        syscall1(60, 127);
    }

    // Parent (PID 1): wait for container runner to exit
    int wstatus = 0;
    long reaped = syscall4(61, pid, (long)&wstatus, 0, 0); // wait4
    print("[init-arch] Container runner exited with status ");
    print_num(wstatus);
    print("\n");

    if (wstatus == 0) {
        print("\n======================================================================\n");
        print("  [ARCH_CONTAINER_SUCCESS] ALL CHECKS PASSED!\n");
        print("  \"I run arch btw (in a namespace with my own kernel\n");
        print("      in Qemu on Ubuntu on WSL2 on Windows 11)\"\n");
        print("======================================================================\n\n");
    } else {
        print("\n======================================================================\n");
        print("  [ARCH_CONTAINER_FAILED] Container exited with non-zero status\n");
        print("======================================================================\n\n");
    }

    // Flush output queues
    for (volatile int i = 0; i < 50; i++) {
        syscall0(24); // sched_yield
    }

    // Power off QEMU cleanly via reboot
    print("[init-arch] Powering off system...\n");
    for (volatile int i = 0; i < 10; i++) {
        syscall0(24);
    }
    syscall4(169, 0xfee1dead, 672274793, 0x4321fedc, 0);

    // Fallback infinite loop
    while (1) {
        syscall0(24);
    }
}
