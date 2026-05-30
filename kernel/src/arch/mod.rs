//! Architecture-specific code.
//!
//! This module provides abstractions over hardware-specific functionality.
//! Currently only x86_64 is supported, but the module structure allows
//! for easy addition of other architectures (e.g., aarch64).

#[cfg(target_arch = "x86_64")]
pub mod x86_64;
