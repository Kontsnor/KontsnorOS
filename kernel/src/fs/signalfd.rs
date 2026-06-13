//! signalfd — read POSIX signals via a file descriptor.

use crate::fs::inode::{is_inode_nonblocking, DirEntry, FileType, Inode, InodeOps, POLLIN};
use crate::sync::wait_queue::WaitQueue;
use crate::syscall::{Errno, SyscallResult};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SignalFdSiginfo {
    pub ssi_signo: u32,   /* Signal number */
    pub ssi_errno: i32,   /* Error number */
    pub ssi_code: i32,    /* Signal code */
    pub ssi_pid: u32,     /* PID of sender */
    pub ssi_uid: u32,     /* Real UID of sender */
    pub ssi_fd: i32,      /* File descriptor */
    pub ssi_tid: u32,     /* Kernel timer ID */
    pub ssi_band: u32,    /* Band event */
    pub ssi_overrun: u32, /* POSIX timer overrun count */
    pub ssi_trapno: u32,  /* Trap number that caused signal */
    pub ssi_status: i32,  /* Exit value or signal */
    pub ssi_int: i32,     /* Integer sent by sigqueue */
    pub ssi_ptr: u64,     /* Pointer sent by sigqueue */
    pub ssi_utime: u64,   /* User CPU time consumed */
    pub ssi_stime: u64,   /* System CPU time consumed */
    pub ssi_addr: u64,    /* Address that generated signal */
    pub _pad: [u8; 48],   /* Pad size to 128 bytes */
}

impl Default for SignalFdSiginfo {
    fn default() -> Self {
        Self {
            ssi_signo: 0,
            ssi_errno: 0,
            ssi_code: 0,
            ssi_pid: 0,
            ssi_uid: 0,
            ssi_fd: 0,
            ssi_tid: 0,
            ssi_band: 0,
            ssi_overrun: 0,
            ssi_trapno: 0,
            ssi_status: 0,
            ssi_int: 0,
            ssi_ptr: 0,
            ssi_utime: 0,
            ssi_stime: 0,
            ssi_addr: 0,
            _pad: [0; 48],
        }
    }
}

pub struct SignalFd {
    inode: Inode,
    pub mask: Mutex<u64>,
    pub wait_queue: Arc<WaitQueue>,
}

impl SignalFd {
    pub fn new(mask: u64) -> Self {
        Self {
            inode: Inode::new(0, FileType::Regular),
            mask: Mutex::new(mask),
            wait_queue: Arc::new(WaitQueue::new()),
        }
    }
}

impl InodeOps for SignalFd {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn as_signalfd(&self) -> Option<&SignalFd> {
        Some(self)
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.len() < core::mem::size_of::<SignalFdSiginfo>() {
            return Err(-22); // EINVAL
        }

        let pid = crate::process::scheduler::current_pid().ok_or(-3)?; // ESRCH
        let task_arc = crate::process::scheduler::get_task_arc(pid).ok_or(-3)?; // ESRCH

        loop {
            let mut task = task_arc.lock();
            let mask = self.mask.lock();
            let pending_matching = task.pending_signals & *mask;

            if pending_matching != 0 {
                // Find the lowest matching signal (1-based index)
                let sig = pending_matching.trailing_zeros() + 1;
                // Dequeue the signal
                task.pending_signals &= !(1 << (sig - 1));

                // Clear pending flag in CPU scratch if no more signals
                let pending_unblocked = task.pending_signals & !task.blocked_signals;
                let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
                unsafe {
                    if apic_id < 32 {
                        crate::syscall::CPU_SCRATCHES[apic_id].signals_pending =
                            if pending_unblocked != 0 { 1 } else { 0 };
                    }
                }

                drop(mask);
                drop(task);

                let siginfo = SignalFdSiginfo {
                    ssi_signo: sig,
                    ..Default::default()
                };

                let ptr = &siginfo as *const SignalFdSiginfo as *const u8;
                let slice = unsafe {
                    core::slice::from_raw_parts(ptr, core::mem::size_of::<SignalFdSiginfo>())
                };
                buf[..core::mem::size_of::<SignalFdSiginfo>()].copy_from_slice(slice);

                return Ok(core::mem::size_of::<SignalFdSiginfo>());
            }

            if is_inode_nonblocking(self) {
                return Err(-11); // EAGAIN
            }

            drop(mask);
            drop(task);

            // Wait/block
            self.wait_queue.wait();
        }
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        if (events & POLLIN) != 0 {
            if let Some(pid) = crate::process::scheduler::current_pid() {
                if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
                    let task = task_arc.lock();
                    let mask = self.mask.lock();
                    if (task.pending_signals & *mask) != 0 {
                        revents |= POLLIN;
                    }
                }
            }
        }
        revents
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }
}

/// `sys_signalfd4(fd, mask, sizemask, flags)` — Create or update a signalfd.
pub fn sys_signalfd4(fd: i32, mask: *const u64, sizemask: usize, flags: i32) -> SyscallResult {
    if mask.is_null() || sizemask != 8 {
        return Errno::EINVAL.into();
    }
    if !crate::syscall::validation::validate_user_ptr(mask as *const u8, 8) {
        return Errno::EFAULT.into();
    }

    let signal_mask = unsafe { *mask };

    if fd >= 0 {
        // Update existing signalfd
        let inode = match crate::process::fd::current_task_read_fd(fd) {
            Some(i) => i,
            None => return Errno::EBADF.into(),
        };
        let signalfd = match inode.as_signalfd() {
            Some(s) => s,
            None => return Errno::EINVAL.into(),
        };
        *signalfd.mask.lock() = signal_mask;
        fd as SyscallResult
    } else {
        // Create new signalfd
        let nonblock = (flags & 0o4000) != 0; // SFD_NONBLOCK = O_NONBLOCK = 0o4000
        let cloexec = (flags & 0x80000) != 0; // SFD_CLOEXEC = O_CLOEXEC = 0x80000

        let mut open_flags = crate::fs::file::OpenFlags::O_RDWR;
        if nonblock {
            open_flags |= crate::fs::file::OpenFlags::O_NONBLOCK;
        }
        if cloexec {
            open_flags |= crate::fs::file::OpenFlags::O_CLOEXEC;
        }

        let signalfd = Arc::new(SignalFd::new(signal_mask));
        match crate::process::fd::current_task_alloc_fd_with_flags(
            signalfd,
            crate::fs::file::OpenFlags(open_flags),
        ) {
            Some(new_fd) => new_fd as SyscallResult,
            None => Errno::EMFILE.into(),
        }
    }
}
