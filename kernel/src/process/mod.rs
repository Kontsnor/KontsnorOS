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

//! Process management subsystem for KontsnorOS.
//!
//! This module provides Unix-compatible process management including:
//! - Task/Thread Control Blocks
//! - PID allocation
//! - Process scheduling (multi-level feedback queue)
//! - Context switching

use crate::kprintln;

pub mod binaries;
pub mod context;
pub mod elf;
pub mod fd;
pub mod lifecycle;
pub mod pid;
pub mod scheduler;
pub mod task;

pub use binaries::create_net_test_elf;
pub use binaries::{hello_elf, net_test_elf, shell_elf};
pub use lifecycle::{
    spawn_kernel_thread, spawn_user_process,
    spawn_user_process_with_pid,
};

/// Initialize the process management subsystem.
pub fn init() {
    pid::init();
    scheduler::init();

    // Register the early boot thread as a real running task (PID 1, "main")
    let pid = pid::allocate(); // Should allocate 1
    let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
    let cr3_val = cr3_frame.start_address().as_u64();

    let main_task = task::Task::new(pid, alloc::string::String::from("main"), cr3_val);

    // Set the boot thread as active in the scheduler
    scheduler::set_bootstrap_thread(main_task);

    kprintln!(
        "[process] Process subsystem ready. Bootstrap thread registered as PID {}.",
        pid
    );
}
