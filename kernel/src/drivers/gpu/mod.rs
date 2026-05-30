//! GPU subsystem.
//!
//! Provides the foundation for GPU drivers, including a basic
//! framebuffer abstraction for display output.

use crate::kprintln;
pub mod framebuffer;

/// Initialize the GPU subsystem.
pub fn init() {
    kprintln!("[gpu] GPU subsystem initialized (no GPU drivers loaded).");
}
