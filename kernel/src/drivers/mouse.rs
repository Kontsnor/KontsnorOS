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

//! PS/2 mouse driver — 8042 auxiliary port driver and event queues.
//!
//! Handles IRQ 12 PS/2 mouse interrupts, maintains packet synchronization,
//! and feeds input event queues for `/dev/input/mice` and `/dev/input/event0`.

use crate::kprintln;
use crate::sync::spinlock::TicketLock;
use crate::sync::wait_queue::WaitQueue;
use core::sync::atomic::{AtomicU8, Ordering};
use x86_64::instructions::port::Port;

/// Linux `struct input_event` (24 bytes on 64-bit systems).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InputEvent {
    pub time_sec: i64,
    pub time_usec: i64,
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;

pub const SYN_REPORT: u16 = 0x00;

pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;

pub const BTN_LEFT: u16 = 0x110;   // 272
pub const BTN_RIGHT: u16 = 0x111;  // 273
pub const BTN_MIDDLE: u16 = 0x112; // 274

/// Capacity of mouse ring buffers.
const MICE_BUFFER_CAPACITY: usize = 1024;
const EVENT_BUFFER_CAPACITY: usize = 256;

/// Ring buffer for raw 3-byte packets (`/dev/input/mice`).
struct MiceBuffer {
    data: [u8; MICE_BUFFER_CAPACITY],
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

impl MiceBuffer {
    const fn new() -> Self {
        Self {
            data: [0u8; MICE_BUFFER_CAPACITY],
            read_pos: 0,
            write_pos: 0,
            len: 0,
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.len < MICE_BUFFER_CAPACITY {
            self.data[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) % MICE_BUFFER_CAPACITY;
            self.len += 1;
        }
    }

    fn pop_byte(&mut self) -> Option<u8> {
        if self.len == 0 {
            None
        } else {
            let b = self.data[self.read_pos];
            self.read_pos = (self.read_pos + 1) % MICE_BUFFER_CAPACITY;
            self.len -= 1;
            Some(b)
        }
    }

    fn has_data(&self) -> bool {
        self.len > 0
    }
}

/// Ring buffer for Linux `InputEvent` structs (`/dev/input/event0`).
struct EventBuffer {
    events: [InputEvent; EVENT_BUFFER_CAPACITY],
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

impl EventBuffer {
    const fn new() -> Self {
        Self {
            events: [InputEvent {
                time_sec: 0,
                time_usec: 0,
                type_: 0,
                code: 0,
                value: 0,
            }; EVENT_BUFFER_CAPACITY],
            read_pos: 0,
            write_pos: 0,
            len: 0,
        }
    }

    fn push_event(&mut self, ev: InputEvent) {
        if self.len < EVENT_BUFFER_CAPACITY {
            self.events[self.write_pos] = ev;
            self.write_pos = (self.write_pos + 1) % EVENT_BUFFER_CAPACITY;
            self.len += 1;
        }
    }

    fn pop_event(&mut self) -> Option<InputEvent> {
        if self.len == 0 {
            None
        } else {
            let ev = self.events[self.read_pos];
            self.read_pos = (self.read_pos + 1) % EVENT_BUFFER_CAPACITY;
            self.len -= 1;
            Some(ev)
        }
    }

    fn has_data(&self) -> bool {
        self.len > 0
    }
}

static MICE_BUFFER: TicketLock<MiceBuffer> = TicketLock::new(MiceBuffer::new());
static EVENT_BUFFER: TicketLock<EventBuffer> = TicketLock::new(EventBuffer::new());

/// Global wait queue for mouse events (`/dev/input/mice` & `/dev/input/event0`).
pub static MOUSE_WAIT_QUEUE: WaitQueue = WaitQueue::new();

/// State machine for 3-byte PS/2 packet collection.
static PACKET_STATE: AtomicU8 = AtomicU8::new(0);
static PACKET_BYTE0: AtomicU8 = AtomicU8::new(0);
static PACKET_BYTE1: AtomicU8 = AtomicU8::new(0);

/// Previous button states (bit 0 = left, bit 1 = right, bit 2 = middle).
static PREV_BUTTONS: AtomicU8 = AtomicU8::new(0);

/// Try reading raw bytes for `/dev/input/mice`.
pub fn try_read_mice_bytes(buf: &mut [u8]) -> usize {
    let mut mice = MICE_BUFFER.lock();
    let mut count = 0;
    while count < buf.len() {
        if let Some(b) = mice.pop_byte() {
            buf[count] = b;
            count += 1;
        } else {
            break;
        }
    }
    count
}

/// Check if `/dev/input/mice` has data available.
pub fn mice_has_data() -> bool {
    MICE_BUFFER.lock().has_data()
}

/// Try reading `InputEvent` structs for `/dev/input/event0`.
pub fn try_read_events(buf: &mut [InputEvent]) -> usize {
    let mut events = EVENT_BUFFER.lock();
    let mut count = 0;
    while count < buf.len() {
        if let Some(ev) = events.pop_event() {
            buf[count] = ev;
            count += 1;
        } else {
            break;
        }
    }
    count
}

/// Check if `/dev/input/event0` has data available.
pub fn event0_has_data() -> bool {
    EVENT_BUFFER.lock().has_data()
}

// ── 8042 PS/2 Helper Functions ──────────────────────────────────────────

fn wait_input_empty() {
    let mut status_port = Port::<u8>::new(0x64);
    for _ in 0..100_000 {
        // SAFETY: Reading status port 0x64 is safe.
        let status = unsafe { status_port.read() };
        if (status & 0x02) == 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

fn wait_output_full() {
    let mut status_port = Port::<u8>::new(0x64);
    for _ in 0..100_000 {
        // SAFETY: Reading status port 0x64 is safe.
        let status = unsafe { status_port.read() };
        if (status & 0x01) != 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

fn mouse_write(cmd: u8) {
    let mut cmd_port = Port::<u8>::new(0x64);
    let mut data_port = Port::<u8>::new(0x60);

    wait_input_empty();
    // SAFETY: Command 0xD4 tells 8042 the next byte goes to second PS/2 port.
    unsafe { cmd_port.write(0xD4) };

    wait_input_empty();
    // SAFETY: Write command byte to data port 0x60.
    unsafe { data_port.write(cmd) };
}

fn mouse_read() -> u8 {
    let mut data_port = Port::<u8>::new(0x60);
    wait_output_full();
    // SAFETY: Read response from data port 0x60.
    unsafe { data_port.read() }
}

/// Helper to get current timestamp as (sec, usec).
fn current_timeval() -> (i64, i64) {
    let boot_sec = crate::syscall::process::info::boot_realtime_sec();
    let ticks = crate::arch::x86_64::interrupts::timer_ticks();
    let monotonic_us = ticks * 10_000; // each tick is 10ms = 10,000us
    let total_sec = boot_sec as i64 + (monotonic_us / 1_000_000) as i64;
    let total_usec = (monotonic_us % 1_000_000) as i64;
    (total_sec, total_usec)
}

/// Push a single raw byte received from IRQ 12 into packet assembly.
pub fn push_mouse_byte(byte: u8) {
    let state = PACKET_STATE.load(Ordering::Relaxed);
    match state {
        0 => {
            // Byte 0 must have bit 3 set to 1 for PS/2 alignment
            if (byte & 0x08) != 0 {
                PACKET_BYTE0.store(byte, Ordering::Relaxed);
                PACKET_STATE.store(1, Ordering::Relaxed);
            }
        }
        1 => {
            PACKET_BYTE1.store(byte, Ordering::Relaxed);
            PACKET_STATE.store(2, Ordering::Relaxed);
        }
        2 => {
            let b0 = PACKET_BYTE0.load(Ordering::Relaxed);
            let b1 = PACKET_BYTE1.load(Ordering::Relaxed);
            let b2 = byte;
            PACKET_STATE.store(0, Ordering::Relaxed);

            // Complete 3-byte packet received!
            process_packet(b0, b1, b2);
        }
        _ => {
            PACKET_STATE.store(0, Ordering::Relaxed);
        }
    }
}

/// Process a complete 3-byte PS/2 mouse packet.
fn process_packet(b0: u8, b1: u8, b2: u8) {
    // 1. Push raw 3-byte packet to MICE_BUFFER
    {
        let mut mice = MICE_BUFFER.lock();
        mice.push_byte(b0);
        mice.push_byte(b1);
        mice.push_byte(b2);
    }

    // 2. Parse movement and buttons
    let left = (b0 & 0x01) != 0;
    let right = (b0 & 0x02) != 0;
    let middle = (b0 & 0x04) != 0;

    let dx_sign = (b0 & 0x10) != 0;
    let dy_sign = (b0 & 0x20) != 0;

    let dx = if dx_sign {
        b1 as i8 as i32
    } else {
        b1 as i32
    };

    let dy = if dy_sign {
        b2 as i8 as i32
    } else {
        b2 as i32
    };

    // 3. Generate Linux input events for EVENT_BUFFER
    let (time_sec, time_usec) = current_timeval();
    let mut ev_count = 0;

    {
        let mut ev_buf = EVENT_BUFFER.lock();

        // Relative movement events
        if dx != 0 {
            ev_buf.push_event(InputEvent {
                time_sec,
                time_usec,
                type_: EV_REL,
                code: REL_X,
                value: dx,
            });
            ev_count += 1;
        }

        if dy != 0 {
            // PS/2 dy positive is UP; Linux evdev REL_Y positive is DOWN
            ev_buf.push_event(InputEvent {
                time_sec,
                time_usec,
                type_: EV_REL,
                code: REL_Y,
                value: -dy,
            });
            ev_count += 1;
        }

        // Button events
        let new_buttons = (left as u8) | ((right as u8) << 1) | ((middle as u8) << 2);
        let prev_buttons = PREV_BUTTONS.swap(new_buttons, Ordering::Relaxed);

        let prev_left = (prev_buttons & 0x01) != 0;
        let prev_right = (prev_buttons & 0x02) != 0;
        let prev_middle = (prev_buttons & 0x04) != 0;

        if left != prev_left {
            ev_buf.push_event(InputEvent {
                time_sec,
                time_usec,
                type_: EV_KEY,
                code: BTN_LEFT,
                value: if left { 1 } else { 0 },
            });
            ev_count += 1;
        }

        if right != prev_right {
            ev_buf.push_event(InputEvent {
                time_sec,
                time_usec,
                type_: EV_KEY,
                code: BTN_RIGHT,
                value: if right { 1 } else { 0 },
            });
            ev_count += 1;
        }

        if middle != prev_middle {
            ev_buf.push_event(InputEvent {
                time_sec,
                time_usec,
                type_: EV_KEY,
                code: BTN_MIDDLE,
                value: if middle { 1 } else { 0 },
            });
            ev_count += 1;
        }

        // Emit SYN_REPORT if any events were generated
        if ev_count > 0 {
            ev_buf.push_event(InputEvent {
                time_sec,
                time_usec,
                type_: EV_SYN,
                code: SYN_REPORT,
                value: 0,
            });
        }
    }

    // 4. Wake up tasks waiting on mouse input
    MOUSE_WAIT_QUEUE.wake_all();
}

/// Initialize the PS/2 mouse hardware and driver.
pub fn init() {
    kprintln!("[mouse] Initializing PS/2 mouse...");

    // Enable second PS/2 port (command 0xA8)
    wait_input_empty();
    let mut cmd_port = Port::<u8>::new(0x64);
    let mut data_port = Port::<u8>::new(0x60);
    // SAFETY: Command 0xA8 enables 2nd PS/2 port.
    unsafe { cmd_port.write(0xA8) };

    // Read Controller Configuration Byte (command 0x20)
    wait_input_empty();
    // SAFETY: Command 0x20 reads config byte.
    unsafe { cmd_port.write(0x20) };
    let mut config = mouse_read();

    // Enable IRQ 12 mouse interrupt (bit 1) and enable clock line (clear bit 5)
    config |= 0x02;
    config &= !0x20;

    wait_input_empty();
    // SAFETY: Command 0x60 writes config byte.
    unsafe { cmd_port.write(0x60) };
    wait_input_empty();
    // SAFETY: Write updated config byte to port 0x60.
    unsafe { data_port.write(config) };

    // Send Reset command (0xFF) to mouse
    mouse_write(0xFF);
    let ack = mouse_read();   // 0xFA
    let test = mouse_read();  // 0xAA
    let id = mouse_read();    // 0x00
    kprintln!("[mouse] Reset response: ACK={:#x}, test={:#x}, id={:#x}", ack, test, id);

    // Send Enable Data Reporting command (0xF4)
    mouse_write(0xF4);
    let ack2 = mouse_read(); // 0xFA
    kprintln!("[mouse] Enable Data Reporting ACK={:#x}", ack2);

    kprintln!("[mouse] PS/2 mouse initialized successfully.");
}
