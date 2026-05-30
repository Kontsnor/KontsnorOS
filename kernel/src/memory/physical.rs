//! Physical memory frame allocator.
//!
//! This module manages the allocation and deallocation of physical memory
//! frames (4 KiB pages). It uses the memory map provided by the bootloader
//! to identify usable memory regions.
//!
//! ## Design
//!
//! We use a bitmap-based allocator where each bit represents a physical
//! frame. A set bit means the frame is in use; a clear bit means it's free.
//! This provides O(1) allocation (amortized) with efficient memory usage.

use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use crate::sync::spinlock::TicketLock;
use crate::kprintln;

use super::PAGE_SIZE;

/// Maximum supported physical memory: 4 GiB initially.
/// This gives us 1M frames, requiring a 128 KiB bitmap.
const MAX_FRAMES: usize = 1024 * 1024;

/// Bitmap size in bytes.
const BITMAP_SIZE: usize = MAX_FRAMES / 8;

/// The global physical frame allocator.
static FRAME_ALLOCATOR: TicketLock<FrameAllocator> = TicketLock::new(FrameAllocator::new());

/// A bitmap-based physical frame allocator.
///
/// Each bit in the bitmap represents a 4 KiB physical frame.
/// - Bit set (1) = frame is allocated / reserved
/// - Bit clear (0) = frame is free
struct FrameAllocator {
    /// Bitmap tracking frame allocation state.
    bitmap: [u8; BITMAP_SIZE],
    /// Total number of usable frames.
    total_frames: usize,
    /// Number of currently allocated frames.
    allocated_frames: usize,
    /// Hint for where to start searching for free frames.
    next_free_hint: usize,
    /// Whether the allocator has been initialized.
    initialized: bool,
}

impl FrameAllocator {
    /// Create a new, uninitialized frame allocator.
    const fn new() -> Self {
        Self {
            bitmap: [0xFF; BITMAP_SIZE], // Mark all frames as used initially
            total_frames: 0,
            allocated_frames: 0,
            next_free_hint: 0,
            initialized: false,
        }
    }

    /// Mark a frame as free in the bitmap.
    fn mark_free(&mut self, frame_index: usize) {
        if frame_index < MAX_FRAMES {
            let byte_index = frame_index / 8;
            let bit_index = frame_index % 8;
            self.bitmap[byte_index] &= !(1 << bit_index);
        }
    }

    /// Mark a frame as used in the bitmap.
    fn mark_used(&mut self, frame_index: usize) {
        if frame_index < MAX_FRAMES {
            let byte_index = frame_index / 8;
            let bit_index = frame_index % 8;
            self.bitmap[byte_index] |= 1 << bit_index;
        }
    }

    /// Check if a frame is free.
    fn is_free(&self, frame_index: usize) -> bool {
        if frame_index >= MAX_FRAMES {
            return false;
        }
        let byte_index = frame_index / 8;
        let bit_index = frame_index % 8;
        self.bitmap[byte_index] & (1 << bit_index) == 0
    }

    /// Find and allocate a free frame, returning its physical address.
    fn allocate(&mut self) -> Option<u64> {
        if !self.initialized {
            return None;
        }

        // Start searching from the hint
        for i in 0..MAX_FRAMES {
            let index = (self.next_free_hint + i) % MAX_FRAMES;
            if self.is_free(index) {
                self.mark_used(index);
                self.allocated_frames += 1;
                self.next_free_hint = (index + 1) % MAX_FRAMES;
                return Some(index as u64 * PAGE_SIZE as u64);
            }
        }

        None // Out of memory
    }

    /// Free a previously allocated frame.
    fn deallocate(&mut self, phys_addr: u64) {
        let frame_index = (phys_addr / PAGE_SIZE as u64) as usize;
        if frame_index < MAX_FRAMES && !self.is_free(frame_index) {
            self.mark_free(frame_index);
            self.allocated_frames -= 1;

            // Update hint to point to this freed frame for faster reallocation
            if frame_index < self.next_free_hint {
                self.next_free_hint = frame_index;
            }
        }
    }
}

/// Initialize the physical frame allocator using the bootloader's memory map.
///
/// This must be called during early boot, before any physical memory
/// allocation is attempted.
pub fn init(memory_regions: &MemoryRegions) {
    let mut allocator = FRAME_ALLOCATOR.lock();

    let mut total = 0usize;

    for region in memory_regions.iter() {
        if region.kind == MemoryRegionKind::Usable {
            let start_frame = (region.start as usize) / PAGE_SIZE;
            let end_frame = (region.end as usize) / PAGE_SIZE;

            for frame in start_frame..end_frame {
                if frame < MAX_FRAMES {
                    allocator.mark_free(frame);
                    total += 1;
                }
            }
        }
    }

    allocator.total_frames = total;
    allocator.allocated_frames = 0;
    allocator.initialized = true;

    // Don't hold the lock while printing
    let total_frames = allocator.total_frames;
    drop(allocator);

    kprintln!(
        "[memory] Physical frames available: {} ({} MiB)",
        total_frames,
        (total_frames * PAGE_SIZE) / (1024 * 1024)
    );
}

/// Allocate a single physical frame.
///
/// Returns the physical address of the allocated frame, or `None` if
/// no free frames are available.
pub fn allocate_frame() -> Option<u64> {
    FRAME_ALLOCATOR.lock().allocate()
}

/// Free a previously allocated physical frame.
///
/// # Safety
///
/// The caller must ensure that:
/// - The address was previously returned by `allocate_frame()`
/// - The frame is no longer in use by any page table or data structure
/// - The frame is not freed more than once
pub fn deallocate_frame(phys_addr: u64) {
    FRAME_ALLOCATOR.lock().deallocate(phys_addr);
}

/// Get memory statistics.
///
/// Returns (total_frames, allocated_frames, free_frames).
pub fn stats() -> (usize, usize, usize) {
    let allocator = FRAME_ALLOCATOR.lock();
    let total = allocator.total_frames;
    let allocated = allocator.allocated_frames;
    (total, allocated, total - allocated)
}
