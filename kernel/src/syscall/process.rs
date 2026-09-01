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
    calculate_exec_creds, sys_getegid, sys_geteuid, sys_getgid, sys_getpgid, sys_getpid,
    sys_getppid, sys_gettid, sys_getuid, sys_setgid, sys_setpgid, sys_setsid, sys_setuid,
};
pub use futex::sys_futex;
pub use info::{
    sys_clock_gettime, sys_get_robust_list, sys_getrandom, sys_getrlimit, sys_gettimeofday,
    sys_nanosleep, sys_prlimit64, sys_sched_getaffinity, sys_set_robust_list, sys_setrlimit,
    sys_sigaltstack, sys_sysinfo, sys_tgkill, sys_times, sys_tkill, sys_uname,
};
pub use lifecycle::{
    sys_arch_prctl, sys_brk, sys_clone, sys_execve, sys_exit, sys_exit_group, sys_fork, sys_prctl,
    sys_sched_yield, sys_set_tid_address, sys_wait4,
};
