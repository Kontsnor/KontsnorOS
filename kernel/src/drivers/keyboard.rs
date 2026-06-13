//! PS/2 keyboard driver — scancode-to-ASCII and ring buffer.
//!
//! This module handles the physical keyboard IRQ, translates PS/2 Set 1
//! scan codes into ASCII characters, and stores them in a global lock-free
//! ring buffer that `/dev/stdin` and `/dev/tty` read from.

use crate::kprintln;
use crate::sync::spinlock::TicketLock;

/// Size of the keyboard input ring buffer (4 KiB, power-of-2 for fast masking).
const BUFFER_CAPACITY: usize = 4096;

/// A fixed-capacity ring buffer for storing ASCII characters from the keyboard.
struct RingBuffer {
    data: [u8; BUFFER_CAPACITY],
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

impl RingBuffer {
    const fn new() -> Self {
        Self {
            data: [0u8; BUFFER_CAPACITY],
            read_pos: 0,
            write_pos: 0,
            len: 0,
        }
    }

    /// Push one byte. Silently drops if full.
    fn push(&mut self, byte: u8) {
        if self.len < BUFFER_CAPACITY {
            self.data[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) & (BUFFER_CAPACITY - 1);
            self.len += 1;
        }
    }

    /// Pop one byte, returns `None` if empty.
    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.data[self.read_pos];
        self.read_pos = (self.read_pos + 1) & (BUFFER_CAPACITY - 1);
        self.len -= 1;
        Some(byte)
    }

    /// Pop the last pushed byte (for backspace support).
    fn pop_back(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        self.write_pos = (self.write_pos.wrapping_sub(1)) & (BUFFER_CAPACITY - 1);
        let byte = self.data[self.write_pos];
        self.len -= 1;
        Some(byte)
    }

    /// Returns true if there is at least one byte available.
    fn has_data(&self) -> bool {
        self.len > 0
    }
}

/// Global keyboard input ring buffer.
static KEYBOARD_BUFFER: TicketLock<RingBuffer> = TicketLock::new(RingBuffer::new());

/// Global stdin wait queue.
pub static STDIN_WAIT_QUEUE: crate::sync::wait_queue::WaitQueue =
    crate::sync::wait_queue::WaitQueue::new();

/// Whether the Shift key is currently pressed.
static SHIFT_HELD: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// ── PS/2 Set 1 US-QWERTY Scancode Table ──────────────────────────────────
//
// Index = scancode byte (make code). Break codes are scancode | 0x80.
// 0x00 = no character (modifier / unmapped key).

const SCANCODE_TABLE_NORMAL: [u8; 128] = [
    0, 0x1b, b'1', b'2', b'3', b'4', b'5', b'6', // 0x00–0x07
    b'7', b'8', b'9', b'0', b'-', b'=', b'\x08', b'\t', // 0x08–0x0F  (BS, TAB)
    b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', // 0x10–0x17
    b'o', b'p', b'[', b']', b'\n', 0, b'a', b's', // 0x18–0x1F  (Enter, LCtrl, ...)
    b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', // 0x20–0x27
    b'\'', b'`', 0, b'\\', b'z', b'x', b'c', b'v', // 0x28–0x2F  (LShift)
    b'b', b'n', b'm', b',', b'.', b'/', 0, b'*', // 0x30–0x37  (RShift)
    0, b' ', 0, 0, 0, 0, 0, 0, // 0x38–0x3F  (LAlt, Space, ...)
    0, 0, 0, 0, 0, 0, 0, b'7', // 0x40–0x47  (F-keys, KP7)
    b'8', b'9', b'-', b'4', b'5', b'6', b'+', b'1', // 0x48–0x4F
    b'2', b'3', b'0', b'.', 0, 0, 0, 0, // 0x50–0x57
    0, 0, 0, 0, 0, 0, 0, 0, // 0x58–0x5F
    0, 0, 0, 0, 0, 0, 0, 0, // 0x60–0x67
    0, 0, 0, 0, 0, 0, 0, 0, // 0x68–0x6F
    0, 0, 0, 0, 0, 0, 0, 0, // 0x70–0x77
    0, 0, 0, 0, 0, 0, 0, 0, // 0x78–0x7F
];

const SCANCODE_TABLE_SHIFT: [u8; 128] = [
    0, 0x1b, b'!', b'@', b'#', b'$', b'%', b'^', // 0x00–0x07
    b'&', b'*', b'(', b')', b'_', b'+', b'\x08', b'\t', // 0x08–0x0F
    b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I', // 0x10–0x17
    b'O', b'P', b'{', b'}', b'\n', 0, b'A', b'S', // 0x18–0x1F
    b'D', b'F', b'G', b'H', b'J', b'K', b'L', b':', // 0x20–0x27
    b'"', b'~', 0, b'|', b'Z', b'X', b'C', b'V', // 0x28–0x2F
    b'B', b'N', b'M', b'<', b'>', b'?', 0, b'*', // 0x30–0x37
    0, b' ', 0, 0, 0, 0, 0, 0, // 0x38–0x3F
    0, 0, 0, 0, 0, 0, 0, b'7', // 0x40–0x47
    b'8', b'9', b'-', b'4', b'5', b'6', b'+', b'1', // 0x48–0x4F
    b'2', b'3', b'0', b'.', 0, 0, 0, 0, // 0x50–0x57
    0, 0, 0, 0, 0, 0, 0, 0, // 0x58–0x5F
    0, 0, 0, 0, 0, 0, 0, 0, // 0x60–0x67
    0, 0, 0, 0, 0, 0, 0, 0, // 0x68–0x6F
    0, 0, 0, 0, 0, 0, 0, 0, // 0x70–0x77
    0, 0, 0, 0, 0, 0, 0, 0, // 0x78–0x7F
];

// PS/2 Set 1 make-codes for modifier keys
const SC_LEFT_SHIFT: u8 = 0x2A;
const SC_RIGHT_SHIFT: u8 = 0x36;
const SC_BREAK_FLAG: u8 = 0x80; // High bit set = break (key-up) code

/// Translate a raw PS/2 Set 1 scancode into an ASCII byte.
/// Returns `None` for non-character keys (modifiers, F-keys, unmapped).
fn scan_to_ascii(scancode: u8) -> Option<u8> {
    // Detect break codes (key-up)
    if scancode & SC_BREAK_FLAG != 0 {
        let make = scancode & !SC_BREAK_FLAG;
        if make == SC_LEFT_SHIFT || make == SC_RIGHT_SHIFT {
            SHIFT_HELD.store(false, core::sync::atomic::Ordering::Relaxed);
        }
        return None; // Break codes do not produce characters
    }

    // Track Shift state
    if scancode == SC_LEFT_SHIFT || scancode == SC_RIGHT_SHIFT {
        SHIFT_HELD.store(true, core::sync::atomic::Ordering::Relaxed);
        return None;
    }

    let idx = scancode as usize;
    if idx >= 128 {
        return None;
    }

    let table = if SHIFT_HELD.load(core::sync::atomic::Ordering::Relaxed) {
        &SCANCODE_TABLE_SHIFT
    } else {
        &SCANCODE_TABLE_NORMAL
    };

    let ch = table[idx];
    if ch == 0 {
        None
    } else {
        Some(ch)
    }
}

/// Called from the keyboard interrupt handler.
///
/// Translates the scancode to ASCII and pushes it into the global ring buffer.
pub fn push_scancode(scancode: u8) {
    if let Some(ascii) = scan_to_ascii(scancode) {
        KEYBOARD_BUFFER.lock().push(ascii);
        STDIN_WAIT_QUEUE.wake_all();
    }
}

/// Directly push a character into the keyboard ring buffer.
/// Used to route input from alternative devices like the serial port.
pub fn push_char(byte: u8) {
    KEYBOARD_BUFFER.lock().push(byte);
    STDIN_WAIT_QUEUE.wake_all();
}

/// Non-blocking read of one ASCII character from the keyboard buffer.
///
/// Returns `None` if the buffer is currently empty.
pub fn try_read_char() -> Option<u8> {
    KEYBOARD_BUFFER.lock().pop()
}

/// Pop the last character from the buffer (for backspace support).
pub fn try_pop_back() -> Option<u8> {
    KEYBOARD_BUFFER.lock().pop_back()
}

/// Returns true if there is a newline character in the keyboard buffer.
pub fn has_newline() -> bool {
    let buf = KEYBOARD_BUFFER.lock();
    if buf.len == 0 {
        return false;
    }
    for i in 0..buf.len {
        let pos = (buf.read_pos + i) & (BUFFER_CAPACITY - 1);
        if buf.data[pos] == b'\n' {
            return true;
        }
    }
    false
}

/// Returns true if there is at least one character ready in the buffer.
pub fn has_input() -> bool {
    KEYBOARD_BUFFER.lock().has_data()
}

/// Initialize the keyboard driver.
pub fn init() {
    kprintln!("[keyboard] PS/2 keyboard driver initialized.");
}
