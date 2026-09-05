// Copyright (C) 2026 KontsnorOS Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Process management system calls module.

pub mod creds;
pub mod futex;
pub mod info;
pub mod lifecycle;

pub use creds::{
    calculate_exec_creds, sys_capget, sys_capset, sys_getegid, sys_geteuid, sys_getgid,
    sys_getgroups, sys_getpgid, sys_getpgrp, sys_getpid, sys_getppid, sys_getresgid, sys_getresuid,
    sys_getsid, sys_gettid, sys_getuid, sys_setfsgid, sys_setfsuid, sys_setgid, sys_setgroups,
    sys_setpgid, sys_setregid, sys_setresgid, sys_setresuid, sys_setreuid, sys_setsid, sys_setuid,
};
pub use futex::{sys_futex, sys_futex_waitv};
pub use info::{
    sys_alarm, sys_clock_getres, sys_clock_gettime, sys_clock_nanosleep, sys_clock_settime,
    sys_get_robust_list, sys_getcpu, sys_getitimer, sys_getpriority, sys_getrandom, sys_getrlimit,
    sys_getrusage, sys_gettimeofday, sys_nanosleep, sys_personality, sys_prlimit64, sys_rseq,
    sys_sched_get_priority_max, sys_sched_get_priority_min, sys_sched_getaffinity,
    sys_sched_getparam, sys_sched_getscheduler, sys_sched_rr_get_interval, sys_sched_setaffinity,
    sys_sched_setparam, sys_sched_setscheduler, sys_set_robust_list, sys_setdomainname,
    sys_sethostname, sys_setitimer, sys_setpriority, sys_setrlimit, sys_sigaltstack, sys_sysinfo,
    sys_tgkill, sys_time, sys_times, sys_tkill, sys_uname,
};
pub use lifecycle::{
    sys_arch_prctl, sys_brk, sys_clone, sys_clone3, sys_execve, sys_exit, sys_exit_group, sys_fork,
    sys_prctl, sys_sched_yield, sys_set_tid_address, sys_vfork, sys_wait4, sys_waitid,
};
