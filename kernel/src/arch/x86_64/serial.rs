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

//! Serial port (UART 16550) driver for early boot logging.
//!
//! This module provides a global serial port interface for kernel logging.
//! The serial port is the primary output channel during boot before any
//! framebuffer or console is available.

use core::cell::UnsafeCell;
use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};
use lazy_static::lazy_static;
use uart_16550::SerialPort;

/// Standard COM1 I/O port address.
const COM1_PORT: u16 = 0x3F8;

/// A re-entrant, interrupt-safe spinlock protecting the serial port.
pub struct ReentrantSerialLock {
    holding_cpu: AtomicU32,
    recursion: AtomicU32,
    port: UnsafeCell<SerialPort>,
}

unsafe impl Send for ReentrantSerialLock {}
unsafe impl Sync for ReentrantSerialLock {}

impl ReentrantSerialLock {
    pub const fn new(port: SerialPort) -> Self {
        Self {
            holding_cpu: AtomicU32::new(0xFFFF_FFFF),
            recursion: AtomicU32::new(0),
            port: UnsafeCell::new(port),
        }
    }

    /// Execute a closure with exclusive access to the serial port.
    ///
    /// Supports re-entrant acquisition from the same CPU core (e.g. within
    /// page fault handlers or exception dumps) without deadlocking.
    pub fn with_lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut SerialPort) -> R,
    {
        let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();
        if interrupts_enabled {
            x86_64::instructions::interrupts::disable();
        }

        let cpu_id = crate::arch::x86_64::smp::current_lapic_id() as u32;

        // Check for recursive re-entrancy on the same CPU core
        if self.holding_cpu.load(Ordering::Relaxed) == cpu_id {
            self.recursion.fetch_add(1, Ordering::Relaxed);
            let port = unsafe { &mut *self.port.get() };
            let res = f(port);
            self.recursion.fetch_sub(1, Ordering::Relaxed);
            if interrupts_enabled {
                x86_64::instructions::interrupts::enable();
            }
            return res;
        }

        // Spin until we acquire the lock
        while self
            .holding_cpu
            .compare_exchange_weak(0xFFFF_FFFF, cpu_id, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            if crate::arch::x86_64::smp::has_pending_tlb_shootdown() {
                x86_64::instructions::tlb::flush_all();
                crate::arch::x86_64::smp::tlb_shootdown_ack();
            }
            core::hint::spin_loop();
        }

        self.recursion.store(1, Ordering::Relaxed);
        let port = unsafe { &mut *self.port.get() };
        let res = f(port);
        self.recursion.store(0, Ordering::Relaxed);
        self.holding_cpu.store(0xFFFF_FFFF, Ordering::Release);

        if interrupts_enabled {
            x86_64::instructions::interrupts::enable();
        }
        res
    }
}

lazy_static! {
    /// Global serial port instance, protected by a re-entrant spinlock.
    pub static ref SERIAL1: ReentrantSerialLock = {
        let mut serial_port = unsafe { SerialPort::new(COM1_PORT) };
        serial_port.init();
        ReentrantSerialLock::new(serial_port)
    };
}

/// Initialize the serial port for early output.
pub fn init() {
    SERIAL1.with_lock(|_| {});
}

/// Try to read one byte from the serial receive buffer (non-blocking).
pub fn try_read_byte() -> Option<u8> {
    use x86_64::instructions::port::Port;
    let mut lsr: Port<u8> = Port::new(COM1_PORT + 5);
    let mut data: Port<u8> = Port::new(COM1_PORT);
    let status = unsafe { lsr.read() };
    if status & 0x01 != 0 {
        Some(unsafe { data.read() })
    } else {
        None
    }
}

/// Output a single byte to serial and graphics console.
pub fn write_byte(byte: u8) {
    SERIAL1.with_lock(|port| {
        let _ = port.write_fmt(format_args!("{}", byte as char));
    });

    if !crate::drivers::gpu::bochs::DISABLE_CONSOLE_MIRROR
        .load(core::sync::atomic::Ordering::Relaxed)
    {
        if let Some(ref mut console) = *crate::drivers::gpu::bochs::GRAPHICS_CONSOLE.lock() {
            console.write_char(byte);
            if byte == b'\n' {
                console.gpu.blit();
            }
        }
    }
}

/// Internal print function — writes to the serial port and mirrors to graphics console.
#[doc(hidden)]
pub fn _print(args: ::core::fmt::Arguments) {
    SERIAL1.with_lock(|port| {
        port.write_fmt(args).expect("Printing to serial failed");
    });

    if !crate::drivers::gpu::bochs::DISABLE_CONSOLE_MIRROR
        .load(core::sync::atomic::Ordering::Relaxed)
    {
        if let Some(ref mut console) = *crate::drivers::gpu::bochs::GRAPHICS_CONSOLE.lock() {
            let _ = console.write_fmt(args);
        }
    }
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
