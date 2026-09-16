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

pub const DEFAULT_C_CC: [u8; 19] = [
    3,   // 0: VINTR (Ctrl+C)
    28,  // 1: VQUIT (Ctrl+\)
    127, // 2: VERASE (Backspace)
    21,  // 3: VKILL (Ctrl+U)
    4,   // 4: VEOF (Ctrl+D)
    0,   // 5: VTIME
    1,   // 6: VMIN
    0,   // 7: VSWTC
    17,  // 8: VSTART (Ctrl+Q)
    19,  // 9: VSTOP (Ctrl+S)
    26,  // 10: VSUSP (Ctrl+Z)
    0,   // 11: VEOL
    18,  // 12: VREPRINT (Ctrl+R)
    23,  // 13: VDISCARD (Ctrl+O)
    23,  // 14: VWERASE (Ctrl+W)
    22,  // 15: VLNEXT (Ctrl+V)
    0,   // 16: VEOL2
    0,
    0,
];

/// Global active TTY termios settings.
/// Default: ICANON | ECHO | ISIG | IEXTEN | ECHOE | ECHOK
pub static TTY_TERMIOS: Mutex<Termios> = Mutex::new(Termios {
    c_iflag: 0x00000100, // ICRNL
    c_oflag: 0x00000005, // OPOST | ONLCR
    c_cflag: 0x000000bf,
    c_lflag: 0x00000002 | 0x00000008 | 0x00000001 | 0x00008000 | 0x00000010 | 0x00000020,
    c_line: 0,
    c_cc: DEFAULT_C_CC,
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

    fn wait_queue(&self) -> Option<alloc::sync::Arc<crate::sync::wait_queue::WaitQueue>> {
        Some(crate::drivers::keyboard::stdin_wait_queue())
    }

    /// Read characters based on ICANON, ECHO, and ISIG termios flags.
    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.is_empty() {
            return Ok(0);
        }

        let is_nonblocking = crate::fs::inode::is_inode_nonblocking(self);

        // Wait cooperatively for input
        loop {
            let mut got_input = false;
            let mut raw_char = None;
            let mut interrupted = None;

            let mut sig_to_deliver = None;

            {
                let _lock = STDIN_LOCK.lock();

                let termios = TTY_TERMIOS.lock();
                let icanon = (termios.c_lflag & 0x00000002) != 0;
                let echo = (termios.c_lflag & 0x00000008) != 0;
                let isig = (termios.c_lflag & 0x00000001) != 0;
                drop(termios);

                if !icanon && crate::drivers::keyboard::has_input() {
                    got_input = true;
                } else if icanon && crate::drivers::keyboard::has_newline() {
                    got_input = true;
                }

                // Check if any character is available on serial
                if let Some(mut byte) = crate::arch::x86_64::serial::try_read_byte() {
                    if byte == b'\r' {
                        byte = b'\n';
                    }

                    // Terminal signals when ISIG is enabled
                    if isig {
                        if byte == 0x03 {
                            // Ctrl+C -> SIGINT
                            if echo {
                                crate::arch::x86_64::serial::write_byte(b'^');
                                crate::arch::x86_64::serial::write_byte(b'C');
                                crate::arch::x86_64::serial::write_byte(b'\n');
                            }
                            let mut pgid = *TTY_FOREGROUND_PGID.lock();
                            if pgid == 0 || pgid == 1 {
                                pgid = crate::fs::pty::find_foreground_pgid();
                            }
                            sig_to_deliver = Some((pgid, 2));
                            interrupted = Some(-4); // EINTR
                        } else if byte == 0x1C {
                            // Ctrl+\ -> SIGQUIT
                            if echo {
                                crate::arch::x86_64::serial::write_byte(b'^');
                                crate::arch::x86_64::serial::write_byte(b'\\');
                                crate::arch::x86_64::serial::write_byte(b'\n');
                            }
                            let mut pgid = *TTY_FOREGROUND_PGID.lock();
                            if pgid == 0 || pgid == 1 {
                                pgid = crate::fs::pty::find_foreground_pgid();
                            }
                            sig_to_deliver = Some((pgid, 3));
                            interrupted = Some(-4);
                        } else if byte == 0x1A {
                            // Ctrl+Z -> SIGTSTP
                            if echo {
                                crate::arch::x86_64::serial::write_byte(b'^');
                                crate::arch::x86_64::serial::write_byte(b'Z');
                                crate::arch::x86_64::serial::write_byte(b'\n');
                            }
                            let mut pgid = *TTY_FOREGROUND_PGID.lock();
                            if pgid == 0 || pgid == 1 {
                                pgid = crate::fs::pty::find_foreground_pgid();
                            }
                            sig_to_deliver = Some((pgid, 20));
                            interrupted = Some(-4);
                        }
                    }

                    if interrupted.is_none() {
                        if icanon {
                            if byte == 0x04 {
                                // Ctrl+D (EOF in canonical mode)
                                if !crate::drivers::keyboard::has_input() {
                                    return Ok(0);
                                } else {
                                    got_input = true;
                                }
                            } else if byte == 0x15 {
                                // Ctrl+U (Line Kill)
                                while let Some(popped) = crate::drivers::keyboard::try_pop_back() {
                                    if echo && popped != b'\n' {
                                        crate::arch::x86_64::serial::write_byte(b'\x08');
                                        crate::arch::x86_64::serial::write_byte(b' ');
                                        crate::arch::x86_64::serial::write_byte(b'\x08');
                                    }
                                }
                            } else if byte == 0x17 {
                                // Ctrl+W (Word Erase)
                                while let Some(popped) = crate::drivers::keyboard::try_pop_back() {
                                    if echo && popped != b'\n' {
                                        crate::arch::x86_64::serial::write_byte(b'\x08');
                                        crate::arch::x86_64::serial::write_byte(b' ');
                                        crate::arch::x86_64::serial::write_byte(b'\x08');
                                    }
                                    if popped == b' ' || popped == b'\t' {
                                        continue;
                                    }
                                    break;
                                }
                            } else if byte == 0x7F || byte == b'\x08' {
                                // Backspace/delete cooked character erasing
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
            }

            if let Some((pgid, sig)) = sig_to_deliver {
                if pgid != 0 {
                    crate::fs::pty::deliver_signal_to_pgrp(pgid, sig);
                }
            }

            if let Some(err) = interrupted {
                return Err(err);
            }

            if let Some(ch) = raw_char {
                buf[0] = ch;
                return Ok(1);
            }

            if got_input {
                break;
            }

            if is_nonblocking {
                return Err(-11); // -EAGAIN
            }

            // Sleep cooperatively on wait queue
            crate::drivers::keyboard::stdin_wait_queue().wait();

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
                let mut pgid_lock = TTY_FOREGROUND_PGID.lock();
                if *pgid_lock == 0 {
                    if let Some(pid) = crate::process::scheduler::current_pid() {
                        if let Some(task) = crate::process::scheduler::get_task_arc(pid) {
                            *pgid_lock = task.lock().pgid;
                        }
                    }
                }
                let pgid = *pgid_lock as i32;
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
                if let Some(calling_pid) = crate::process::scheduler::current_pid() {
                    if let Some(calling_task) = crate::process::scheduler::get_task_arc(calling_pid) {
                        let calling_sid = calling_task.lock().sid;
                        if pgid != 0 {
                            let tasks = crate::process::scheduler::TASKS.read();
                            let valid = tasks.iter().any(|slot| {
                                if let Some(t_arc) = slot {
                                    if let Some(t) = t_arc.try_lock() {
                                        t.pgid == pgid && t.sid == calling_sid
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            });
                            if !valid {
                                return Err(-1); // EPERM
                            }
                        }
                    }
                }
                *TTY_FOREGROUND_PGID.lock() = pgid;
                Ok(0)
            }
            0x540E => {
                // TIOCSCTTY: Set controlling terminal
                if let Some(pid) = crate::process::scheduler::current_pid() {
                    if let Some(task) = crate::process::scheduler::get_task_arc(pid) {
                        *TTY_FOREGROUND_PGID.lock() = task.lock().pgid;
                    }
                }
                Ok(0)
            }
            _ => Err(-25), // ENOTTY
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
        if let Some(current_pid) = crate::process::scheduler::current_pid() {
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
                let task_pgid = task_arc.lock().pgid;
                let fg_pgid = *TTY_FOREGROUND_PGID.lock();
                let termios = TTY_TERMIOS.lock();
                let tostop = (termios.c_lflag & 0x00000100) != 0;
                drop(termios);
                if fg_pgid != 0 && task_pgid != fg_pgid && tostop {
                    crate::fs::pty::deliver_signal_to_pgrp(task_pgid, 22); // SIGTTOU = 22
                    return Err(-4); // EINTR
                }
            }
        }
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

    fn wait_queue(&self) -> Option<alloc::sync::Arc<crate::sync::wait_queue::WaitQueue>> {
        Some(crate::drivers::keyboard::stdin_wait_queue())
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
