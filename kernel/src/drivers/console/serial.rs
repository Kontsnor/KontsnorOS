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
//! and exposes it as a CharDevice through the driver framework.

use alloc::string::String;
use spin::Mutex;

use super::super::traits::{CharDevice, DriverError, DriverInfo, PollResult};
use crate::util::ring_buffer::RingBuffer;

/// Capacity of the serial console receive buffer.
const RX_BUFFER_SIZE: usize = 256;

/// The serial console driver instance.
pub struct SerialConsole {
    rx_buffer: Mutex<RingBuffer<u8, RX_BUFFER_SIZE>>,
}

impl SerialConsole {
    /// Create a new serial console driver instance.
    pub const fn new() -> Self {
        Self {
            rx_buffer: Mutex::new(RingBuffer::new()),
        }
    }

    /// Enqueue a byte into the receive ring buffer.
    pub fn enqueue_byte(&self, byte: u8) -> Result<(), u8> {
        self.rx_buffer.lock().push(byte)
    }
}

impl Default for SerialConsole {
    fn default() -> Self {
        Self::new()
    }
}

impl CharDevice for SerialConsole {
    fn read(&self, buf: &mut [u8]) -> Result<usize, DriverError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut rx = self.rx_buffer.lock();

        // First, drain any pending hardware bytes from the serial port into the buffer
        while rx.len() < RX_BUFFER_SIZE {
            if let Some(byte) = crate::arch::x86_64::serial::try_read_byte() {
                if rx.push(byte).is_err() {
                    break; // Buffer full
                }
            } else {
                break; // Hardware FIFO empty
            }
        }

        // Dequeue available bytes into user buffer
        let mut copied = 0;
        while copied < buf.len() {
            if let Some(byte) = rx.pop() {
                buf[copied] = byte;
                copied += 1;
            } else {
                break;
            }
        }

        if copied > 0 {
            Ok(copied)
        } else {
            Err(DriverError::NotReady)
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
        let rx = self.rx_buffer.lock();
        let readable = !rx.is_empty() || crate::arch::x86_64::serial::has_data();
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
    let driver = SerialConsole::new();
    let info = driver.info();
    crate::drivers::register_driver(info);
}
