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

//! Pseudoterminals (PTY) multiplexing.
//!
//! Provides master/slave pairs (/dev/ptmx and /dev/pts/*) used by terminal
//! emulators and shells for interactive sessions. Implements line discipline
//! formatting (canonical editing, echo, and signal routing).

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use spin::Mutex;

use crate::fs::inode::{FileType, Inode, InodeOps};
use crate::fs::tty::{Termios, Winsize};

/// Shared state of a PTY master/slave pair.
pub struct PtyShared {
    pub id: usize,
    /// Queue of bytes written by the slave (program output) -> read by the master.
    pub master_read_queue: Mutex<VecDeque<u8>>,
    /// Queue of cooked bytes written by the master (keyboard input) -> read by the slave.
    pub slave_read_queue: Mutex<VecDeque<u8>>,
    /// Queue of raw keyboard bytes typed before Enter is pressed (canonical mode).
    pub raw_input_queue: Mutex<VecDeque<u8>>,
    /// Controlling terminal settings.
    pub termios: Mutex<Termios>,
    /// Terminal window size.
    pub winsize: Mutex<Winsize>,
    /// Foreground process group ID (for job control signal delivery).
    pub foreground_pgid: Mutex<u64>,
    /// Pending EOF flag (Ctrl+D on empty input in canonical mode).
    pub eof_pending: core::sync::atomic::AtomicBool,
    pub wait_queue: crate::sync::wait_queue::WaitQueue,
}

/// The PTY master device node.
pub struct PtyMaster {
    inode: Inode,
    shared: Arc<PtyShared>,
    pub non_blocking: core::sync::atomic::AtomicBool,
}

impl InodeOps for PtyMaster {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn set_nonblocking(&self, nonblocking: bool) {
        self.non_blocking
            .store(nonblocking, core::sync::atomic::Ordering::SeqCst);
    }

    /// Read data written by the slave (program output).
    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            {
                let mut queue = self.shared.master_read_queue.lock();
                if !queue.is_empty() {
                    let mut count = 0;
                    while count < buf.len() {
                        if let Some(ch) = queue.pop_front() {
                            buf[count] = ch;
                            count += 1;
                        } else {
                            break;
                        }
                    }
                    return Ok(count);
                }
            }

            if self.non_blocking.load(core::sync::atomic::Ordering::SeqCst) {
                return Err(-11); // -EAGAIN
            }

            // Interruptible by signals
            if let Some(current_pid) = crate::process::scheduler::current_pid() {
                if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
                    let task = task_arc.lock();
                    let unblocked = task.pending_signals & !task.blocked_signals;
                    if unblocked != 0 {
                        return Err(-4); // EINTR
                    }
                }
            }

            self.shared.wait_queue.wait();

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
    }

    /// Write data to the slave (keyboard input). Handles echo and editing.
    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        let mut sig_to_deliver = None;

        {
            let mut slave_read = self.shared.slave_read_queue.lock();
            let mut raw_input = self.shared.raw_input_queue.lock();
            let mut master_read = self.shared.master_read_queue.lock();
            let termios = self.shared.termios.lock();

            let icanon = (termios.c_lflag & 0x00000002) != 0;
            let echo = (termios.c_lflag & 0x00000008) != 0;
            let isig = (termios.c_lflag & 0x00000001) != 0;

            for &byte in data {
                let mut byte = byte;
                if byte == b'\r' {
                    byte = b'\n';
                }

                // 1. Interactive terminal signals when ISIG is enabled
                if isig {
                    if byte == 0x03 {
                        // Ctrl+C -> SIGINT (signal 2)
                        if echo {
                            master_read.push_back(b'^');
                            master_read.push_back(b'C');
                            master_read.push_back(b'\n');
                        }
                        raw_input.clear();
                        let mut pgid = *self.shared.foreground_pgid.lock();
                        if pgid == 0 || pgid == 1 {
                            pgid = find_foreground_pgid();
                        }
                        sig_to_deliver = Some((pgid, 2));
                        continue;
                    } else if byte == 0x1C {
                        // Ctrl+\ -> SIGQUIT (signal 3)
                        if echo {
                            master_read.push_back(b'^');
                            master_read.push_back(b'\\');
                            master_read.push_back(b'\n');
                        }
                        raw_input.clear();
                        let mut pgid = *self.shared.foreground_pgid.lock();
                        if pgid == 0 || pgid == 1 {
                            pgid = find_foreground_pgid();
                        }
                        sig_to_deliver = Some((pgid, 3));
                        continue;
                    } else if byte == 0x1A {
                        // Ctrl+Z -> SIGTSTP (signal 20)
                        if echo {
                            master_read.push_back(b'^');
                            master_read.push_back(b'Z');
                            master_read.push_back(b'\n');
                        }
                        let mut pgid = *self.shared.foreground_pgid.lock();
                        if pgid == 0 || pgid == 1 {
                            pgid = find_foreground_pgid();
                        }
                        sig_to_deliver = Some((pgid, 20));
                        continue;
                    }
                }

                // 2. Canonical mode line discipline
                if icanon {
                    if byte == 0x04 {
                        // Ctrl+D (EOF in canonical mode)
                        if raw_input.is_empty() {
                            self.shared
                                .eof_pending
                                .store(true, core::sync::atomic::Ordering::Release);
                        } else {
                            // Flush uncommitted input to slave without newline
                            while let Some(ch) = raw_input.pop_front() {
                                slave_read.push_back(ch);
                            }
                        }
                        continue;
                    } else if byte == 0x15 {
                        // Ctrl+U (Line Kill)
                        while let Some(popped) = raw_input.pop_back() {
                            if echo && popped != b'\n' {
                                master_read.push_back(b'\x08');
                                master_read.push_back(b' ');
                                master_read.push_back(b'\x08');
                            }
                        }
                        continue;
                    } else if byte == 0x17 {
                        // Ctrl+W (Word Erase)
                        // Skip trailing whitespace
                        while let Some(&last) = raw_input.back() {
                            if last == b' ' || last == b'\t' {
                                raw_input.pop_back();
                                if echo {
                                    master_read.push_back(b'\x08');
                                    master_read.push_back(b' ');
                                    master_read.push_back(b'\x08');
                                }
                            } else {
                                break;
                            }
                        }
                        // Erase word characters
                        while let Some(&last) = raw_input.back() {
                            if last != b' ' && last != b'\t' && last != b'\n' {
                                raw_input.pop_back();
                                if echo {
                                    master_read.push_back(b'\x08');
                                    master_read.push_back(b' ');
                                    master_read.push_back(b'\x08');
                                }
                            } else {
                                break;
                            }
                        }
                        continue;
                    } else if byte == 0x7F || byte == b'\x08' {
                        if let Some(popped) = raw_input.pop_back() {
                            if echo {
                                if popped != b'\n' {
                                    master_read.push_back(b'\x08');
                                    master_read.push_back(b' ');
                                    master_read.push_back(b'\x08');
                                }
                            }
                        }
                    } else {
                        raw_input.push_back(byte);
                        if echo {
                            master_read.push_back(byte);
                        }
                        if byte == b'\n' {
                            while let Some(ch) = raw_input.pop_front() {
                                slave_read.push_back(ch);
                            }
                        }
                    }
                } else {
                    slave_read.push_back(byte);
                    if echo {
                        master_read.push_back(byte);
                    }
                }
            }
        }

        self.shared.wait_queue.wake_all();

        if let Some((pgid, sig)) = sig_to_deliver {
            if pgid != 0 {
                deliver_signal_to_pgrp(pgid, sig);
            }
        }

        Ok(data.len())
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        if (events & crate::fs::inode::POLLIN) != 0 {
            if !self.shared.master_read_queue.lock().is_empty() {
                revents |= crate::fs::inode::POLLIN;
            }
        }
        if (events & crate::fs::inode::POLLOUT) != 0 {
            revents |= crate::fs::inode::POLLOUT;
        }
        revents
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        match request {
            0x5413 => {
                // TIOCGWINSZ
                if !crate::syscall::fs::validate_user_ptr(
                    arg as *const u8,
                    core::mem::size_of::<Winsize>(),
                ) {
                    return Err(-14); // EFAULT
                }
                let ws = self.shared.winsize.lock();
                unsafe {
                    core::ptr::write(arg as *mut Winsize, *ws);
                }
                Ok(0)
            }
            0x5414 => {
                // TIOCSWINSZ
                if !crate::syscall::fs::validate_user_ptr(
                    arg as *const u8,
                    core::mem::size_of::<Winsize>(),
                ) {
                    return Err(-14); // EFAULT
                }
                let mut ws = self.shared.winsize.lock();
                unsafe {
                    *ws = core::ptr::read(arg as *const Winsize);
                }
                Ok(0)
            }
            0x5421 => {
                // FIONBIO
                if !crate::syscall::fs::validate_user_ptr(arg as *const u8, 4) {
                    return Err(-14); // EFAULT
                }
                let val = unsafe { *(arg as *const i32) };
                self.non_blocking
                    .store(val != 0, core::sync::atomic::Ordering::SeqCst);
                Ok(0)
            }
            _ => Err(-25), // ENOTTY
        }
    }
}

/// The PTY slave device node.
pub struct PtySlave {
    inode: Inode,
    shared: Arc<PtyShared>,
    pub non_blocking: core::sync::atomic::AtomicBool,
}

impl InodeOps for PtySlave {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn set_nonblocking(&self, nonblocking: bool) {
        self.non_blocking
            .store(nonblocking, core::sync::atomic::Ordering::SeqCst);
    }

    /// Read data written by the master (keyboard input).
    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            {
                let mut queue = self.shared.slave_read_queue.lock();
                if !queue.is_empty() {
                    let mut count = 0;
                    while count < buf.len() {
                        if let Some(ch) = queue.pop_front() {
                            buf[count] = ch;
                            count += 1;
                        } else {
                            break;
                        }
                    }
                    return Ok(count);
                }
                if self
                    .shared
                    .eof_pending
                    .swap(false, core::sync::atomic::Ordering::AcqRel)
                {
                    return Ok(0); // EOF
                }
            }

            // Track reader pgid as foreground if not currently set
            if *self.shared.foreground_pgid.lock() == 0 {
                if let Some(current_pid) = crate::process::scheduler::current_pid() {
                    if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
                        *self.shared.foreground_pgid.lock() = task_arc.lock().pgid;
                    }
                }
            }

            if self.non_blocking.load(core::sync::atomic::Ordering::SeqCst) {
                return Err(-11); // -EAGAIN
            }

            // Interruptible by signals
            if let Some(current_pid) = crate::process::scheduler::current_pid() {
                if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
                    let task = task_arc.lock();
                    let unblocked = task.pending_signals & !task.blocked_signals;
                    if unblocked != 0 {
                        return Err(-4); // EINTR
                    }
                }
            }

            self.shared.wait_queue.wait();

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
    }

    /// Write data to the master (program output).
    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        let mut master_read = self.shared.master_read_queue.lock();
        for &byte in data {
            master_read.push_back(byte);
        }
        drop(master_read);
        self.shared.wait_queue.wake_all();
        Ok(data.len())
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        if (events & crate::fs::inode::POLLIN) != 0 {
            if !self.shared.slave_read_queue.lock().is_empty() {
                revents |= crate::fs::inode::POLLIN;
            }
        }
        if (events & crate::fs::inode::POLLOUT) != 0 {
            revents |= crate::fs::inode::POLLOUT;
        }
        revents
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
                let t = self.shared.termios.lock();
                unsafe {
                    core::ptr::write(arg as *mut Termios, *t);
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
                let mut t = self.shared.termios.lock();
                unsafe {
                    *t = core::ptr::read(arg as *const Termios);
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
                let ws = self.shared.winsize.lock();
                unsafe {
                    core::ptr::write(arg as *mut Winsize, *ws);
                }
                Ok(0)
            }
            0x5414 => {
                // TIOCSWINSZ
                if !crate::syscall::fs::validate_user_ptr(
                    arg as *const u8,
                    core::mem::size_of::<Winsize>(),
                ) {
                    return Err(-14); // EFAULT
                }
                let mut ws = self.shared.winsize.lock();
                unsafe {
                    *ws = core::ptr::read(arg as *const Winsize);
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
                let mut pgid_lock = self.shared.foreground_pgid.lock();
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
                *self.shared.foreground_pgid.lock() = pgid;
                Ok(0)
            }
            0x540E => {
                // TIOCSCTTY: Make the terminal the controlling terminal for the calling process
                if let Some(pid) = crate::process::scheduler::current_pid() {
                    if let Some(task) = crate::process::scheduler::get_task_arc(pid) {
                        *self.shared.foreground_pgid.lock() = task.lock().pgid;
                    }
                }
                Ok(0)
            }
            0x5421 => {
                // FIONBIO
                if !crate::syscall::fs::validate_user_ptr(arg as *const u8, 4) {
                    return Err(-14); // EFAULT
                }
                let val = unsafe { *(arg as *const i32) };
                self.non_blocking
                    .store(val != 0, core::sync::atomic::Ordering::SeqCst);
                Ok(0)
            }
            _ => Err(-25), // ENOTTY
        }
    }
}

/// Helper function to find the active foreground process group.
/// Scans running/runnable user tasks with PID > 1.
pub fn find_foreground_pgid() -> u64 {
    use crate::process::scheduler;
    let tasks = scheduler::TASKS.read();
    let mut best_pid: u64 = 0;
    let mut best_pgid: u64 = 0;
    for slot in tasks.iter() {
        if let Some(task_arc) = slot {
            if let Some(task) = task_arc.try_lock() {
                let pid = task.pid.as_u64();
                // Never target PID 0 (kernel), PID 1 (init), or kernel threads
                if pid > 1
                    && task.is_user()
                    && task.state != crate::process::task::TaskState::Zombie
                {
                    if pid > best_pid {
                        best_pid = pid;
                        best_pgid = task.pgid;
                    }
                }
            }
        }
    }
    best_pgid
}

/// Helper function to deliver signal to process group.
pub fn deliver_signal_to_pgrp(pgid: u64, sig: i32) {
    use crate::process::scheduler;
    if sig < 1 || sig > 64 || pgid == 0 {
        return;
    }
    let tasks = scheduler::TASKS.read();
    let mut pids = alloc::vec::Vec::new();
    let mut seen_tgids = alloc::vec::Vec::new();
    for task_opt in tasks.iter() {
        if let Some(task_arc) = task_opt {
            if let Some(task) = task_arc.try_lock() {
                // In POSIX, never deliver fatal interactive keyboard signals to PID 1 or kernel threads
                if (task.pid.as_u64() == 1 || task.is_pid_ns_init)
                    && (sig == 2 || sig == 3 || sig == 20)
                {
                    continue;
                }
                if !task.is_user() {
                    continue;
                }
                if task.pgid == pgid {
                    let tgid = task.tgid;
                    // In POSIX, process group signals are delivered per-process (thread group),
                    // not broadcast to every sibling thread individually.
                    if !seen_tgids.contains(&tgid) {
                        seen_tgids.push(tgid);
                        pids.push(tgid);
                    }
                }
            }
        }
    }
    drop(tasks);
    for pid in pids {
        crate::syscall::signal::deliver_signal(pid, sig);
    }
}

/// Allocates a new PTY pair and registers the slave node in `/dev/pts`.
pub fn allocate_new_pty() -> Result<Arc<dyn InodeOps>, i32> {
    static NEXT_PTY_ID: spin::Mutex<usize> = spin::Mutex::new(0);
    let mut id_lock = NEXT_PTY_ID.lock();
    let id = *id_lock;
    *id_lock += 1;

    let shared = Arc::new(PtyShared {
        id,
        master_read_queue: Mutex::new(VecDeque::new()),
        slave_read_queue: Mutex::new(VecDeque::new()),
        raw_input_queue: Mutex::new(VecDeque::new()),
        termios: Mutex::new(Termios {
            c_iflag: 0,
            c_oflag: 0,
            c_cflag: 0,
            c_lflag: 0x00000002 | 0x00000008 | 0x00000001, // ICANON | ECHO | ISIG
            c_line: 0,
            c_cc: [0; 19],
        }),
        winsize: Mutex::new(Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
        foreground_pgid: Mutex::new(0),
        eof_pending: core::sync::atomic::AtomicBool::new(false),
        wait_queue: crate::sync::wait_queue::WaitQueue::new(),
    });

    let master = Arc::new(PtyMaster {
        inode: Inode::new(10000 + id as u64 * 2, FileType::CharDevice),
        shared: shared.clone(),
        non_blocking: core::sync::atomic::AtomicBool::new(false),
    });

    let slave = Arc::new(PtySlave {
        inode: Inode::new(10000 + id as u64 * 2 + 1, FileType::CharDevice),
        shared,
        non_blocking: core::sync::atomic::AtomicBool::new(false),
    });

    // Register slave device in devfs under "/dev/pts/<id>"
    let name = alloc::format!("{}", id);
    crate::fs::devfs::register_pts_device(name, slave);

    Ok(master as Arc<dyn InodeOps>)
}

/// Global active PTY master reference.
pub static ACTIVE_PTY_MASTER: spin::Mutex<Option<Arc<dyn InodeOps>>> = spin::Mutex::new(None);

fn pty_flusher_thread() {
    let mut buf = [0u8; 128];
    loop {
        let master_opt = ACTIVE_PTY_MASTER.lock().clone();
        if let Some(master) = master_opt {
            match master.read(0, &mut buf) {
                Ok(n) if n > 0 => {
                    x86_64::instructions::interrupts::without_interrupts(|| {
                        if let Some(ref mut console) =
                            *crate::drivers::gpu::bochs::GRAPHICS_CONSOLE.lock()
                        {
                            for &byte in &buf[..n] {
                                console.write_char(byte);
                            }
                            console.gpu.blit();
                        }
                    });
                    // Mirror output to the serial port
                    for &byte in &buf[..n] {
                        crate::arch::x86_64::serial::write_byte(byte);
                    }
                }
                _ => {}
            }
        }
        crate::process::scheduler::yield_now();
    }
}

pub const DEBUG_PTY_ROUTER: bool = false;

fn pty_router_thread() {
    loop {
        // Poll serial input for QEMU stdio
        if let Some(ch) = crate::arch::x86_64::serial::try_read_byte() {
            if DEBUG_PTY_ROUTER {
                crate::kprintln!("[pty_router] serial byte: '{}' ({:#x})", ch as char, ch);
            }
            let mut byte = ch;
            if byte == b'\r' {
                byte = b'\n';
            }
            crate::drivers::keyboard::push_char(byte);
        }

        // Drain keyboard input buffer and write to the PTY master
        let mut temp_buf = [0u8; 64];
        let mut count = 0;
        while count < temp_buf.len() {
            if let Some(ch) = crate::drivers::keyboard::try_read_char() {
                temp_buf[count] = ch;
                count += 1;
            } else {
                break;
            }
        }
        if count > 0 {
            let master_opt = ACTIVE_PTY_MASTER.lock().clone();
            if let Some(master) = master_opt {
                let _ = master.write(0, &temp_buf[..count]);
            }
        }

        crate::process::scheduler::yield_now();
    }
}

/// Starts the PTY master I/O flusher and router threads.
pub fn start_pty_io_loop() {
    crate::process::spawn_kernel_thread(
        alloc::string::String::from("pty_flusher"),
        pty_flusher_thread,
    );
    crate::process::spawn_kernel_thread(
        alloc::string::String::from("pty_router"),
        pty_router_thread,
    );
}
