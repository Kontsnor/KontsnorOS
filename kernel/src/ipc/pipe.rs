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

use alloc::sync::Arc;
use spin::Mutex;
use crate::sync::wait_queue::WaitQueue;

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
            if !*self.read_open.lock() {
                if written > 0 {
                    return Ok(written);
                }
                return Err(-13); // EPIPE
            }

            let space_available = {
                let mut buf = self.buffer.lock();
                let available = PIPE_BUF_SIZE - buf.count;
                if available > 0 {
                    let to_write = (data.len() - written).min(available);
                    for &byte in &data[written..written + to_write] {
                        let pos = buf.write_pos;
                        buf.data[pos] = byte;
                        buf.write_pos = (pos + 1) % PIPE_BUF_SIZE;
                        buf.count += 1;
                    }
                    written += to_write;
                    drop(buf);
                    self.wait_queue.wake_all();
                    true
                } else {
                    false
                }
            };

            if !space_available {
                // Pre-check before blocking: re-evaluate if space freed or read end closed
                if self.buffer.lock().count < PIPE_BUF_SIZE || !*self.read_open.lock() {
                    continue;
                }
                self.wait_queue.wait();
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
            let mut buf = self.buffer.lock();

            if buf.count > 0 {
                let to_read = out.len().min(buf.count);
                for byte in &mut out[..to_read] {
                    let pos = buf.read_pos;
                    *byte = buf.data[pos];
                    buf.read_pos = (pos + 1) % PIPE_BUF_SIZE;
                    buf.count -= 1;
                }
                drop(buf);
                self.wait_queue.wake_all();
                return Ok(to_read);
            }

            if !*self.write_open.lock() {
                return Ok(0); // EOF — write end closed, no data left
            }

            drop(buf);

            // Pre-check before blocking: re-evaluate if data arrived or write end closed
            if self.buffer.lock().count > 0 || !*self.write_open.lock() {
                continue;
            }

            self.wait_queue.wait();
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
