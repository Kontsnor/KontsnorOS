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
            license: String::from("MIT OR Apache-2.0"),
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
