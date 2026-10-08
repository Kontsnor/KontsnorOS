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

//! Unix pipes — unidirectional byte stream IPC.
//!
//! A pipe provides a one-way data channel between processes.
//! Data written to the write end can be read from the read end.
//!
//! ```text
//! Writer Process ──write()──→ [Ring Buffer] ──read()──→ Reader Process
//! ```

use crate::sync::wait_queue::WaitQueue;
use alloc::sync::Arc;
use spin::Mutex;

/// Default pipe buffer size (64 KiB, matching Linux).
const PIPE_BUF_SIZE: usize = 64 * 1024;

/// A pipe buffer.
pub struct Pipe {
    /// Ring buffer for pipe data.
    buffer: Mutex<PipeBuffer>,
    /// Whether the write end is still open.
    write_open: Mutex<bool>,
    /// Whether the read end is still open.
    read_open: Mutex<bool>,
    /// Wait queue for blocking threads when pipe is full or empty.
    wait_queue: WaitQueue,
}

struct PipeBuffer {
    data: [u8; PIPE_BUF_SIZE],
    read_pos: usize,
    write_pos: usize,
    count: usize,
}

impl PipeBuffer {
    /// Push a slice of bytes into the ring buffer using bulk memory copies.
    ///
    /// Performance: Replaces byte-by-byte loop iterations and modulo division with at most two
    /// `copy_from_slice` calls (vectorized bulk memory moves), reducing O(N) loop overheads
    /// to O(1) bulk operations and maximizing CPU pipeline instruction throughput.
    fn push_slice(&mut self, src: &[u8]) -> usize {
        let available = PIPE_BUF_SIZE - self.count;
        if available == 0 || src.is_empty() {
            return 0;
        }
        let to_write = core::cmp::min(src.len(), available);

        // First contiguous chunk: from write_pos to end of buffer
        let first_chunk = core::cmp::min(to_write, PIPE_BUF_SIZE - self.write_pos);
        self.data[self.write_pos..self.write_pos + first_chunk]
            .copy_from_slice(&src[..first_chunk]);

        // Second contiguous chunk: wrap around to start of ring buffer
        let second_chunk = to_write - first_chunk;
        if second_chunk > 0 {
            self.data[..second_chunk].copy_from_slice(&src[first_chunk..to_write]);
        }

        self.write_pos = (self.write_pos + to_write) & (PIPE_BUF_SIZE - 1);
        self.count += to_write;
        to_write
    }

    /// Pop bytes from the ring buffer into a slice using bulk memory copies.
    ///
    /// Performance: Replaces byte-by-byte loop iterations and modulo division with at most two
    /// `copy_from_slice` calls, drastically reducing IPC read latency.
    fn pop_slice(&mut self, dst: &mut [u8]) -> usize {
        if self.count == 0 || dst.is_empty() {
            return 0;
        }
        let to_read = core::cmp::min(dst.len(), self.count);

        // First contiguous chunk: from read_pos to end of buffer
        let first_chunk = core::cmp::min(to_read, PIPE_BUF_SIZE - self.read_pos);
        dst[..first_chunk].copy_from_slice(&self.data[self.read_pos..self.read_pos + first_chunk]);

        // Second contiguous chunk: wrap around to start of ring buffer
        let second_chunk = to_read - first_chunk;
        if second_chunk > 0 {
            dst[first_chunk..to_read].copy_from_slice(&self.data[..second_chunk]);
        }

        self.read_pos = (self.read_pos + to_read) & (PIPE_BUF_SIZE - 1);
        self.count -= to_read;
        to_read
    }
}

impl Pipe {
    /// Create a new pipe.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            buffer: Mutex::new(PipeBuffer {
                data: [0; PIPE_BUF_SIZE],
                read_pos: 0,
                write_pos: 0,
                count: 0,
            }),
            write_open: Mutex::new(true),
            read_open: Mutex::new(true),
            wait_queue: WaitQueue::new(),
        })
    }

    /// Write data to the pipe.
    ///
    /// Returns the number of bytes written, or an error if the
    /// read end is closed (EPIPE).
    pub fn write(&self, data: &[u8]) -> Result<usize, i32> {
        if data.is_empty() {
            return Ok(0);
        }

        let mut written = 0;
        while written < data.len() {
            let tok = self.wait_queue.token();
            if !*self.read_open.lock() {
                if written > 0 {
                    return Ok(written);
                }
                return Err(-13); // EPIPE
            }

            let space_available = {
                let mut buf = self.buffer.lock();
                let n = buf.push_slice(&data[written..]);
                if n > 0 {
                    written += n;
                    drop(buf);
                    self.wait_queue.wake_all();
                    true
                } else {
                    false
                }
            };

            if !space_available {
                self.wait_queue.wait_since(tok);
            }
        }

        Ok(written)
    }

    /// Read data from the pipe.
    ///
    /// Returns the number of bytes read, or 0 if the write end is
    /// closed and the buffer is empty (EOF).
    pub fn read(&self, out: &mut [u8]) -> Result<usize, i32> {
        if out.is_empty() {
            return Ok(0);
        }

        loop {
            let tok = self.wait_queue.token();
            let mut buf = self.buffer.lock();

            if buf.count > 0 {
                let count = buf.pop_slice(out);
                drop(buf);
                self.wait_queue.wake_all();
                return Ok(count);
            }

            if !*self.write_open.lock() {
                return Ok(0); // EOF — write end closed, no data left
            }

            drop(buf);

            self.wait_queue.wait_since(tok);
        }
    }

    /// Close the write end of the pipe.
    pub fn close_write(&self) {
        *self.write_open.lock() = false;
        self.wait_queue.wake_all();
    }

    /// Close the read end of the pipe.
    pub fn close_read(&self) {
        *self.read_open.lock() = false;
        self.wait_queue.wake_all();
    }

    /// Check if the pipe has data available for reading.
    pub fn has_data(&self) -> bool {
        self.buffer.lock().count > 0
    }

    /// Check if the pipe has space for writing.
    pub fn has_space(&self) -> bool {
        self.buffer.lock().count < PIPE_BUF_SIZE
    }
}
