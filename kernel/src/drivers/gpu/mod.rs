//! GPU subsystem.
//!
//! Provides the foundation for GPU drivers, including a basic
//! framebuffer abstraction for display output.

pub mod framebuffer;
pub mod bochs;

/// Initialize the GPU subsystem.
pub fn init() {
    bochs::init();
}
