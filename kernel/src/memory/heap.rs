//! Kernel heap allocator.
//!
//! This module sets up a heap for kernel-space dynamic memory allocation,
//! enabling use of `alloc` types like `Vec`, `Box`, `String`, etc.
//!
//! ## Memory Layout
//!
//! ```text
//! Virtual Address Space:
//! ┌─────────────────────────────────┐ 0xFFFF_FFFF_FFFF_FFFF
//! │         Kernel Code/Data        │
//! ├─────────────────────────────────┤
//! │         Kernel Heap             │ HEAP_START (64 MiB region)
//! │         (grows upward)          │
//! ├─────────────────────────────────┤ HEAP_START + HEAP_SIZE
//! │              ...                │
//! └─────────────────────────────────┘
//! ```

use crate::kprintln;
use linked_list_allocator::LockedHeap;
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::VirtAddr;

use super::PAGE_SIZE;

/// Start address of the kernel heap.
///
/// Placed at a high virtual address to leave room for user space mappings.
pub const HEAP_START: u64 = 0xFFFF_8000_0000_0000;

/// Size of the kernel heap (64 MiB).
///
/// This is the initial heap size. The heap can be extended later by
/// mapping additional pages.
pub const HEAP_SIZE: u64 = 64 * 1024 * 1024;

/// The global kernel heap allocator.
///
/// Uses a linked-list allocator which provides reasonable performance
/// for general-purpose allocation patterns.
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Initialize the kernel heap.
///
/// This function:
/// 1. Allocates physical frames for the heap
/// 2. Maps them to contiguous virtual addresses starting at `HEAP_START`
/// 3. Initializes the linked-list allocator over the mapped region
///
/// # Errors
///
/// Returns an error if physical frame allocation or page mapping fails.
pub fn init() -> Result<(), &'static str> {
    let num_pages = (HEAP_SIZE as usize) / PAGE_SIZE;

    kprintln!(
        "[heap] Allocating {} pages ({} MiB) at {:#x}",
        num_pages,
        HEAP_SIZE / (1024 * 1024),
        HEAP_START
    );

    for i in 0..num_pages {
        let page_addr = HEAP_START + (i as u64 * PAGE_SIZE as u64);
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(page_addr));

        let frame_addr =
            super::physical::allocate_frame().ok_or("Out of physical memory for heap")?;
        let frame = PhysFrame::containing_address(x86_64::PhysAddr::new(frame_addr));

        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;

        // SAFETY: We are mapping fresh physical frames to unused virtual addresses
        // in the kernel heap region. Each frame is used exactly once.
        unsafe {
            super::r#virtual::map_page(page, frame, flags)?;
        }
    }

    // SAFETY: The heap memory region [HEAP_START, HEAP_START + HEAP_SIZE) has
    // been fully mapped with writable pages. No other code uses this region.
    unsafe {
        ALLOCATOR
            .lock()
            .init(HEAP_START as *mut u8, HEAP_SIZE as usize);
    }

    kprintln!("[heap] Kernel heap initialized successfully.");
    Ok(())
}

/// Get heap usage statistics.
///
/// Returns (used_bytes, free_bytes, total_bytes).
pub fn stats() -> (usize, usize, usize) {
    let allocator = ALLOCATOR.lock();
    let free = allocator.free();
    let used = allocator.used();
    (used, free, used + free)
}
