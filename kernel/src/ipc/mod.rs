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
