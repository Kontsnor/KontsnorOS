//! counter-based userspace synchronization eventfd.

use crate::fs::inode::{
    is_inode_nonblocking, DirEntry, FileType, Inode, InodeOps, POLLIN, POLLOUT,
};
use crate::sync::wait_queue::WaitQueue;
use crate::syscall::{Errno, SyscallResult};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

pub struct EventFd {
    inode: Inode,
    pub counter: Mutex<u64>,
    pub semaphore: bool,
    pub wait_queue: Arc<WaitQueue>,
}

impl EventFd {
    pub fn new(initval: u64, semaphore: bool) -> Self {
        Self {
            inode: Inode::new(0, FileType::Regular),
            counter: Mutex::new(initval),
            semaphore,
            wait_queue: Arc::new(WaitQueue::new()),
        }
    }
}

impl InodeOps for EventFd {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn as_eventfd(&self) -> Option<&EventFd> {
        Some(self)
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.len() < 8 {
            return Err(-22); // EINVAL
        }

        loop {
            let mut val = 0u64;
            let ready = {
                let mut counter = self.counter.lock();
                if *counter > 0 {
                    if self.semaphore {
                        val = 1;
                        *counter -= 1;
                    } else {
                        val = *counter;
                        *counter = 0;
                    }
                    true
                } else {
                    false
                }
            };

            if ready {
                buf[..8].copy_from_slice(&val.to_ne_bytes());
                self.wait_queue.wake_all();
                return Ok(8);
            }

            if is_inode_nonblocking(self) {
                return Err(-11); // EAGAIN
            }

            // Sleep/block
            self.wait_queue.wait();
        }
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        if data.len() < 8 {
            return Err(-22); // EINVAL
        }

        let val = u64::from_ne_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]);

        if val == u64::MAX {
            return Err(-22); // EINVAL
        }

        loop {
            let ready = {
                let mut counter = self.counter.lock();
                if *counter < u64::MAX - 1 - val {
                    *counter += val;
                    true
                } else {
                    false
                }
            };

            if ready {
                self.wait_queue.wake_all();
                return Ok(8);
            }

            if is_inode_nonblocking(self) {
                return Err(-11); // EAGAIN
            }

            // Sleep/block
            self.wait_queue.wait();
        }
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        let counter = self.counter.lock();
        if (events & POLLIN) != 0 {
            if *counter > 0 {
                revents |= POLLIN;
            }
        }
        if (events & POLLOUT) != 0 {
            if *counter < u64::MAX - 1 {
                revents |= POLLOUT;
            }
        }
        revents
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }
}

/// `sys_eventfd2(initval, flags)` — Create an eventfd.
pub fn sys_eventfd2(initval: u32, flags: i32) -> SyscallResult {
    let semaphore = (flags & 1) != 0; // EFD_SEMAPHORE = 1
    let nonblock = (flags & 0o4000) != 0; // EFD_NONBLOCK = O_NONBLOCK = 0o4000
    let cloexec = (flags & 0x80000) != 0; // EFD_CLOEXEC = O_CLOEXEC = 0x80000

    let mut open_flags = crate::fs::file::OpenFlags::O_RDWR;
    if nonblock {
        open_flags |= crate::fs::file::OpenFlags::O_NONBLOCK;
    }
    if cloexec {
        open_flags |= crate::fs::file::OpenFlags::O_CLOEXEC;
    }

    let eventfd = Arc::new(EventFd::new(initval as u64, semaphore));
    match crate::process::fd::current_task_alloc_fd_with_flags(
        eventfd,
        crate::fs::file::OpenFlags(open_flags),
    ) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}
