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

//! Serial console driver.
//!
//! This driver wraps the low-level serial port from `arch::x86_64::serial`
//! and exposes it as a CharDevice through the driver framework with
//! interrupt-backed ring buffer input.

use alloc::string::String;

use super::super::traits::{CharDevice, DriverError, DriverInfo, PollResult};
use crate::sync::spinlock::TicketLock;

/// Size of the serial console input ring buffer (4 KiB, power-of-2 for fast masking).
const BUFFER_CAPACITY: usize = 4096;

/// Fixed-capacity ring buffer for storing received serial bytes.
struct SerialRingBuffer {
    data: [u8; BUFFER_CAPACITY],
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

impl SerialRingBuffer {
    const fn new() -> Self {
        Self {
            data: [0u8; BUFFER_CAPACITY],
            read_pos: 0,
            write_pos: 0,
            len: 0,
        }
    }

    /// Push one byte into the buffer. Drops byte if buffer is full.
    fn push(&mut self, byte: u8) {
        if self.len < BUFFER_CAPACITY {
            self.data[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) & (BUFFER_CAPACITY - 1);
            self.len += 1;
        }
    }

    /// Pop one byte from the buffer. Returns `None` if empty.
    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.data[self.read_pos];
        self.read_pos = (self.read_pos + 1) & (BUFFER_CAPACITY - 1);
        self.len -= 1;
        Some(byte)
    }

    /// Check whether buffer contains data.
    fn has_data(&self) -> bool {
        self.len > 0
    }
}

/// Global serial input ring buffer.
static SERIAL_BUFFER: TicketLock<SerialRingBuffer> = TicketLock::new(SerialRingBuffer::new());

/// Drain hardware UART FIFO into the ring buffer.
///
/// This function polls the raw hardware serial port and buffers any newly
/// arrived bytes. Safe to call from interrupt context or driver methods.
pub fn handle_interrupt() {
    while let Some(byte) = crate::arch::x86_64::serial::raw_try_read_byte() {
        SERIAL_BUFFER.lock().push(byte);
    }
}

/// Read a single byte from the serial ring buffer (non-blocking).
///
/// Automatically drains any pending hardware FIFO bytes first.
pub fn read_byte() -> Option<u8> {
    handle_interrupt();
    SERIAL_BUFFER.lock().pop()
}

/// Push a byte directly into the serial ring buffer (for tests or manual injection).
pub fn push_byte(byte: u8) {
    SERIAL_BUFFER.lock().push(byte);
}

/// The serial console driver instance.
pub struct SerialConsole;

impl CharDevice for SerialConsole {
    fn read(&self, buf: &mut [u8]) -> Result<usize, DriverError> {
        if buf.is_empty() {
            return Ok(0);
        }

        handle_interrupt();

        let mut ring = SERIAL_BUFFER.lock();
        let mut count = 0;
        while count < buf.len() {
            if let Some(b) = ring.pop() {
                buf[count] = b;
                count += 1;
            } else {
                break;
            }
        }

        if count == 0 {
            Err(DriverError::NotReady)
        } else {
            Ok(count)
        }
    }

    fn write(&self, data: &[u8]) -> Result<usize, DriverError> {
        // Write each byte through the serial port
        for &byte in data {
            crate::arch::x86_64::serial::_print(format_args!("{}", byte as char));
        }
        Ok(data.len())
    }

    fn poll(&self) -> PollResult {
        handle_interrupt();
        let readable = SERIAL_BUFFER.lock().has_data();
        PollResult {
            readable,
            writable: true,
            error: false,
        }
    }

    fn info(&self) -> DriverInfo {
        DriverInfo {
            name: String::from("serial-console"),
            version: String::from("0.1.0"),
            author: String::from("KontsnorOS"),
            license: String::from("GPL-3.0-only"),
            description: String::from("Serial console driver (COM1)"),
        }
    }
}

/// Initialize the serial console driver.
pub fn init() {
    crate::arch::x86_64::serial::enable_interrupts();
    let driver = SerialConsole;
    let info = driver.info();
    crate::drivers::register_driver(info);
}
