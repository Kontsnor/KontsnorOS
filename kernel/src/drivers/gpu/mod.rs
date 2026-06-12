//! GPU subsystem.
//!
//! Provides the foundation for GPU drivers, including a basic
//! framebuffer abstraction for display output.

pub mod bochs;
pub mod framebuffer;

/// Initialize the GPU subsystem.
pub fn init() {
    bochs::init();
}
