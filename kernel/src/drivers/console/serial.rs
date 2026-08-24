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

use super::super::traits::{CharDevice, DriverError, DriverInfo, PollResult};

/// The serial console driver instance.
pub struct SerialConsole;

impl CharDevice for SerialConsole {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, DriverError> {
        // TODO: Read from serial port with buffering
        Err(DriverError::NotReady)
    }

    fn write(&self, data: &[u8]) -> Result<usize, DriverError> {
        // Write each byte through the serial port
        for &byte in data {
            crate::arch::x86_64::serial::_print(format_args!("{}", byte as char));
        }
        Ok(data.len())
    }

    fn poll(&self) -> PollResult {
        PollResult {
            readable: false,
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
    let driver = SerialConsole;
    let info = driver.info();
    crate::drivers::register_driver(info);
}
