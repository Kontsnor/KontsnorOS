//! File system system calls module.

pub mod io;
pub mod meta;
pub mod open;

pub use io::{
    sys_dup, sys_dup2, sys_fcntl, sys_fsync, sys_lseek, sys_pipe, sys_pread64, sys_read, sys_write,
    sys_writev, IoVec,
};
pub use meta::{
    sys_access, sys_chdir, sys_faccessat, sys_fstat, sys_getcwd, sys_getdents64, sys_link,
    sys_lstat, sys_mkdir, sys_newfstatat, sys_poll, sys_readlink, sys_readlinkat, sys_rename,
    sys_rmdir, sys_stat, sys_symlink, sys_symlinkat, sys_unlink, LinuxStat,
};
pub use open::{sys_close, sys_open, sys_openat};

// Re-export the validation functions for backward compatibility so other modules can import them from `fs`
pub use crate::syscall::validation::{
    copy_string_from_user, copy_string_from_user_pub, validate_user_ptr, validate_user_ptr_write,
};

pub use crate::fs::epoll::{sys_epoll_create1, sys_epoll_ctl, sys_epoll_wait};
pub use crate::fs::eventfd::sys_eventfd2;
pub use crate::fs::signalfd::sys_signalfd4;
pub use crate::fs::timerfd::{sys_timerfd_create, sys_timerfd_settime};
