//! Driver memory allocation helpers.
//!
//! Provides allocation primitives specifically designed for
//! driver use, with proper alignment and page boundary handling.

/// A page-aligned memory allocation.
#[derive(Debug)]
pub struct PageAllocation {
    /// Virtual address of the allocation.
    pub virt_addr: u64,
    /// Physical address of the allocation.
    pub phys_addr: u64,
    /// Number of pages allocated.
    pub num_pages: usize,
}
