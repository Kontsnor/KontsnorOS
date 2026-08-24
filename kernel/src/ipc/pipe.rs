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
        })
    }

    /// Write data to the pipe.
    ///
    /// Returns the number of bytes written, or an error if the
    /// read end is closed (EPIPE).
    pub fn write(&self, data: &[u8]) -> Result<usize, i32> {
        if !*self.read_open.lock() {
            return Err(-13); // EPIPE (in real POSIX, also delivers SIGPIPE)
        }

        let mut buf = self.buffer.lock();
        let available = PIPE_BUF_SIZE - buf.count;
        let to_write = data.len().min(available);

        if to_write == 0 {
            // TODO: Block until space is available
            return Ok(0);
        }

        for &byte in &data[..to_write] {
            let pos = buf.write_pos;
            buf.data[pos] = byte;
            buf.write_pos = (pos + 1) % PIPE_BUF_SIZE;
            buf.count += 1;
        }

        Ok(to_write)
    }

    /// Read data from the pipe.
    ///
    /// Returns the number of bytes read, or 0 if the write end is
    /// closed and the buffer is empty (EOF).
    pub fn read(&self, out: &mut [u8]) -> Result<usize, i32> {
        let mut buf = self.buffer.lock();

        if buf.count == 0 {
            if !*self.write_open.lock() {
                return Ok(0); // EOF — write end closed, no data left
            }
            // TODO: Block until data is available
            return Ok(0);
        }

        let to_read = out.len().min(buf.count);

        for byte in &mut out[..to_read] {
            let pos = buf.read_pos;
            *byte = buf.data[pos];
            buf.read_pos = (pos + 1) % PIPE_BUF_SIZE;
            buf.count -= 1;
        }

        Ok(to_read)
    }

    /// Close the write end of the pipe.
    pub fn close_write(&self) {
        *self.write_open.lock() = false;
    }

    /// Close the read end of the pipe.
    pub fn close_read(&self) {
        *self.read_open.lock() = false;
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
