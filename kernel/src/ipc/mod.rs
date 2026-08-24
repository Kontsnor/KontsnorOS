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

//! Inter-Process Communication (IPC) for KontsnorOS.
//!
//! Provides Unix-compatible IPC mechanisms:
//! - Pipes (unidirectional byte streams)
//! - Signals (asynchronous notifications)
//! - Unix domain sockets (placeholder)

use crate::kprintln;
pub mod pipe;
pub mod signal;
pub mod socket;

/// Initialize the IPC subsystem.
pub fn init() {
    kprintln!("[ipc] IPC subsystem ready (pipes, signals).");
}
