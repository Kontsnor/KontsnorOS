.intel_syntax noprefix
.global _start

.section .text

_start:
    # Main shell loop
main_loop:
    # 1. Print prompt "kontsnorsh# "
    mov rax, 1          # sys_write
    mov rdi, 1          # stdout
    lea rsi, [rip + prompt]
    mov rdx, 12         # length of "kontsnorsh# "
    syscall

    # 2. Read line from stdin into buffer
    mov rax, 0          # sys_read
    mov rdi, 0          # stdin
    lea rsi, [rip + input_buf]
    mov rdx, 255        # read up to 255 bytes
    syscall

    # Check if read returned <= 0
    cmp rax, 0
    jle main_loop       # If error or EOF, loop back

    # Put a null terminator at the end of the input (replace newline with null)
    # rax contains the number of bytes read.
    # The last character is usually '\n' at index rax - 1.
    dec rax             # index of '\n'
    lea rbx, [rip + input_buf]
    cmp byte ptr [rbx + rax], 10  # is it '\n'?
    je replace_nl
    inc rax             # if not '\n', restore index to rax (after the last char)
replace_nl:
    mov byte ptr [rbx + rax], 0   # Null terminate

    # Check if input is empty (first char is 0)
    cmp byte ptr [rbx], 0
    je main_loop

    # 3. Compare command with "exit"
    lea rdi, [rip + input_buf]
    lea rsi, [rip + cmd_exit]
    call strcmp
    test rax, rax
    jz do_exit

    # 4. Compare command with "uname"
    lea rdi, [rip + input_buf]
    lea rsi, [rip + cmd_uname]
    call strcmp
    test rax, rax
    jz do_uname

    # 5. Check if command starts with "echo "
    lea rdi, [rip + input_buf]
    lea rsi, [rip + cmd_echo]
    mov rcx, 5
    call strncmp
    test rax, rax
    jz do_echo

    # 5.5 Compare command with "pwd"
    lea rdi, [rip + input_buf]
    lea rsi, [rip + cmd_pwd]
    call strcmp
    test rax, rax
    jz do_pwd

    # 5.6 Check if command starts with "cd " or is exactly "cd"
    lea rdi, [rip + input_buf]
    lea rsi, [rip + cmd_cd_exact]
    call strcmp
    test rax, rax
    jz do_cd_home

    lea rdi, [rip + input_buf]
    lea rsi, [rip + cmd_cd]
    mov rcx, 3
    call strncmp
    test rax, rax
    jz do_cd

    # 5.7 Compare command with "alloc"
    lea rdi, [rip + input_buf]
    lea rsi, [rip + cmd_alloc]
    call strcmp
    test rax, rax
    jz do_alloc

    # 5.8 Compare command with "ls"
    lea rdi, [rip + input_buf]
    lea rsi, [rip + cmd_ls]
    call strcmp
    test rax, rax
    jz do_ls

    # 5.9 Compare command with "sig"
    lea rdi, [rip + input_buf]
    lea rsi, [rip + cmd_sig]
    call strcmp
    test rax, rax
    jz do_sig

    # 6. For anything else, do fork + execve
    mov rax, 57         # sys_fork
    syscall
    
    cmp rax, 0
    jl fork_failed
    je child_process    # child returns 0

    # Parent process
    # wait4(child_pid, &wstatus, 0, NULL)
    mov rdi, rax        # child pid
    lea rsi, [rip + wstatus] # wstatus pointer
    mov rdx, 0          # options
    mov r10, 0          # rusage
    mov rax, 61         # sys_wait4
    syscall
    jmp main_loop

child_process:
    push 0
    lea rdi, [rip + input_buf]
    push rdi
    mov rsi, rsp
    mov rdx, 0
    mov rax, 59         # sys_execve
    syscall

    # If execve returns, it failed.
    mov rax, 1          # sys_write
    mov rdi, 2          # stderr
    lea rsi, [rip + err_not_found_1]
    mov rdx, 31
    syscall

    lea rsi, [rip + input_buf]
    call strlen
    mov rdx, rax
    mov rax, 1          # sys_write
    mov rdi, 2          # stderr
    syscall

    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + newline]
    mov rdx, 1
    syscall

    mov rdi, 127
    mov rax, 60         # sys_exit
    syscall

    jmp main_loop

fork_failed:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + err_fork_failed]
    mov rdx, 12
    syscall
    jmp main_loop

do_exit:
    mov rdi, 0          # status = 0
    mov rax, 60         # sys_exit
    syscall

do_uname:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + uname_str]
    mov rdx, 18
    syscall
    jmp main_loop

do_echo:
    lea rsi, [rip + input_buf + 5]
    call strlen
    mov rdx, rax
    mov rax, 1          # sys_write
    mov rdi, 1          # stdout
    syscall

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + newline]
    mov rdx, 1
    syscall
    jmp main_loop

do_pwd:
    sub rsp, 256
    mov rdi, rsp
    mov rsi, 256
    mov rax, 79         # sys_getcwd
    syscall
    test rax, rax
    jz pwd_error

    mov rsi, rsp
    call strlen
    mov rdx, rax        # length of path
    mov rsi, rsp        # path buffer
    mov rdi, 1          # stdout
    mov rax, 1          # sys_write
    syscall

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + newline]
    mov rdx, 1
    syscall
    jmp pwd_done

pwd_error:
    mov rax, 1
    mov rdi, 2          # stderr
    lea rsi, [rip + err_pwd]
    mov rdx, 21
    syscall

pwd_done:
    add rsp, 256
    jmp main_loop

do_cd_home:
    lea rdi, [rip + slash]
    mov rax, 80         # sys_chdir
    syscall
    test rax, rax
    js cd_error
    jmp main_loop

do_cd:
    lea rdi, [rip + input_buf + 3]
    mov rax, 80         # sys_chdir
    syscall
    test rax, rax
    js cd_error
    jmp main_loop

cd_error:
    mov rax, 1
    mov rdi, 2          # stderr
    lea rsi, [rip + err_cd]
    mov rdx, 10
    syscall
    jmp main_loop

do_alloc:
    mov rax, 1
    mov rdi, 1          # stdout
    lea rsi, [rip + msg_alloc_start]
    mov rdx, 38
    syscall

    mov rdi, 0          # addr = 0
    mov rsi, 65536      # length = 64KB
    mov rdx, 3          # prot = 3 (PROT_READ | PROT_WRITE)
    mov r10, 0x22       # flags = 0x22 (MAP_PRIVATE | MAP_ANONYMOUS)
    mov r8, -1          # fd = -1
    mov r9, 0           # offset = 0
    mov rax, 9          # sys_mmap
    syscall
    test rax, rax
    js alloc_failed

    mov r12, rax        # Save mapped address in r12

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + msg_alloc_success]
    mov rdx, 36
    syscall

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + msg_alloc_write]
    mov rdx, 35
    syscall

    lea rsi, [rip + test_signature]
    mov rdi, r12
    mov rcx, 50         # length of signature (including null terminator)
    rep movsb

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + msg_alloc_read]
    mov rdx, 25
    syscall

    mov rax, 1
    mov rdi, 1
    mov rsi, r12
    mov rdx, 50
    syscall

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + newline]
    mov rdx, 1
    syscall

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + msg_alloc_free]
    mov rdx, 39
    syscall

    mov rdi, r12
    mov rsi, 65536
    mov rax, 11         # sys_munmap
    syscall

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + msg_alloc_done]
    mov rdx, 41
    syscall
    jmp main_loop

alloc_failed:
    mov rax, 1
    mov rdi, 2          # stderr
    lea rsi, [rip + err_alloc]
    mov rdx, 19
    syscall
    jmp main_loop

# `ls` command implementation
do_ls:
    # Open current directory
    lea rdi, [rip + dot]
    mov rsi, 0          # O_RDONLY
    mov rdx, 0
    mov rax, 2          # sys_open
    syscall
    test rax, rax
    js ls_error

    mov r12, rax        # Save fd in r12

    # Allocate 1024 bytes buffer on stack
    sub rsp, 1024

    # Call getdents64(fd, buf, 1024)
    mov rdi, r12
    mov rsi, rsp
    mov rdx, 1024
    mov rax, 217        # sys_getdents64
    syscall
    test rax, rax
    js ls_close_error

    mov r13, rax        # Save bytes read in r13
    mov r14, 0          # current offset in buffer

ls_loop:
    cmp r14, r13
    jge ls_done

    lea rbx, [rsp + r14]
    movzx r15, word ptr [rbx + 16] # d_reclen is at offset 16
    lea rsi, [rbx + 19]            # d_name starts at offset 19

    # Print entry name
    call strlen
    mov rdx, rax
    mov rdi, 1          # stdout
    mov rax, 1          # sys_write
    syscall

    # Print space
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + space]
    mov rdx, 1
    syscall

    add r14, r15
    jmp ls_loop

ls_done:
    # Print newline
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + newline]
    mov rdx, 1
    syscall

    add rsp, 1024
    mov rdi, r12
    mov rax, 3          # sys_close
    syscall
    jmp main_loop

ls_close_error:
    add rsp, 1024
    mov rdi, r12
    mov rax, 3          # sys_close
    syscall

ls_error:
    mov rax, 1
    mov rdi, 2          # stderr
    lea rsi, [rip + err_ls]
    mov rdx, 10
    syscall
    jmp main_loop

# `sig` command implementation
do_sig:
    # Setup SigAction struct on stack (32 bytes)
    sub rsp, 32
    lea rax, [rip + sig_handler]
    mov [rsp + 0], rax             # sa_handler
    mov qword ptr [rsp + 8], 0x04000000 # sa_flags = SA_RESTORER
    lea rax, [rip + sig_restorer]
    mov [rsp + 16], rax            # sa_restorer
    mov qword ptr [rsp + 24], 0    # sa_mask

    # Call rt_sigaction(SIGINT=2, act, NULL, 8)
    mov rdi, 2          # signum
    mov rsi, rsp        # act
    mov rdx, 0          # oldact
    mov r10, 8          # sigsetsize
    mov rax, 13         # sys_rt_sigaction
    syscall
    test rax, rax
    js sig_error

    add rsp, 32

    # Print registration msg
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + msg_sig_sent]
    mov rdx, 61
    syscall

    # Getpid to send to self
    mov rax, 39         # sys_getpid
    syscall
    mov rdi, rax        # self pid
    mov rsi, 2          # SIGINT = 2
    mov rax, 62         # sys_kill
    syscall

    # Print success message after returning from handler
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + msg_sig_done]
    mov rdx, 52
    syscall
    jmp main_loop

sig_error:
    add rsp, 32
    mov rax, 1
    mov rdi, 2          # stderr
    lea rsi, [rip + err_sig]
    mov rdx, 21
    syscall
    jmp main_loop

# Signal handler function
sig_handler:
    push rdi
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + msg_sig_caught]
    mov rdx, 38
    syscall
    pop rdi

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + msg_sig_sigint]
    mov rdx, 9
    syscall
    ret

# Signal restorer trampoline
sig_restorer:
    mov rax, 15         # sys_rt_sigreturn
    syscall
    ret

# Helper: strcmp(rdi, rsi) -> rax (0 if equal)
strcmp:
    xor rax, rax
strcmp_loop:
    mov al, byte ptr [rdi]
    mov bl, byte ptr [rsi]
    cmp al, bl
    jne strcmp_diff
    test al, al
    jz strcmp_equal
    inc rdi
    inc rsi
    jmp strcmp_loop
strcmp_diff:
    mov rax, 1
    ret
strcmp_equal:
    xor rax, rax
    ret

# Helper: strncmp(rdi, rsi, rcx) -> rax (0 if equal)
strncmp:
    xor rax, rax
strncmp_loop:
    test rcx, rcx
    jz strncmp_equal
    mov al, byte ptr [rdi]
    mov bl, byte ptr [rsi]
    cmp al, bl
    jne strncmp_diff
    test al, al
    jz strncmp_equal
    inc rdi
    inc rsi
    dec rcx
    jmp strncmp_loop
strncmp_diff:
    mov rax, 1
    ret
strncmp_equal:
    xor rax, rax
    ret

# Helper: strlen(rsi) -> rax (length of null-terminated string)
strlen:
    xor rax, rax
strlen_loop:
    cmp byte ptr [rsi + rax], 0
    je strlen_done
    inc rax
    jmp strlen_loop
strlen_done:
    ret

.section .data
prompt:
    .ascii "kontsnorsh# "
cmd_exit:
    .asciz "exit"
cmd_uname:
    .asciz "uname"
cmd_echo:
    .ascii "echo "
cmd_pwd:
    .asciz "pwd"
cmd_cd:
    .ascii "cd "
cmd_cd_exact:
    .asciz "cd"
slash:
    .asciz "/"
dot:
    .asciz "."
space:
    .ascii " "
uname_str:
    .ascii "KontsnorOS v0.1.0\n"
newline:
    .ascii "\n"
err_not_found_1:
    .ascii "kontsnorsh: command not found: "
err_fork_failed:
    .ascii "fork failed\n"
err_pwd:
    .ascii "getcwd syscall failed"
err_cd:
    .ascii "cd failed\n"
cmd_alloc:
    .asciz "alloc"
msg_alloc_start:
    .ascii "[shell] Allocating 64KB via sys_mmap...\n"
msg_alloc_success:
    .ascii "[shell] Memory mapped successfully!\n"
msg_alloc_write:
    .ascii "[shell] Writing test signature...\n"
msg_alloc_read:
    .ascii "[shell] Read signature: "
msg_alloc_free:
    .ascii "[shell] Freeing memory via sys_munmap...\n"
msg_alloc_done:
    .ascii "[shell] Memory test completed successfully!\n"
test_signature:
    .ascii "KontsnorOS Dynamic Memory Verification Successful!"
    .byte 0
err_alloc:
    .ascii "sys_mmap failed\n"

cmd_ls:
    .asciz "ls"
err_ls:
    .ascii "ls failed\n"

cmd_sig:
    .asciz "sig"
err_sig:
    .ascii "sys_sigaction failed\n"
msg_sig_caught:
    .ascii "[shell] Signal handler caught signal: "
msg_sig_sigint:
    .ascii "SIGINT!\n"
msg_sig_sent:
    .ascii "[shell] Signal handler registered. Sending SIGINT to self...\n"
msg_sig_done:
    .ascii "[shell] Successfully returned from signal handler!\n"

.section .bss
.align 16
input_buf:
    .zero 256
wstatus:
    .zero 4
