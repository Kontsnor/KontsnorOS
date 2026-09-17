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

//! Background dirty page writeback and filesystem flusher daemon.

use crate::process::pid::Pid;
use core::sync::atomic::{AtomicU64, Ordering};

/// PID of the background flusher kernel thread.
static FLUSHER_PID: AtomicU64 = AtomicU64::new(0);

/// High watermark threshold for dirty pages (256 pages = 1 MiB).
pub const DIRTY_WATERMARK_PAGES: usize = 256;

/// Wake the background flusher daemon if it is currently sleeping.
pub fn wake_flusher() {
    let pid_val = FLUSHER_PID.load(Ordering::Relaxed);
    if pid_val != 0 {
        let pid = Pid::from_raw(pid_val);
        crate::process::scheduler::wake_task(pid);
    }
}

/// Helper function to put current thread to sleep for `ms` milliseconds.
fn sleep_ms(ms: u64) {
    if let Some(pid) = crate::process::scheduler::current_pid() {
        let deadline = crate::syscall::process::info::get_monotonic_ns() + ms * 1_000_000;
        x86_64::instructions::interrupts::without_interrupts(|| {
            crate::process::scheduler::register_sleep_timer(pid, deadline);
            let sched_lock = crate::process::scheduler::SCHEDULER.lock();
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
                task_arc.lock().state = crate::process::task::TaskState::Blocked;
            }
            drop(sched_lock);
        });
        crate::process::scheduler::schedule();
        crate::process::scheduler::remove_sleep_timer(pid);
    } else {
        crate::process::scheduler::yield_now();
    }
}

/// Entry point for the `kflusher` kernel thread.
fn flusher_thread() {
    loop {
        let dirty_pages = crate::memory::page_cache::DIRTY_PAGE_COUNT.load(Ordering::Relaxed);
        if dirty_pages > 0 {
            crate::fs::vfs::sync_all();
        }

        // Sleep for 250ms (or woken early by wake_flusher when dirty pages cross watermark)
        sleep_ms(250);
    }
}

/// Initialize and start the background writeback flusher daemon.
pub fn init() {
    let pid = crate::process::spawn_kernel_thread(
        alloc::string::String::from("kflusher"),
        flusher_thread,
    );
    FLUSHER_PID.store(pid.as_u64(), Ordering::Release);
    crate::kprintln!(
        "[flusher] Background writeback daemon started (PID {}).",
        pid
    );
}
