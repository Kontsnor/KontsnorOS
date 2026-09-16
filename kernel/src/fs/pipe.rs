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

//! POSIX-compatible unidirectional pipes.
//!
//! A pipe is a unidirectional data channel that can be used for
//! interprocess communication. It is backed by a ring-buffer in memory.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::fs::inode::{DirEntry, FileType, Inode, InodeOps};

const PIPE_BUF_SIZE: usize = 65536;

/// Circular queue for pipe data.
struct PipeBuffer {
    data: Vec<u8>,
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

impl PipeBuffer {
    fn new() -> Self {
        Self {
            data: alloc::vec![0u8; PIPE_BUF_SIZE],
            read_pos: 0,
            write_pos: 0,
            len: 0,
        }
    }

    fn is_full(&self) -> bool {
        self.len == PIPE_BUF_SIZE
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Push a slice of bytes into the circular buffer using fast contiguous block memory copies.
    ///
    /// Performance: Replaces byte-by-byte loop iterations and modulo division with at most two
    /// `copy_from_slice` calls (vectorized block memory moves), reducing O(N) loop overheads
    /// to O(1) bulk operations.
    fn push_slice(&mut self, src: &[u8]) -> usize {
        let available = PIPE_BUF_SIZE - self.len;
        if available == 0 || src.is_empty() {
            return 0;
        }
        let to_write = core::cmp::min(src.len(), available);

        // First contiguous chunk: from write_pos to buffer end (or write end)
        let first_chunk = core::cmp::min(to_write, PIPE_BUF_SIZE - self.write_pos);
        self.data[self.write_pos..self.write_pos + first_chunk]
            .copy_from_slice(&src[..first_chunk]);

        // Second contiguous chunk: wrap around to start of ring buffer
        let second_chunk = to_write - first_chunk;
        if second_chunk > 0 {
            self.data[..second_chunk].copy_from_slice(&src[first_chunk..to_write]);
        }

        self.write_pos = (self.write_pos + to_write) & (PIPE_BUF_SIZE - 1);
        self.len += to_write;
        to_write
    }

    /// Pop bytes from the circular buffer into a slice using fast contiguous block memory copies.
    ///
    /// Performance: Replaces byte-by-byte loop iterations and modulo division with at most two
    /// `copy_from_slice` calls, maximizing CPU throughput during pipe IPC reads.
    fn pop_slice(&mut self, dst: &mut [u8]) -> usize {
        if self.len == 0 || dst.is_empty() {
            return 0;
        }
        let to_read = core::cmp::min(dst.len(), self.len);

        // First contiguous chunk: from read_pos to buffer end (or read end)
        let first_chunk = core::cmp::min(to_read, PIPE_BUF_SIZE - self.read_pos);
        dst[..first_chunk].copy_from_slice(&self.data[self.read_pos..self.read_pos + first_chunk]);

        // Second contiguous chunk: wrap around to start of ring buffer
        let second_chunk = to_read - first_chunk;
        if second_chunk > 0 {
            dst[first_chunk..to_read].copy_from_slice(&self.data[..second_chunk]);
        }

        self.read_pos = (self.read_pos + to_read) & (PIPE_BUF_SIZE - 1);
        self.len -= to_read;
        to_read
    }
}

/// Shared state of a unidirectional pipe.
pub struct PipeState {
    buffer: Mutex<PipeBuffer>,
    readers: AtomicUsize,
    writers: AtomicUsize,
    pub wait_queue: Arc<crate::sync::wait_queue::WaitQueue>,
}

impl PipeState {
    fn new() -> Self {
        Self {
            buffer: Mutex::new(PipeBuffer::new()),
            readers: AtomicUsize::new(1),
            writers: AtomicUsize::new(1),
            wait_queue: Arc::new(crate::sync::wait_queue::WaitQueue::new()),
        }
    }
}

/// Read end of the pipe.
pub struct PipeReader {
    inode: Inode,
    state: Arc<PipeState>,
    non_blocking: core::sync::atomic::AtomicBool,
}

impl InodeOps for PipeReader {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn wait_queue(&self) -> Option<Arc<crate::sync::wait_queue::WaitQueue>> {
        Some(self.state.wait_queue.clone())
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            // Atomic check under buffer lock: read data if present, or detect EOF if writers closed
            {
                let mut guard = self.state.buffer.lock();
                if !guard.is_empty() {
                    let count = guard.pop_slice(buf);
                    drop(guard);
                    self.state.wait_queue.wake_all();
                    return Ok(count);
                }

                // Buffer is empty under the lock. Check if all writers have closed.
                // Holding the lock ensures no writer can push bytes before we observe writers == 0.
                if self.state.writers.load(Ordering::SeqCst) == 0 {
                    return Ok(0); // EOF
                }
            }

            if self.non_blocking.load(Ordering::SeqCst) {
                return Err(-11); // EAGAIN / EWOULDBLOCK
            }

            // Pre-check before blocking: re-evaluate if data arrived or writers closed
            if !self.state.buffer.lock().is_empty()
                || self.state.writers.load(Ordering::SeqCst) == 0
            {
                continue;
            }

            // Sleep on wait queue until data is written or writers close
            self.state.wait_queue.wait();

            // Interrupted by signal
            if let Some(current_pid) = crate::process::scheduler::current_pid() {
                if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
                    let task = task_arc.lock();
                    if (task.pending_signals & !task.blocked_signals) != 0 {
                        return Err(-4); // EINTR
                    }
                }
            }
        }
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        if request == 0x5421 {
            // FIONBIO
            if !crate::syscall::fs::validate_user_ptr(arg as *const u8, 4) {
                return Err(-14); // EFAULT
            }
            let val = unsafe { *(arg as *const i32) };
            self.non_blocking.store(val != 0, Ordering::SeqCst);
            Ok(0)
        } else {
            Err(-22) // EINVAL
        }
    }

    fn set_nonblocking(&self, nonblocking: bool) {
        self.non_blocking.store(nonblocking, Ordering::SeqCst);
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        let buf = self.state.buffer.lock();
        let writers = self.state.writers.load(Ordering::SeqCst);
        let has_data = !buf.is_empty();
        if (events & crate::fs::inode::POLLIN) != 0 {
            if has_data || writers == 0 {
                revents |= crate::fs::inode::POLLIN;
            }
        }
        // POSIX: POLLHUP is only returned when the peer closed AND all buffered data has been consumed.
        if writers == 0 && !has_data {
            revents |= crate::fs::inode::POLLHUP;
        }
        revents
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.state.readers.fetch_sub(1, Ordering::SeqCst);
        self.state.wait_queue.wake_all();
    }
}

/// Write end of the pipe.
pub struct PipeWriter {
    inode: Inode,
    state: Arc<PipeState>,
    non_blocking: core::sync::atomic::AtomicBool,
}

impl InodeOps for PipeWriter {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn wait_queue(&self) -> Option<Arc<crate::sync::wait_queue::WaitQueue>> {
        Some(self.state.wait_queue.clone())
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        if data.is_empty() {
            return Ok(0);
        }

        let mut written = 0;
        while written < data.len() {
            // Check if readers are closed
            if self.state.readers.load(Ordering::SeqCst) == 0 {
                if let Some(current_pid) = crate::process::scheduler::current_pid() {
                    crate::syscall::signal::deliver_signal(current_pid, 13); // SIGPIPE = 13
                }
                return Err(-32); // EPIPE
            }

            let bytes_pushed = {
                let mut guard = self.state.buffer.lock();
                if !guard.is_full() {
                    let n = guard.push_slice(&data[written..]);
                    written += n;
                    drop(guard);
                    self.state.wait_queue.wake_all();
                    n
                } else {
                    0
                }
            };

            if bytes_pushed == 0 {
                if self.non_blocking.load(Ordering::SeqCst) {
                    if written > 0 {
                        return Ok(written);
                    } else {
                        return Err(-11); // EAGAIN
                    }
                }
                // Pre-check before blocking: re-evaluate if space freed or readers closed
                if !self.state.buffer.lock().is_full()
                    || self.state.readers.load(Ordering::SeqCst) == 0
                {
                    continue;
                }
                // Sleep on wait queue until space is freed or readers close
                self.state.wait_queue.wait();

                // Interrupted by signal
                if let Some(current_pid) = crate::process::scheduler::current_pid() {
                    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
                        let task = task_arc.lock();
                        if (task.pending_signals & !task.blocked_signals) != 0 {
                            return Err(-4); // EINTR
                        }
                    }
                }
            }
        }

        Ok(written)
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        if request == 0x5421 {
            // FIONBIO
            if !crate::syscall::fs::validate_user_ptr(arg as *const u8, 4) {
                return Err(-14); // EFAULT
            }
            let val = unsafe { *(arg as *const i32) };
            self.non_blocking.store(val != 0, Ordering::SeqCst);
            Ok(0)
        } else {
            Err(-22) // EINVAL
        }
    }

    fn set_nonblocking(&self, nonblocking: bool) {
        self.non_blocking.store(nonblocking, Ordering::SeqCst);
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        let buf = self.state.buffer.lock();
        if (events & crate::fs::inode::POLLOUT) != 0 {
            if !buf.is_full() {
                revents |= crate::fs::inode::POLLOUT;
            }
        }
        if self.state.readers.load(Ordering::SeqCst) == 0 {
            revents |= crate::fs::inode::POLLERR | crate::fs::inode::POLLHUP;
        }
        revents
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.state.writers.fetch_sub(1, Ordering::SeqCst);
        self.state.wait_queue.wake_all();
    }
}

/// Construct a new unidirectional VFS pipe, returning (Reader, Writer) tuple.
pub fn make_pipe() -> (Arc<dyn InodeOps>, Arc<dyn InodeOps>) {
    let state = Arc::new(PipeState::new());

    let reader = Arc::new(PipeReader {
        inode: Inode::new(0, FileType::Pipe),
        state: state.clone(),
        non_blocking: core::sync::atomic::AtomicBool::new(false),
    });

    let writer = Arc::new(PipeWriter {
        inode: Inode::new(0, FileType::Pipe),
        state,
        non_blocking: core::sync::atomic::AtomicBool::new(false),
    });

    (reader, writer)
}

/// Bidirectional Unix socket created via socketpair.
pub struct UnixSocket {
    pub inode: Inode,
    pub reader: Arc<dyn InodeOps>,
    pub writer: Arc<dyn InodeOps>,
    pub wait_queue: Arc<crate::sync::wait_queue::WaitQueue>,
}

impl InodeOps for UnixSocket {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn wait_queue(&self) -> Option<Arc<crate::sync::wait_queue::WaitQueue>> {
        Some(self.wait_queue.clone())
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        self.reader.read(offset, buf)
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<usize, i32> {
        self.writer.write(offset, data)
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        let _ = self.writer.ioctl(request, arg);
        self.reader.ioctl(request, arg)
    }

    fn set_nonblocking(&self, nonblocking: bool) {
        self.reader.set_nonblocking(nonblocking);
        self.writer.set_nonblocking(nonblocking);
    }

    fn poll(&self, events: u32) -> u32 {
        self.reader.poll(events) | self.writer.poll(events)
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }
}

/// Construct a bidirectional connected socket pair.
pub fn make_socketpair(nonblock: bool) -> (Arc<dyn InodeOps>, Arc<dyn InodeOps>) {
    let (r1, w1) = make_pipe();
    let (r2, w2) = make_pipe();

    if nonblock {
        r1.set_nonblocking(true);
        w1.set_nonblocking(true);
        r2.set_nonblocking(true);
        w2.set_nonblocking(true);
    }

    let wq_a = Arc::new(crate::sync::wait_queue::WaitQueue::new());
    let wq_b = Arc::new(crate::sync::wait_queue::WaitQueue::new());

    if let Some(w) = r1.wait_queue() {
        w.add_listener(&wq_a);
    }
    if let Some(w) = w2.wait_queue() {
        w.add_listener(&wq_a);
    }
    if let Some(w) = r2.wait_queue() {
        w.add_listener(&wq_b);
    }
    if let Some(w) = w1.wait_queue() {
        w.add_listener(&wq_b);
    }

    let sock_a = Arc::new(UnixSocket {
        inode: Inode::new(0, FileType::Socket),
        reader: r1,
        writer: w2,
        wait_queue: wq_a,
    });

    let sock_b = Arc::new(UnixSocket {
        inode: Inode::new(0, FileType::Socket),
        reader: r2,
        writer: w1,
        wait_queue: wq_b,
    });

    (sock_a, sock_b)
}
