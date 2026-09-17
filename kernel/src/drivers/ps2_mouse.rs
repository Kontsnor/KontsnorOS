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

//! PS/2 Auxiliary Mouse Driver.
//!
//! Handles PS/2 mouse initialization, IRQ 12 byte assembly into standard
//! 3-byte movement packets, and formats packets for userspace consumption
//! via `/dev/input/mice`.

use alloc::sync::Arc;
use x86_64::instructions::port::Port;

use crate::kprintln;
use crate::sync::spinlock::TicketLock;

/// Capacity of the mouse packet ring buffer (power of 2).
const MOUSE_BUFFER_CAPACITY: usize = 256;

/// A raw 3-byte PS/2 mouse packet: (flags, dx, dy).
#[derive(Debug, Clone, Copy)]
pub struct MousePacket {
    pub flags: u8,
    pub dx: u8,
    pub dy: u8,
}

struct MouseRingBuffer {
    data: [MousePacket; MOUSE_BUFFER_CAPACITY],
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

impl MouseRingBuffer {
    const fn new() -> Self {
        Self {
            data: [MousePacket {
                flags: 0,
                dx: 0,
                dy: 0,
            }; MOUSE_BUFFER_CAPACITY],
            read_pos: 0,
            write_pos: 0,
            len: 0,
        }
    }

    fn push(&mut self, pkt: MousePacket) {
        if self.len < MOUSE_BUFFER_CAPACITY {
            self.data[self.write_pos] = pkt;
            self.write_pos = (self.write_pos + 1) & (MOUSE_BUFFER_CAPACITY - 1);
            self.len += 1;
        }
    }

    fn pop(&mut self) -> Option<MousePacket> {
        if self.len == 0 {
            return None;
        }
        let pkt = self.data[self.read_pos];
        self.read_pos = (self.read_pos + 1) & (MOUSE_BUFFER_CAPACITY - 1);
        self.len -= 1;
        Some(pkt)
    }

    fn has_data(&self) -> bool {
        self.len > 0
    }
}

static MOUSE_BUFFER: TicketLock<MouseRingBuffer> = TicketLock::new(MouseRingBuffer::new());

/// State machine for accumulating incoming IRQ 12 bytes into a 3-byte packet.
struct MouseParser {
    packet_bytes: [u8; 3],
    index: usize,
}

static MOUSE_PARSER: TicketLock<MouseParser> = TicketLock::new(MouseParser {
    packet_bytes: [0; 3],
    index: 0,
});

pub static MOUSE_WAIT_QUEUE: crate::sync::wait_queue::WaitQueue =
    crate::sync::wait_queue::WaitQueue::new();

lazy_static::lazy_static! {
    pub static ref MOUSE_WAIT_QUEUE_ARC: Arc<crate::sync::wait_queue::WaitQueue> =
        Arc::new(crate::sync::wait_queue::WaitQueue::new());
}

/// Get the shared Arc wait queue for mouse events.
pub fn mouse_wait_queue() -> Arc<crate::sync::wait_queue::WaitQueue> {
    MOUSE_WAIT_QUEUE_ARC.clone()
}

/// Wait until the PS/2 controller input buffer is ready for a command/data write.
fn wait_input_ready() -> bool {
    let mut status = Port::<u8>::new(0x64);
    for _ in 0..10_000 {
        // SAFETY: Reading status from port 0x64 has no side effects.
        if unsafe { status.read() } & 0x02 == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Wait until the PS/2 controller output buffer has data to be read.
fn wait_output_ready() -> bool {
    let mut status = Port::<u8>::new(0x64);
    for _ in 0..10_000 {
        // SAFETY: Reading status from port 0x64 has no side effects.
        if unsafe { status.read() } & 0x01 != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Called from IRQ 12 mouse interrupt handler.
pub fn push_byte(byte: u8) {
    let mut parser = MOUSE_PARSER.lock();
    if parser.index == 0 {
        // PS/2 mouse packet byte 0 always has bit 3 set to 1.
        // If not set, stream is out of sync; discard and wait for a valid header byte.
        if byte & 0x08 == 0 {
            return;
        }
        parser.packet_bytes[0] = byte;
        parser.index = 1;
    } else if parser.index == 1 {
        parser.packet_bytes[1] = byte;
        parser.index = 2;
    } else {
        parser.packet_bytes[2] = byte;
        parser.index = 0;

        let pkt = MousePacket {
            flags: parser.packet_bytes[0],
            dx: parser.packet_bytes[1],
            dy: parser.packet_bytes[2],
        };

        MOUSE_BUFFER.lock().push(pkt);
        MOUSE_WAIT_QUEUE.wake_all();
        MOUSE_WAIT_QUEUE_ARC.wake_all();
    }
}

/// Read a packet from the mouse buffer.
/// Emits standard 5-byte ImPS/2 format if buffer is >= 5 bytes, or 3-byte format if >= 3 bytes.
pub fn read_mice_packet(buf: &mut [u8]) -> Option<usize> {
    if buf.len() < 3 {
        return None;
    }

    let pkt = MOUSE_BUFFER.lock().pop()?;

    if buf.len() >= 5 {
        buf[0] = pkt.flags;
        buf[1] = pkt.dx;
        buf[2] = pkt.dy;
        buf[3] = 0; // Z wheel delta
        buf[4] = 0; // extra buttons
        Some(5)
    } else {
        buf[0] = pkt.flags;
        buf[1] = pkt.dx;
        buf[2] = pkt.dy;
        Some(3)
    }
}

/// Check if mouse data is available.
pub fn has_data() -> bool {
    MOUSE_BUFFER.lock().has_data()
}

/// Initialize the PS/2 mouse hardware driver.
pub fn init() {
    let mut cmd_port = Port::<u8>::new(0x64);
    let mut data_port = Port::<u8>::new(0x60);

    // 1. Enable auxiliary PS/2 device (second PS/2 port)
    if !wait_input_ready() {
        kprintln!("[ps2_mouse] Warning: PS/2 controller not ready for command");
        return;
    }
    // SAFETY: Writing command 0xA8 enables the auxiliary PS/2 port.
    unsafe { cmd_port.write(0xA8) };

    // 2. Read PS/2 controller configuration byte
    if !wait_input_ready() {
        return;
    }
    // SAFETY: Writing command 0x20 requests the configuration byte from the controller.
    unsafe { cmd_port.write(0x20) };

    if !wait_output_ready() {
        return;
    }
    // SAFETY: Reading controller configuration byte from port 0x60.
    let mut config = unsafe { data_port.read() };

    // Set bit 1: Enable second port interrupt (IRQ 12)
    // Clear bit 5: Enable second port clock (0 = enabled)
    config |= 0x02;
    config &= !0x20;

    // 3. Write back updated configuration byte
    if !wait_input_ready() {
        return;
    }
    // SAFETY: Writing command 0x60 sets the controller configuration byte.
    unsafe { cmd_port.write(0x60) };

    if !wait_input_ready() {
        return;
    }
    // SAFETY: Writing configuration value to port 0x60.
    unsafe { data_port.write(config) };

    // 4. Enable data reporting from the mouse (command 0xF4 to mouse)
    if !wait_input_ready() {
        return;
    }
    // SAFETY: Writing command 0xD4 routes next byte to auxiliary device.
    unsafe { cmd_port.write(0xD4) };

    if !wait_input_ready() {
        return;
    }
    // SAFETY: Writing 0xF4 enables data streaming on the PS/2 mouse.
    unsafe { data_port.write(0xF4) };

    // Read ACK (0xFA) if available
    if wait_output_ready() {
        // SAFETY: Consuming ACK byte from port 0x60.
        let _ack = unsafe { data_port.read() };
    }

    kprintln!("[ps2_mouse] PS/2 mouse driver initialized.");
}
