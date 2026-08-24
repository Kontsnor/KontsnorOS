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

//! TTY/Console character devices — `/dev/tty`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr`.
//!
//! These devices bridge the physical keyboard ring buffer and the serial console
//! with the VFS `InodeOps` trait so that user-space programs can use the standard
//! Unix file-I/O API to perform terminal I/O.

use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use crate::fs::inode::{DirEntry, FileType, Inode, InodeOps};

/// POSIX termios structure.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 19],
}

/// POSIX winsize structure for TIOCGWINSZ.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// Global active TTY termios settings.
/// Default: ICANON (0x02) | ECHO (0x08) | ISIG (0x01)
pub static TTY_TERMIOS: Mutex<Termios> = Mutex::new(Termios {
    c_iflag: 0,
    c_oflag: 0,
    c_cflag: 0,
    c_lflag: 0x00000002 | 0x00000008 | 0x00000001,
    c_line: 0,
    c_cc: [0; 19],
});

/// Global active TTY foreground process group ID.
pub static TTY_FOREGROUND_PGID: Mutex<u64> = Mutex::new(1);

// ── /dev/stdin ────────────────────────────────────────────────────────────────

/// Global lock to serialize reads from `/dev/stdin`.
static STDIN_LOCK: Mutex<()> = Mutex::new(());

/// `/dev/stdin` character device.
pub struct DevStdin {
    pub inode: Inode,
}

impl InodeOps for DevStdin {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    /// Read characters based on ICANON, ECHO, and ISIG termios flags.
    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Wait cooperatively for input
        loop {
            let mut got_input = false;
            let mut raw_char = None;
            let mut interrupted = None;

            {
                let _lock = STDIN_LOCK.lock();

                let termios = TTY_TERMIOS.lock();
                let icanon = (termios.c_lflag & 0x00000002) != 0;
                let echo = (termios.c_lflag & 0x00000008) != 0;
                let isig = (termios.c_lflag & 0x00000001) != 0;
                drop(termios);

                // Check if any character is available on serial
                if let Some(mut byte) = crate::arch::x86_64::serial::try_read_byte() {
                    if byte == b'\r' {
                        byte = b'\n';
                    }

                    // If ISIG is enabled and Ctrl+C is typed, deliver SIGINT immediately!
                    if isig && byte == 0x03 {
                        let pgid = *TTY_FOREGROUND_PGID.lock();
                        crate::fs::pty::deliver_signal_to_pgrp(pgid, 2); // SIGINT = 2
                        interrupted = Some(-4); // EINTR
                    }

                    if interrupted.is_none() {
                        if icanon {
                            // Backspace/delete cooked character erasing
                            if byte == 0x7F || byte == b'\x08' {
                                if let Some(popped) = crate::drivers::keyboard::try_pop_back() {
                                    if popped != b'\n' {
                                        if echo {
                                            crate::arch::x86_64::serial::write_byte(b'\x08');
                                            crate::arch::x86_64::serial::write_byte(b' ');
                                            crate::arch::x86_64::serial::write_byte(b'\x08');
                                        }
                                    } else {
                                        crate::drivers::keyboard::push_char(popped);
                                    }
                                }
                            } else {
                                if echo {
                                    crate::arch::x86_64::serial::write_byte(byte);
                                }
                                crate::drivers::keyboard::push_char(byte);
                            }
                        } else {
                            // Raw mode bypasses cooked edits
                            if echo {
                                crate::arch::x86_64::serial::write_byte(byte);
                            }
                            crate::drivers::keyboard::push_char(byte);
                        }
                    }
                }

                if let Some(err) = interrupted {
                    return Err(err);
                }

                // Verify if we can return
                if icanon {
                    if crate::drivers::keyboard::has_newline() {
                        got_input = true;
                    }
                } else {
                    // Raw mode: check if buffer has characters and return immediately
                    if let Some(ch) = crate::drivers::keyboard::try_read_char() {
                        raw_char = Some(ch);
                    }
                }
            }

            if let Some(ch) = raw_char {
                buf[0] = ch;
                return Ok(1);
            }

            if got_input {
                break;
            }

            // Yield to avoid hard locking
            crate::process::scheduler::yield_now();

            // Cooperatively exit on signals
            if let Some(current_pid) = crate::process::scheduler::current_pid() {
                if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
                    let task = task_arc.lock();
                    let unblocked = task.pending_signals & !task.blocked_signals;
                    if unblocked != 0 {
                        return Err(-4); // EINTR
                    }
                }
            }
        }

        // Drain canonical buffer to user
        let mut count = 0;
        {
            let _lock = STDIN_LOCK.lock();
            while count < buf.len() {
                match crate::drivers::keyboard::try_read_char() {
                    Some(ch) => {
                        buf[count] = ch;
                        count += 1;
                        if ch == b'\n' {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }

        Ok(count)
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        match request {
            0x5401 => {
                // TCGETS
                if !crate::syscall::fs::validate_user_ptr(
                    arg as *const u8,
                    core::mem::size_of::<Termios>(),
                ) {
                    return Err(-14); // EFAULT
                }
                let termios = TTY_TERMIOS.lock();
                unsafe {
                    core::ptr::write(arg as *mut Termios, *termios);
                }
                Ok(0)
            }
            0x5402 | 0x5403 | 0x5404 => {
                // TCSETS, TCSETSW, TCSETSF
                if !crate::syscall::fs::validate_user_ptr(
                    arg as *const u8,
                    core::mem::size_of::<Termios>(),
                ) {
                    return Err(-14); // EFAULT
                }
                let mut termios = TTY_TERMIOS.lock();
                unsafe {
                    *termios = core::ptr::read(arg as *const Termios);
                }
                Ok(0)
            }
            0x5413 => {
                // TIOCGWINSZ
                if !crate::syscall::fs::validate_user_ptr(
                    arg as *const u8,
                    core::mem::size_of::<Winsize>(),
                ) {
                    return Err(-14); // EFAULT
                }
                let ws = Winsize {
                    ws_row: 24,
                    ws_col: 80,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                unsafe {
                    core::ptr::write(arg as *mut Winsize, ws);
                }
                Ok(0)
            }
            0x540F => {
                // TIOCGPGRP
                if !crate::syscall::fs::validate_user_ptr(
                    arg as *mut i32 as *const u8,
                    core::mem::size_of::<i32>(),
                ) {
                    return Err(-14); // EFAULT
                }
                let pgid = *TTY_FOREGROUND_PGID.lock() as i32;
                unsafe {
                    core::ptr::write(arg as *mut i32, pgid);
                }
                Ok(0)
            }
            0x5410 => {
                // TIOCSPGRP
                if !crate::syscall::fs::validate_user_ptr(
                    arg as *const i32 as *const u8,
                    core::mem::size_of::<i32>(),
                ) {
                    return Err(-14); // EFAULT
                }
                let pgid = unsafe { core::ptr::read(arg as *const i32) } as u64;
                *TTY_FOREGROUND_PGID.lock() = pgid;
                Ok(0)
            }
            _ => Err(-22), // EINVAL
        }
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        let termios = TTY_TERMIOS.lock();
        let icanon = (termios.c_lflag & 0x00000002) != 0;
        drop(termios);

        let has_data = if icanon {
            crate::drivers::keyboard::has_newline()
        } else {
            crate::drivers::keyboard::has_input()
        };

        if (events & crate::fs::inode::POLLIN) != 0 && has_data {
            revents |= crate::fs::inode::POLLIN;
        }
        revents
    }
}

// ── /dev/stdout ───────────────────────────────────────────────────────────────

/// `/dev/stdout` character device.
pub struct DevStdout {
    pub inode: Inode,
}

impl InodeOps for DevStdout {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        for &byte in data {
            crate::arch::x86_64::serial::write_byte(byte);
        }
        Ok(data.len())
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        let stdin = DevStdin {
            inode: Inode::new(10, FileType::CharDevice),
        };
        stdin.ioctl(request, arg)
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        if (events & crate::fs::inode::POLLOUT) != 0 {
            revents |= crate::fs::inode::POLLOUT;
        }
        revents
    }
}

// ── /dev/stderr ───────────────────────────────────────────────────────────────

/// `/dev/stderr` character device.
pub struct DevStderr {
    pub inode: Inode,
}

impl InodeOps for DevStderr {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        for &byte in data {
            crate::arch::x86_64::serial::write_byte(byte);
        }
        Ok(data.len())
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        let stdin = DevStdin {
            inode: Inode::new(10, FileType::CharDevice),
        };
        stdin.ioctl(request, arg)
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        if (events & crate::fs::inode::POLLOUT) != 0 {
            revents |= crate::fs::inode::POLLOUT;
        }
        revents
    }
}

// ── /dev/tty ──────────────────────────────────────────────────────────────────

/// `/dev/tty` controlling terminal alias.
pub struct DevTty {
    pub inode: Inode,
}

impl InodeOps for DevTty {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let stdin = DevStdin {
            inode: Inode::new(10, FileType::CharDevice),
        };
        stdin.read(_offset, buf)
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        for &byte in data {
            crate::arch::x86_64::serial::write_byte(byte);
        }
        Ok(data.len())
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        let stdin = DevStdin {
            inode: Inode::new(10, FileType::CharDevice),
        };
        stdin.ioctl(request, arg)
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }

    fn poll(&self, events: u32) -> u32 {
        let stdin = DevStdin {
            inode: Inode::new(10, FileType::CharDevice),
        };
        stdin.poll(events) | (events & crate::fs::inode::POLLOUT)
    }
}

// ── Constructor helpers ───────────────────────────────────────────────────────

pub fn make_stdin() -> Arc<dyn InodeOps> {
    Arc::new(DevStdin {
        inode: Inode::new(10, FileType::CharDevice),
    })
}

pub fn make_stdout() -> Arc<dyn InodeOps> {
    Arc::new(DevStdout {
        inode: Inode::new(11, FileType::CharDevice),
    })
}

pub fn make_stderr() -> Arc<dyn InodeOps> {
    Arc::new(DevStderr {
        inode: Inode::new(12, FileType::CharDevice),
    })
}

pub fn make_tty() -> Arc<dyn InodeOps> {
    Arc::new(DevTty {
        inode: Inode::new(13, FileType::CharDevice),
    })
}
