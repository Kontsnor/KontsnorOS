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
}

/// The PTY master device node.
pub struct PtyMaster {
    inode: Inode,
    shared: Arc<PtyShared>,
}

impl InodeOps for PtyMaster {
    fn inode(&self) -> &Inode {
        &self.inode
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

            // Yield cooperatively to let writing tasks run
            crate::process::scheduler::yield_now();

            // Interruptible by signals
            if let Some(current_pid) = crate::process::scheduler::current_pid() {
                let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
                if let Some(ref mut sched) = *sched_lock {
                    if let Some(task) = sched.get_task(current_pid) {
                        let unblocked = task.pending_signals & !task.blocked_signals;
                        if unblocked != 0 {
                            return Err(-4); // EINTR
                        }
                    }
                }
            }
        }
    }

    /// Write data to the slave (keyboard input). Handles echo and editing.
    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
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

            // SIGINT delivery on Ctrl+C (0x03)
            if isig && byte == 0x03 {
                let pgid = *self.shared.foreground_pgid.lock();
                if pgid != 0 {
                    deliver_signal_to_pgrp(pgid, 2); // SIGINT = 2
                }
                continue;
            }

            if icanon {
                if byte == 0x7F || byte == b'\x08' {
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

        Ok(data.len())
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        match request {
            0x5413 => { // TIOCGWINSZ
                if !crate::syscall::fs::validate_user_ptr(arg as *const u8, core::mem::size_of::<Winsize>()) {
                    return Err(-14); // EFAULT
                }
                let ws = self.shared.winsize.lock();
                unsafe {
                    core::ptr::write(arg as *mut Winsize, *ws);
                }
                Ok(0)
            }
            0x5414 => { // TIOCSWINSZ
                if !crate::syscall::fs::validate_user_ptr(arg as *const u8, core::mem::size_of::<Winsize>()) {
                    return Err(-14); // EFAULT
                }
                let mut ws = self.shared.winsize.lock();
                unsafe {
                    *ws = core::ptr::read(arg as *const Winsize);
                }
                Ok(0)
            }
            _ => Err(-22), // EINVAL
        }
    }
}

/// The PTY slave device node.
pub struct PtySlave {
    inode: Inode,
    shared: Arc<PtyShared>,
}

impl InodeOps for PtySlave {
    fn inode(&self) -> &Inode {
        &self.inode
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
            }

            // Yield cooperatively to wait for master writes
            crate::process::scheduler::yield_now();

            // Interruptible by signals
            if let Some(current_pid) = crate::process::scheduler::current_pid() {
                let mut sched_lock = crate::process::scheduler::SCHEDULER.lock();
                if let Some(ref mut sched) = *sched_lock {
                    if let Some(task) = sched.get_task(current_pid) {
                        let unblocked = task.pending_signals & !task.blocked_signals;
                        if unblocked != 0 {
                            return Err(-4); // EINTR
                        }
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
        Ok(data.len())
    }

    fn ioctl(&self, request: u64, arg: u64) -> Result<u64, i32> {
        match request {
            0x5401 => { // TCGETS
                if !crate::syscall::fs::validate_user_ptr(arg as *const u8, core::mem::size_of::<Termios>()) {
                    return Err(-14); // EFAULT
                }
                let t = self.shared.termios.lock();
                unsafe {
                    core::ptr::write(arg as *mut Termios, *t);
                }
                Ok(0)
            }
            0x5402 | 0x5403 | 0x5404 => { // TCSETS, TCSETSW, TCSETSF
                if !crate::syscall::fs::validate_user_ptr(arg as *const u8, core::mem::size_of::<Termios>()) {
                    return Err(-14); // EFAULT
                }
                let mut t = self.shared.termios.lock();
                unsafe {
                    *t = core::ptr::read(arg as *const Termios);
                }
                Ok(0)
            }
            0x5413 => { // TIOCGWINSZ
                if !crate::syscall::fs::validate_user_ptr(arg as *const u8, core::mem::size_of::<Winsize>()) {
                    return Err(-14); // EFAULT
                }
                let ws = self.shared.winsize.lock();
                unsafe {
                    core::ptr::write(arg as *mut Winsize, *ws);
                }
                Ok(0)
            }
            0x5414 => { // TIOCSWINSZ
                if !crate::syscall::fs::validate_user_ptr(arg as *const u8, core::mem::size_of::<Winsize>()) {
                    return Err(-14); // EFAULT
                }
                let mut ws = self.shared.winsize.lock();
                unsafe {
                    *ws = core::ptr::read(arg as *const Winsize);
                }
                Ok(0)
            }
            0x540F => { // TIOCGPGRP
                if !crate::syscall::fs::validate_user_ptr(arg as *mut i32 as *const u8, core::mem::size_of::<i32>()) {
                    return Err(-14); // EFAULT
                }
                let pgid = *self.shared.foreground_pgid.lock() as i32;
                unsafe {
                    core::ptr::write(arg as *mut i32, pgid);
                }
                Ok(0)
            }
            0x5410 => { // TIOCSPGRP
                if !crate::syscall::fs::validate_user_ptr(arg as *const i32 as *const u8, core::mem::size_of::<i32>()) {
                    return Err(-14); // EFAULT
                }
                let pgid = unsafe { core::ptr::read(arg as *const i32) } as u64;
                *self.shared.foreground_pgid.lock() = pgid;
                Ok(0)
            }
            _ => Err(-22), // EINVAL
        }
    }
}

/// Helper function to deliver signal to process group.
pub fn deliver_signal_to_pgrp(pgid: u64, sig: i32) {
    use crate::process::scheduler;
    if sig < 1 || sig > 64 || pgid == 0 {
        return;
    }
    let mut sched_lock = scheduler::SCHEDULER.lock();
    if let Some(ref mut sched) = *sched_lock {
        let mut pids_to_wake = alloc::vec::Vec::new();
        let curr_pid = scheduler::current_pid();
        for task_opt in sched.tasks.iter_mut() {
            if let Some(ref mut task) = task_opt {
                if task.pgid == pgid {
                    task.pending_signals |= 1 << (sig - 1);
                    if Some(task.pid) == curr_pid {
                        let pending_unblocked = task.pending_signals & !task.blocked_signals;
                        unsafe {
                            crate::syscall::CPU_SCRATCH.signals_pending = if pending_unblocked != 0 { 1 } else { 0 };
                        }
                    }
                    pids_to_wake.push(task.pid);
                }
            }
        }
        for pid in pids_to_wake {
            sched.wake_task(pid);
        }
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
    });

    let master = Arc::new(PtyMaster {
        inode: Inode::new(10000 + id as u64 * 2, FileType::CharDevice),
        shared: shared.clone(),
    });

    let slave = Arc::new(PtySlave {
        inode: Inode::new(10000 + id as u64 * 2 + 1, FileType::CharDevice),
        shared,
    });

    // Register slave device in devfs under "/dev/pts/<id>"
    let name = alloc::format!("{}", id);
    crate::fs::devfs::register_pts_device(name, slave);

    Ok(master as Arc<dyn InodeOps>)
}
