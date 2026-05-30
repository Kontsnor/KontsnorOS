//! Serial port (UART 16550) driver for early boot logging.
//!
//! This module provides a global serial port interface for kernel logging.
//! The serial port is the primary output channel during boot before any
//! framebuffer or console is available.
//!
//! ## Usage
//!
//! Use the `kprint!` and `kprintln!` macros for kernel output:
//!
//! ```rust
//! kprintln!("Hello from KontsnorOS!");
//! kprintln!("[boot] Memory initialized: {} KiB free", free_kb);
//! ```

use spin::Mutex;
use uart_16550::SerialPort;
use lazy_static::lazy_static;

/// Standard COM1 I/O port address.
const COM1_PORT: u16 = 0x3F8;

lazy_static! {
    /// Global serial port instance, protected by a spinlock.
    ///
    /// The serial port is initialized once during early boot and then
    /// used by the `kprint!`/`kprintln!` macros throughout the kernel.
    pub static ref SERIAL1: Mutex<SerialPort> = {
        // SAFETY: COM1 port address 0x3F8 is the standard x86 serial port.
        // We are the only code accessing this port during initialization.
        let mut serial_port = unsafe { SerialPort::new(COM1_PORT) };
        serial_port.init();
        Mutex::new(serial_port)
    };
}

/// Initialize the serial port for early output.
///
/// This must be called before any use of `kprint!` or `kprintln!`.
/// The lazy_static initialization happens on first access, but calling
/// this function explicitly ensures it happens at the right time.
pub fn init() {
    // Force lazy_static initialization by accessing the serial port
    let _ = SERIAL1.lock();
}

/// Try to read one byte from the serial receive buffer (non-blocking).
///
/// Returns `Some(byte)` if the UART has data ready, `None` if the receive buffer
/// is empty. Used by `/dev/stdin` so user-space reads work in QEMU `-serial stdio`.
pub fn try_read_byte() -> Option<u8> {
    use x86_64::instructions::port::Port;
    // Line Status Register (LSR) is at base + 5. Bit 0 = Data Ready.
    let mut lsr: Port<u8> = Port::new(COM1_PORT + 5);
    let mut data: Port<u8> = Port::new(COM1_PORT);
    // SAFETY: accessing standard COM1 I/O ports.
    let status = unsafe { lsr.read() };
    if status & 0x01 != 0 {
        Some(unsafe { data.read() })
    } else {
        None
    }
}

///
/// Used by TTY devices to output user-space write() data to the console.
pub fn write_byte(byte: u8) {
    use x86_64::instructions::interrupts;
    use core::fmt::Write;
    interrupts::without_interrupts(|| {
        let _ = SERIAL1.lock().write_fmt(format_args!("{}", byte as char));
    });
}

/// Internal print function — writes to the serial port.
#[doc(hidden)]
pub fn _print(args: ::core::fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    // Disable interrupts while writing to prevent deadlock
    // (an interrupt handler might try to print while we hold the lock)
    interrupts::without_interrupts(|| {
        SERIAL1
            .lock()
            .write_fmt(args)
            .expect("Printing to serial failed");
    });
}

/// Print to the kernel serial console.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => ($crate::arch::x86_64::serial::_print(format_args!($($arg)*)));
}

/// Print to the kernel serial console, with a newline.
#[macro_export]
macro_rules! kprintln {
    () => ($crate::kprint!("\n"));
    ($($arg:tt)*) => ($crate::kprint!("{}\n", format_args!($($arg)*)));
}
