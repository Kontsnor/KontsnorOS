//! Process management system calls module.

pub mod creds;
pub mod futex;
pub mod info;
pub mod lifecycle;

pub use creds::{
    calculate_exec_creds, sys_getegid, sys_geteuid, sys_getgid, sys_getpgid, sys_getpid,
    sys_getppid, sys_gettid, sys_getuid, sys_setgid, sys_setpgid, sys_setsid, sys_setuid,
};
pub use futex::sys_futex;
pub use info::{
    sys_clock_gettime, sys_getrandom, sys_getrlimit, sys_gettimeofday, sys_nanosleep,
    sys_prlimit64, sys_sched_getaffinity, sys_setrlimit, sys_sigaltstack, sys_sysinfo, sys_tgkill,
    sys_times, sys_uname,
};
pub use lifecycle::{
    sys_arch_prctl, sys_brk, sys_clone, sys_execve, sys_exit, sys_exit_group, sys_fork, sys_prctl,
    sys_set_tid_address, sys_wait4,
};
