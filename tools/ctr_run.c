// tools/ctr_run.c — Minimal Container Runtime for KontsnorOS
//
// Demonstrates process isolation via Linux namespaces (CLONE_NEWNS, CLONE_NEWUTS, CLONE_NEWPID)
// and filesystem jailing (chroot / pivot_root).

#define NULL ((void*)0)

#define CLONE_NEWNS   0x00020000
#define CLONE_NEWUTS  0x04000000
#define CLONE_NEWPID  0x20000000

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

static inline long syscall5(long num, long a1, long a2, long a3, long a4, long a5) {
    long ret;
    register long r10 __asm("r10") = a4;
    register long r8  __asm("r8")  = a5;
    __asm__ __volatile__("syscall" : "=a"(ret) : "a"(num), "D"(a1), "S"(a2), "d"(a3), "r"(r10), "r"(r8) : "rcx", "r11", "memory");
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

static inline void print_err(const char *s) {
    syscall3(1, 2, (long)s, strlen(s)); // write(2, s, len)
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

int main(int argc, char **argv);

__attribute__((naked)) void _start(void) {
    __asm__ __volatile__(
        "xor %rbp, %rbp\n"
        "mov (%rsp), %rdi\n"       // argc
        "lea 8(%rsp), %rsi\n"      // argv
        "and $-16, %rsp\n"         // 16-byte align stack
        "call main\n"
        "mov %rax, %rdi\n"         // exit status
        "mov $60, %rax\n"          // sys_exit
        "syscall\n"
    );
}

int main(int argc, char **argv) {
    const char *rootfs = "/containers/alpine";
    const char *cmd = "/bin/sh";
    char **cmd_argv = NULL;

    if (argc >= 2) {
        rootfs = argv[1];
    }
    if (argc >= 3) {
        cmd = argv[2];
        cmd_argv = &argv[2];
    }

    print("\n[ctr_run] ========================================\n");
    print("[ctr_run]  KontsnorOS Lightweight Container Launcher\n");
    print("[ctr_run] ========================================\n");
    print("[ctr_run] Target Rootfs: "); print(rootfs); print("\n");
    print("[ctr_run] Entry Command: "); print(cmd); print("\n");

    // Step 1: Disassociate namespaces (Mount, UTS, and PID)
    long flags = CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWPID;
    long ret = syscall1(272, flags); // unshare
    if (ret != 0) {
        print_err("[ctr_run] Error: unshare(CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWPID) failed: ");
        print_num(ret);
        print_err("\n");
        syscall1(60, 1);
    }
    print("[ctr_run] unshare() succeeded: Mount, UTS, and PID namespaces disassociated.\n");

    // Step 2: Fork into new PID namespace
    print("[ctr_run] Spawning container init process via fork()...\n");
    long child_pid = sys_fork();
    if (child_pid < 0) {
        print_err("[ctr_run] Error: fork() failed: ");
        print_num(child_pid);
        print_err("\n");
        syscall1(60, 1);
    }

    if (child_pid == 0) {
        // ── Container Child Process ──────────────────────────────────────────
        print("[container-init] Container init spawned (PID namespace active).\n");

        // 1. UTS isolation: Set container hostname
        const char *hostname = "kontsnor-container";
        for (int i = 0; rootfs[i]; i++) {
            if (rootfs[i] == 'a' && rootfs[i+1] == 'r' && rootfs[i+2] == 'c' && rootfs[i+3] == 'h') {
                hostname = "archlinux";
                break;
            }
        }
        syscall2(170, (long)hostname, strlen(hostname)); // sethostname
        print("[container-init] Hostname set to: "); print(hostname); print("\n");

        // 2. Mount private container pseudo-filesystems inside rootfs
        // Mount procfs on /proc inside container root
        char proc_mount[256];
        int rlen = strlen(rootfs);
        for (int i = 0; i < rlen && i < 200; i++) proc_mount[i] = rootfs[i];
        proc_mount[rlen] = '/';
        proc_mount[rlen + 1] = 'p';
        proc_mount[rlen + 2] = 'r';
        proc_mount[rlen + 3] = 'o';
        proc_mount[rlen + 4] = 'c';
        proc_mount[rlen + 5] = '\0';
        syscall5(165, (long)"proc", (long)proc_mount, (long)"procfs", 0, 0);

        // Mount devtmpfs on /dev
        char dev_mount[256];
        for (int i = 0; i < rlen && i < 200; i++) dev_mount[i] = rootfs[i];
        dev_mount[rlen] = '/';
        dev_mount[rlen + 1] = 'd';
        dev_mount[rlen + 2] = 'e';
        dev_mount[rlen + 3] = 'v';
        dev_mount[rlen + 4] = '\0';
        syscall5(165, (long)"devtmpfs", (long)dev_mount, (long)"devtmpfs", 0, 0);

        // Mount tmpfs on /tmp
        char tmp_mount[256];
        for (int i = 0; i < rlen && i < 200; i++) tmp_mount[i] = rootfs[i];
        tmp_mount[rlen] = '/';
        tmp_mount[rlen + 1] = 't';
        tmp_mount[rlen + 2] = 'm';
        tmp_mount[rlen + 3] = 'p';
        tmp_mount[rlen + 4] = '\0';
        syscall5(165, (long)"tmpfs", (long)tmp_mount, (long)"tmpfs", 0, 0);

        // 3. Filesystem isolation: chdir and chroot to container rootfs
        long cd_ret = syscall1(80, (long)rootfs); // chdir
        if (cd_ret < 0) {
            print_err("[container-init] Error: chdir to rootfs failed: ");
            print_num(cd_ret);
            print_err("\n");
            syscall1(60, 1);
        }

        long cr_ret = syscall1(161, (long)"."); // chroot
        if (cr_ret < 0) {
            print_err("[container-init] Error: chroot to rootfs failed: ");
            print_num(cr_ret);
            print_err("\n");
            syscall1(60, 1);
        }

        syscall1(80, (long)"/"); // chdir to jailed root
        print("[container-init] Filesystem successfully jailed at container root.\n");

        // Ensure /etc/mtab -> /proc/mounts symlink exists so pacman can
        // determine filesystem mount points inside the container.
        syscall1(87, (long)"/etc/mtab");                         // unlink (ignore error if not present)
        syscall2(88, (long)"/proc/mounts", (long)"/etc/mtab");   // symlink("/proc/mounts", "/etc/mtab")
        print("[container-init] /etc/mtab -> /proc/mounts symlink created.\n");

        // 4. Execute container workload
        print("[container-init] Launching entrypoint binary...\n");
        char *default_argv[] = { (char*)cmd, NULL };
        char **exec_argv = cmd_argv ? cmd_argv : default_argv;

        char *envp[] = {
            "PATH=/bin:/usr/bin:/sbin:/usr/sbin",
            "HOME=/root",
            "USER=root",
            "TERM=xterm",
            "CONTAINER=kontsnor",
            NULL
        };

        // Try execve
        syscall3(59, (long)cmd, (long)exec_argv, (long)envp);

        // Fallbacks
        syscall3(59, (long)"/bin/sh", (long)exec_argv, (long)envp);
        syscall3(59, (long)"/bin/busybox", (long)exec_argv, (long)envp);

        print_err("[container-init] Error: execve() failed to execute command!\n");
        syscall1(60, 127);
    }

    // ── Host Parent Process ──────────────────────────────────────────────────
    print("[ctr_run] Container running with host PID ");
    print_num(child_pid);
    print(". Waiting for completion...\n");

    int wstatus = 0;
    long reaped = syscall4(61, child_pid, (long)&wstatus, 0, 0); // wait4

    print("[ctr_run] Container process reaped (PID ");
    print_num(reaped);
    print(", status=");
    print_num(wstatus);
    print(").\n");

    print("[ctr_run] ========================================\n");
    print("[ctr_run]  Container Lifecycle Finished\n");
    print("[ctr_run] ========================================\n");

    return wstatus;
}
