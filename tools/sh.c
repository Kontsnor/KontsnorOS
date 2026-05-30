// tools/sh.c
// Statically linked freestanding interactive Unix C shell for KontsnorOS.

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

long syscall6(long num, long arg1, long arg2, long arg3, long arg4, long arg5, long arg6) {
    long ret;
    register long r10 __asm("r10") = arg4;
    register long r8  __asm("r8")  = arg5;
    register long r9  __asm("r9")  = arg6;
    __asm__ __volatile__(
        "syscall"
        : "=a"(ret)
        : "a"(num), "D"(arg1), "S"(arg2), "d"(arg3), "r"(r10), "r"(r8), "r"(r9)
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

int strcmp(const char *s1, const char *s2) {
    while (*s1 && *s1 == *s2) {
        s1++;
        s2++;
    }
    return (unsigned char)*s1 - (unsigned char)*s2;
}

int strncmp(const char *s1, const char *s2, int n) {
    while (n > 0 && *s1 && *s1 == *s2) {
        s1++;
        s2++;
        n--;
    }
    if (n == 0) return 0;
    return (unsigned char)*s1 - (unsigned char)*s2;
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

// structures
struct sigaction_t {
    void (*sa_handler)(int);
    unsigned long sa_flags;
    void (*sa_restorer)(void);
    unsigned long sa_mask;
};

struct linux_dirent64 {
    unsigned long d_ino;
    long d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

// Signal handling
void sig_handler(int sig) {
    print("\n[Signal] Received SIGINT (Ctrl+C)!\n");
}

void __attribute__((naked)) sig_restorer() {
    __asm__ __volatile__(
        "mov $15, %rax\n" // sys_rt_sigreturn
        "syscall\n"
    );
}

// Argument Parser
void parse_args(char *line, char *argv[]) {
    int argc = 0;
    char *p = line;
    while (*p) {
        while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') {
            *p = '\0';
            p++;
        }
        if (*p == '\0') break;
        argv[argc++] = p;
        if (argc >= 15) break;
        while (*p && *p != ' ' && *p != '\t' && *p != '\n' && *p != '\r') {
            p++;
        }
    }
    argv[argc] = 0;
}

// Commands Implementation
void do_alloc() {
    print("Allocating 64KB memory via sys_mmap...\n");
    // PROT_READ | PROT_WRITE = 3
    // MAP_PRIVATE | MAP_ANONYMOUS = 0x22
    long addr = syscall6(9, 0, 65536, 3, 0x22, -1, 0);
    if (addr < 0) {
        print_err("Error: sys_mmap failed\n");
        return;
    }
    print("Successfully mapped 64KB at: ");
    print_hex((unsigned long)addr);
    print("\n");
    
    print("Writing verification signature to memory...\n");
    char *ptr = (char *)addr;
    char sig[] = "KontsnorOS Dynamic Memory Verification Success!";
    int len = strlen(sig);
    for (int i = 0; i <= len; i++) {
        ptr[i] = sig[i];
    }
    
    print("Reading signature back from memory: ");
    print(ptr);
    print("\n");
    
    print("Freeing memory via sys_munmap...\n");
    syscall2(11, addr, 65536);
    print("Memory freed successfully.\n");
}

void do_ls() {
    long fd = syscall3(2, (long)".", 0, 0); // open(".", O_RDONLY)
    if (fd < 0) {
        print_err("Error: could not open directory\n");
        return;
    }
    
    char buf[1024];
    long nread = syscall3(217, fd, (long)buf, 1024); // getdents64
    if (nread < 0) {
        print_err("Error: getdents64 failed\n");
        syscall1(3, fd);
        return;
    }
    
    long pos = 0;
    while (pos < nread) {
        struct linux_dirent64 *d = (struct linux_dirent64 *)(buf + pos);
        if (d->d_ino != 0) {
            print(d->d_name);
            print("  ");
        }
        pos += d->d_reclen;
    }
    print("\n");
    syscall1(3, fd);
}

void do_sig() {
    struct sigaction_t act;
    act.sa_handler = sig_handler;
    act.sa_flags = 0x04000000; // SA_RESTORER
    act.sa_restorer = sig_restorer;
    act.sa_mask = 0;
    
    long ret = syscall4(13, 2, (long)&act, 0, 8); // rt_sigaction(SIGINT=2, &act, NULL, 8)
    if (ret < 0) {
        print_err("Error: rt_sigaction failed\n");
        return;
    }
    print("Registered SIGINT handler. Sending SIGINT (2) to self...\n");
    long pid = syscall0(39); // getpid
    syscall2(62, pid, 2); // kill(self, SIGINT)
    print("Signal sent and processed.\n");
}

void execute_single_cmd(char *argv[]) {
    // Scan for redirection operators
    int out_redir_idx = -1;
    int in_redir_idx = -1;
    for (int i = 0; argv[i] != 0; i++) {
        if (strcmp(argv[i], ">") == 0) {
            out_redir_idx = i;
        } else if (strcmp(argv[i], "<") == 0) {
            in_redir_idx = i;
        }
    }

    // Handle input redirection "<"
    if (in_redir_idx != -1) {
        if (argv[in_redir_idx + 1] == 0) {
            print_err("sh: syntax error near unexpected token 'newline'\n");
            syscall1(60, 1); // exit
        }
        // open file for read: O_RDONLY = 0
        long fd = syscall3(2, (long)argv[in_redir_idx + 1], 0, 0);
        if (fd < 0) {
            print_err("sh: cannot open input file\n");
            syscall1(60, 1);
        }
        syscall2(33, fd, 0); // dup2(fd, 0)
        syscall1(3, fd); // close
        
        // Strip "< filename" from argv
        argv[in_redir_idx] = 0;
    }

    // Handle output redirection ">"
    if (out_redir_idx != -1) {
        if (argv[out_redir_idx + 1] == 0) {
            print_err("sh: syntax error near unexpected token 'newline'\n");
            syscall1(60, 1);
        }
        // open file for write: O_CREAT | O_WRONLY | O_TRUNC -> 64 | 1 | 512 = 577 in decimal
        long fd = syscall3(2, (long)argv[out_redir_idx + 1], 577, 0666);
        if (fd < 0) {
            print_err("sh: redirection failed\n");
            syscall1(60, 1);
        }
        syscall2(33, fd, 1); // dup2(fd, 1)
        syscall1(3, fd); // close
        
        // Strip "> filename" from argv
        argv[out_redir_idx] = 0;
    }

    if (argv[0] == 0) {
        syscall1(60, 0);
    }

    // Execute built-ins or execve
    if (strcmp(argv[0], "uname") == 0) {
        print("KontsnorOS 1.0.0-release x86_64\n");
        syscall1(60, 0);
    } else if (strcmp(argv[0], "echo") == 0) {
        for (int i = 1; argv[i] != 0; i++) {
            print(argv[i]);
            if (argv[i+1] != 0) print(" ");
        }
        print("\n");
        syscall1(60, 0);
    } else if (strcmp(argv[0], "pwd") == 0) {
        char path_buf[256];
        long ret = syscall2(79, (long)path_buf, 256); // getcwd
        if (ret >= 0) {
            print(path_buf);
            print("\n");
            syscall1(60, 0);
        } else {
            print_err("Error: getcwd failed\n");
            syscall1(60, 1);
        }
    } else if (strcmp(argv[0], "ls") == 0) {
        do_ls();
        syscall1(60, 0);
    } else if (strcmp(argv[0], "alloc") == 0) {
        do_alloc();
        syscall1(60, 0);
    } else if (strcmp(argv[0], "sig") == 0) {
        do_sig();
        syscall1(60, 0);
    } else if (strcmp(argv[0], "mkdir") == 0) {
        if (!argv[1]) {
            print_err("mkdir: missing operand\n");
            syscall1(60, 1);
        } else {
            long ret = syscall2(83, (long)argv[1], 0777); // sys_mkdir
            syscall1(60, ret < 0 ? 1 : 0);
        }
    } else if (strcmp(argv[0], "rmdir") == 0) {
        if (!argv[1]) {
            print_err("rmdir: missing operand\n");
            syscall1(60, 1);
        } else {
            long ret = syscall1(84, (long)argv[1]); // sys_rmdir
            syscall1(60, ret < 0 ? 1 : 0);
        }
    } else if (strcmp(argv[0], "rm") == 0) {
        if (!argv[1]) {
            print_err("rm: missing operand\n");
            syscall1(60, 1);
        } else {
            long ret = syscall1(87, (long)argv[1]); // sys_unlink
            syscall1(60, ret < 0 ? 1 : 0);
        }
    } else if (strcmp(argv[0], "touch") == 0) {
        if (!argv[1]) {
            print_err("touch: missing operand\n");
            syscall1(60, 1);
        } else {
            long fd = syscall3(2, (long)argv[1], 65, 0666);
            if (fd < 0) {
                print_err("touch: failed to create file\n");
                syscall1(60, 1);
            } else {
                syscall1(3, fd);
                syscall1(60, 0);
            }
        }
    } else if (strcmp(argv[0], "cat") == 0) {
        long fd = 0; // Default to stdin
        if (argv[1]) {
            fd = syscall3(2, (long)argv[1], 0, 0); // open(file, O_RDONLY)
            if (fd < 0) {
                print_err("cat: cannot open file\n");
                syscall1(60, 1);
            }
        }
        char read_buf[256];
        long n;
        while ((n = syscall3(0, fd, (long)read_buf, 255)) > 0) {
            read_buf[n] = '\0';
            print(read_buf);
        }
        if (n < 0) {
            print_err("cat: read error: ");
            print_hex(-n);
            print_err("\n");
        }
        if (fd > 0) {
            syscall1(3, fd); // close
        }
        syscall1(60, 0);
    } else {
        // Not a built-in, execute binary
        volatile char **v_argv = (volatile char **)argv;
        char *const *child_argv = (char *const *)v_argv;
        syscall3(59, (long)child_argv[0], (long)child_argv, 0); // execve(cmd, argv, NULL)
        
        // If we get here, execve failed
        print_err("sh: command not found: ");
        print_err(child_argv[0]);
        print_err("\n");
        syscall1(60, 127); // exit(127)
    }
}

void exec_cmd(char *argv[]) {
    // 1. Scan for pipeline first
    int volatile pipe_idx = -1;
    for (int i = 0; argv[i] != 0; i++) {
        if (strcmp(argv[i], "|") == 0) {
            pipe_idx = i;
            break;
        }
    }

    if (pipe_idx != -1) {
        argv[pipe_idx] = 0;
        char **left_argv = argv;
        char **right_argv = &argv[pipe_idx + 1];

        // Create pipe using standard 32-bit int array
        int pipefds[2];
        long ret = syscall1(22, (long)pipefds); // pipe(pipefds)
        if (ret < 0) {
            print_err("sh: pipe creation failed\n");
            return;
        }

        long p0 = pipefds[0];
        long p1 = pipefds[1];

        long pid1 = sys_fork();
        if (pid1 < 0) {
            print_err("sh: fork failed\n");
            return;
        } else if (pid1 == 0) {
            // Child 1 (Left Child): stdout redirects to pipe write end
            syscall2(33, p1, 1); // dup2(p1, 1)
            syscall1(3, p0);     // close(p0)
            syscall1(3, p1);     // close(p1)
            execute_single_cmd(left_argv);
            syscall1(60, 127);   // exit(127) if execute_single_cmd returns
        }

        long pid2 = sys_fork();
        if (pid2 < 0) {
            print_err("sh: fork failed\n");
            return;
        } else if (pid2 == 0) {
            // Child 2 (Right Child): stdin redirects to pipe read end
            syscall2(33, p0, 0); // dup2(p0, 0)
            syscall1(3, p0);     // close(p0)
            syscall1(3, p1);     // close(p1)
            execute_single_cmd(right_argv);
            syscall1(60, 127);   // exit(127) if execute_single_cmd returns
        }

        // Parent closes pipe ends
        syscall1(3, p0);
        syscall1(3, p1);

        // Wait for both children
        long wstatus = 0;
        syscall4(61, pid1, (long)&wstatus, 0, 0); // wait4
        syscall4(61, pid2, (long)&wstatus, 0, 0); // wait4
        return;
    }

    // 2. Otherwise, run single command in a child fork
    volatile char **v_argv = (volatile char **)argv;
    long pid = sys_fork(); // fork
    if (pid < 0) {
        print_err("Error: fork failed\n");
    } else if (pid == 0) {
        char **child_argv = (char **)v_argv;
        execute_single_cmd(child_argv);
    } else {
        long wstatus = 0;
        syscall4(61, pid, (long)&wstatus, 0, 0); // wait4
    }
}

// Shell Entry Point
void _start() {
    char input_buf[256];
    char *argv[16];
    
    print("\nWelcome to KontsnorOS premium C-based Shell!\n\n");
    
    while (1) {
        // Print cyan colored premium prompt
        print("\x1b[36mkontsnorsh-c#\x1b[0m ");
        
        // Clean buffer
        for (int i = 0; i < 256; i++) input_buf[i] = '\0';
        
        long bytes_read = syscall3(0, 0, (long)input_buf, 255); // read(0, buf, 255)
        if (bytes_read <= 0) {
            continue;
        }
        
        // Remove trailing newline
        if (bytes_read > 0 && input_buf[bytes_read - 1] == '\n') {
            input_buf[bytes_read - 1] = '\0';
        }
        
        if (strlen(input_buf) == 0) {
            continue;
        }
        
        // Parse arguments
        parse_args(input_buf, argv);
        if (argv[0] == 0) {
            continue;
        }
        
        // Execute commands
        if (strcmp(argv[0], "exit") == 0) {
            syscall1(60, 0); // exit(0)
        } else if (strcmp(argv[0], "cd") == 0) {
            const char *target = argv[1] ? argv[1] : "/";
            long ret = syscall1(80, (long)target); // chdir
            if (ret < 0) {
                print_err("cd: no such file or directory: ");
                print_err(target);
                print_err("\n");
            }
        } else {
            exec_cmd(argv);
        }
    }
}
