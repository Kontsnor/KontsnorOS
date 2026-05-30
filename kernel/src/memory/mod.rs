//! Memory management subsystem for KontsnorOS.
//!
//! This module provides:
//! - Physical memory frame allocation
//! - Virtual memory / page table management
//! - Kernel heap allocation
//! - Type-safe address wrappers

pub mod address;
pub mod heap;
pub mod physical;
pub mod r#virtual;

/// Page size on x86_64 (4 KiB).
pub const PAGE_SIZE: usize = 4096;
