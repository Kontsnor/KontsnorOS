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

//! File system system calls module.

pub mod io;
pub mod meta;
pub mod open;

pub use io::{
    sys_copy_file_range, sys_dup, sys_dup2, sys_dup3, sys_fallocate, sys_fcntl, sys_flock,
    sys_fsync, sys_ftruncate, sys_lseek, sys_memfd_create, sys_pipe, sys_pipe2, sys_pread64,
    sys_preadv, sys_preadv2, sys_pwrite64, sys_pwritev, sys_pwritev2, sys_read, sys_readahead,
    sys_readv, sys_sendfile, sys_splice, sys_sync, sys_sync_file_range, sys_syncfs, sys_tee,
    sys_vmsplice, sys_write, sys_writev, IoVec,
};
pub use meta::{
    sys_access, sys_chdir, sys_chmod, sys_chown, sys_epoll_create, sys_epoll_pwait,
    sys_epoll_pwait2, sys_faccessat, sys_fchmod, sys_fchmodat, sys_fchown, sys_fchownat,
    sys_fdatasync, sys_fgetxattr, sys_flistxattr, sys_fremovexattr, sys_fsetxattr, sys_fstat,
    sys_fstatfs, sys_getcwd, sys_getdents64, sys_getxattr, sys_lchown, sys_lgetxattr, sys_link,
    sys_linkat, sys_listxattr, sys_llistxattr, sys_lremovexattr, sys_lsetxattr, sys_lstat,
    sys_mkdir, sys_mkdirat, sys_mount, sys_newfstatat, sys_poll, sys_ppoll, sys_pselect6,
    sys_readlink, sys_readlinkat, sys_removexattr, sys_rename, sys_renameat, sys_renameat2,
    sys_rmdir, sys_select, sys_setxattr, sys_stat, sys_statfs, sys_statx, sys_symlink,
    sys_symlinkat, sys_umask, sys_umount2, sys_unlink, sys_unlinkat, sys_utime, sys_utimensat,
    sys_utimes, LinuxStat, LinuxStatfs, StatX, StatxTimestamp, TimeSpec, TimeVal, UTimeBuf,
};
pub use open::{
    sys_chroot, sys_close, sys_close_range, sys_creat, sys_fchdir, sys_open, sys_openat,
    sys_openat2, sys_pivot_root, sys_reboot, sys_truncate, OpenHow,
};

// Re-export the validation functions for backward compatibility so other modules can import them from `fs`
pub use crate::syscall::validation::{
    copy_string_from_user_pub, validate_user_ptr, validate_user_ptr_write,
};

pub use crate::fs::epoll::{sys_epoll_create1, sys_epoll_ctl, sys_epoll_wait};
pub use crate::fs::eventfd::sys_eventfd2;
pub use crate::fs::inotify::{
    sys_inotify_add_watch, sys_inotify_init, sys_inotify_init1, sys_inotify_rm_watch,
};
pub use crate::fs::pidfd::{sys_pidfd_getfd, sys_pidfd_open, sys_pidfd_send_signal};
pub use crate::fs::signalfd::sys_signalfd4;
pub use crate::fs::timerfd::{sys_timerfd_create, sys_timerfd_settime};
