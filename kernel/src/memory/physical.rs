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

use crate::kprintln;
use crate::sync::spinlock::TicketLock;
use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use x86_64::structures::paging::{PageTable, PageTableFlags};

use super::PAGE_SIZE;
use core::sync::atomic::{AtomicU8, Ordering};

/// Maximum supported physical memory: 16 GiB.
/// This gives us 4M frames, requiring a 512 KiB bitmap.
const MAX_FRAMES: usize = 4 * 1024 * 1024;

/// Bitmap size in bytes.
const BITMAP_SIZE: usize = MAX_FRAMES / 8;

/// Array tracking the reference counts of all physical memory frames.
/// Each entry represents a 4 KiB frame.
pub static FRAME_REFS: [AtomicU8; MAX_FRAMES] = {
    const ATOMIC_ZERO: AtomicU8 = AtomicU8::new(0);
    [ATOMIC_ZERO; MAX_FRAMES]
};

/// Increment the reference count of a physical frame.
pub fn increment_ref(phys_addr: u64) {
    let index = (phys_addr / PAGE_SIZE as u64) as usize;
    if index < MAX_FRAMES {
        FRAME_REFS[index].fetch_add(1, Ordering::SeqCst);
    }
}

/// Decrement the reference count of a physical frame.
/// Returns the new reference count.
pub fn decrement_ref(phys_addr: u64) -> u8 {
    let index = (phys_addr / PAGE_SIZE as u64) as usize;
    if index < MAX_FRAMES {
        let mut old = FRAME_REFS[index].load(Ordering::SeqCst);
        loop {
            if old == 0 {
                return 0;
            }
            match FRAME_REFS[index].compare_exchange(
                old,
                old - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return old - 1,
                Err(actual) => old = actual,
            }
        }
    }
    0
}

/// The global physical frame allocator.
static FRAME_ALLOCATOR: TicketLock<FrameAllocator> = TicketLock::new(FrameAllocator::new());

struct CoreFrameCache {
    frames: [u64; 16],
    count: usize,
}

impl CoreFrameCache {
    const fn new() -> Self {
        Self {
            frames: [0; 16],
            count: 0,
        }
    }
}

static CORE_CACHES: [TicketLock<CoreFrameCache>; 32] = {
    const CACHE: TicketLock<CoreFrameCache> = TicketLock::new(CoreFrameCache::new());
    [CACHE; 32]
};

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

        let mut searched = 0;
        let mut index = self.next_free_hint;
        while searched < MAX_FRAMES {
            // Try to skip 64 frames (8 bytes) at once
            if index % 64 == 0 && searched + 64 <= MAX_FRAMES {
                let byte_idx = index / 8;
                if byte_idx + 8 <= self.bitmap.len() {
                    let bytes = &self.bitmap[byte_idx..byte_idx + 8];
                    let val = u64::from_ne_bytes(bytes.try_into().unwrap());
                    if val == 0xffff_ffff_ffff_ffffu64 {
                        index = (index + 64) % MAX_FRAMES;
                        searched += 64;
                        continue;
                    }
                }
            }
            // Try to skip 8 frames (1 byte) at once
            if index % 8 == 0 && searched + 8 <= MAX_FRAMES {
                let byte_idx = index / 8;
                if byte_idx < self.bitmap.len() && self.bitmap[byte_idx] == 0xFF {
                    index = (index + 8) % MAX_FRAMES;
                    searched += 8;
                    continue;
                }
            }
            if self.is_free(index) {
                self.mark_used(index);
                self.allocated_frames += 1;
                self.next_free_hint = (index + 1) % MAX_FRAMES;
                return Some(index as u64 * PAGE_SIZE as u64);
            }
            index = (index + 1) % MAX_FRAMES;
            searched += 1;
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

fn reserve_page_table_frame(paddr: u64, allocator: &mut FrameAllocator) {
    let frame_index = (paddr / PAGE_SIZE as u64) as usize;
    if frame_index < MAX_FRAMES {
        allocator.mark_used(frame_index);
    }
}

unsafe fn walk_and_reserve_page_tables(
    pml4_phys: u64,
    phys_mem_offset: u64,
    allocator: &mut FrameAllocator,
) {
    // 1. Reserve PML4 itself
    reserve_page_table_frame(pml4_phys, allocator);

    let pml4_virt = pml4_phys + phys_mem_offset;
    // SAFETY: pml4_virt is mapped and points to the valid PML4 page table.
    let pml4 = unsafe { &*(pml4_virt as *const PageTable) };

    for i in 0..512 {
        let pml4_entry = &pml4[i];
        if pml4_entry.flags().contains(PageTableFlags::PRESENT) {
            if let Ok(pdpt_frame) = pml4_entry.frame() {
                let pdpt_phys = pdpt_frame.start_address().as_u64();
                reserve_page_table_frame(pdpt_phys, allocator);

                let pdpt_virt = pdpt_phys + phys_mem_offset;
                // SAFETY: pdpt_virt is mapped and points to the valid PDPT page table.
                let pdpt = unsafe { &*(pdpt_virt as *const PageTable) };

                for j in 0..512 {
                    let pdpt_entry = &pdpt[j];
                    if pdpt_entry.flags().contains(PageTableFlags::PRESENT) {
                        if pdpt_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
                            continue;
                        }
                        if let Ok(pd_frame) = pdpt_entry.frame() {
                            let pd_phys = pd_frame.start_address().as_u64();
                            reserve_page_table_frame(pd_phys, allocator);

                            let pd_virt = pd_phys + phys_mem_offset;
                            // SAFETY: pd_virt is mapped and points to the valid PD page table.
                            let pd = unsafe { &*(pd_virt as *const PageTable) };

                            for k in 0..512 {
                                let pd_entry = &pd[k];
                                if pd_entry.flags().contains(PageTableFlags::PRESENT) {
                                    if pd_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
                                        continue;
                                    }
                                    if let Ok(pt_frame) = pd_entry.frame() {
                                        let pt_phys = pt_frame.start_address().as_u64();
                                        reserve_page_table_frame(pt_phys, allocator);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Initialize the physical frame allocator using the bootloader's memory map.
///
/// This must be called during early boot, before any physical memory
/// allocation is attempted.
pub fn init(memory_regions: &MemoryRegions, phys_mem_offset: u64) {
    use x86_64::registers::control::Cr3;

    let mut allocator = FRAME_ALLOCATOR.lock();

    for region in memory_regions.iter() {
        if region.kind == MemoryRegionKind::Usable {
            let start_frame = (region.start as usize) / PAGE_SIZE;
            let end_frame = (region.end as usize) / PAGE_SIZE;

            for frame in start_frame..end_frame {
                if frame < MAX_FRAMES {
                    allocator.mark_free(frame);
                }
            }
        }
    }

    // F-07: Walk and reserve active boot page tables to prevent their physical frames
    // from being allocated and zeroed out by the kernel.
    let (active_pml4_frame, _) = Cr3::read();
    let pml4_phys = active_pml4_frame.start_address().as_u64();
    unsafe {
        walk_and_reserve_page_tables(pml4_phys, phys_mem_offset, &mut allocator);
    }

    // Recalculate total free frames after reserving page tables
    let mut actual_free = 0usize;
    for i in 0..MAX_FRAMES {
        if allocator.is_free(i) {
            actual_free += 1;
        }
    }
    allocator.total_frames = actual_free;
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
    let frame = {
        let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
        if apic_id < 32 {
            let mut cache = CORE_CACHES[apic_id].lock();
            if cache.count > 0 {
                cache.count -= 1;
                let frame = cache.frames[cache.count];
                #[cfg(debug_assertions)]
                {
                    let idx = (frame / PAGE_SIZE as u64) as usize;
                    debug_assert!(
                        !FRAME_ALLOCATOR.lock().is_free(idx),
                        "Frame {:#x} popped from core cache was already free in global bitmap (potential double-free)",
                        frame
                    );
                }
                Some(frame)
            } else {
                // Cache is empty; bulk allocate from the global FRAME_ALLOCATOR
                let mut global_alloc = FRAME_ALLOCATOR.lock();
                let mut first_frame = None;
                for _ in 0..8 {
                    if let Some(f) = global_alloc.allocate() {
                        if first_frame.is_none() {
                            first_frame = Some(f);
                        } else {
                            let count = cache.count;
                            cache.frames[count] = f;
                            cache.count += 1;
                        }
                    } else {
                        break;
                    }
                }
                first_frame
            }
        } else {
            // Fallback direct global allocation
            FRAME_ALLOCATOR.lock().allocate()
        }
    };

    if let Some(f) = frame {
        let idx = (f / PAGE_SIZE as u64) as usize;
        if idx < MAX_FRAMES {
            FRAME_REFS[idx].store(1, Ordering::SeqCst);
        }
    }
    frame
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
    let frame_index = (phys_addr / PAGE_SIZE as u64) as usize;
    if frame_index < MAX_FRAMES {
        let mut old = FRAME_REFS[frame_index].load(Ordering::SeqCst);
        let mut new_val;
        loop {
            if old == 0 {
                return;
            }
            new_val = old - 1;
            match FRAME_REFS[frame_index].compare_exchange(
                old,
                new_val,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(actual) => old = actual,
            }
        }
        if new_val > 0 {
            // Still referenced by other page tables. Do not reclaim yet!
            return;
        }
    }

    let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
    if apic_id < 32 {
        let mut cache = CORE_CACHES[apic_id].lock();
        if cache.count < 16 {
            let count = cache.count;
            cache.frames[count] = phys_addr;
            cache.count += 1;
            return;
        }

        // Cache is full; bulk free half of the cache back to the global FRAME_ALLOCATOR
        let mut global_alloc = FRAME_ALLOCATOR.lock();
        for _ in 0..8 {
            cache.count -= 1;
            let f = cache.frames[cache.count];
            global_alloc.deallocate(f);
        }

        // Cache now has space; push the new frame to the local cache
        let count = cache.count;
        cache.frames[count] = phys_addr;
        cache.count += 1;
        return;
    }

    // Fallback direct global deallocation
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

/// Drain the local core frame cache to the global allocator.
pub fn drain_local_cache() {
    let apic_id = crate::arch::x86_64::smp::current_lapic_id() as usize;
    if apic_id < 32 {
        let mut cache = CORE_CACHES[apic_id].lock();
        let mut global_alloc = FRAME_ALLOCATOR.lock();
        while cache.count > 0 {
            cache.count -= 1;
            let f = cache.frames[cache.count];
            global_alloc.deallocate(f);
        }
    }
}

/// Verify COW reference counting and frame reclaim behavior.
pub fn test_cow_refcounts() {
    kprintln!("[test] Starting COW refcount verification test...");
    drain_local_cache();
    let (_, alloc_before, _) = stats();

    // 1. Allocate a page frame
    let phys = allocate_frame().expect("Failed to allocate frame in COW test");

    // 2. Simulate fork: increment refcount of the frame to 2
    increment_ref(phys);

    // 3. Simulate child resolving COW: allocate new child frame, decrement old frame refcount
    let child_phys = allocate_frame().expect("Failed to allocate child frame in COW test");
    let old_ref = decrement_ref(phys);
    assert_eq!(old_ref, 1);

    // 4. Simulate child exit: free child's resolved frame
    deallocate_frame(child_phys);

    // 5. Simulate parent exit: free parent's frame (refcount drops to 0, reclaimed)
    deallocate_frame(phys);

    drain_local_cache();
    let (_, alloc_after, _) = stats();
    assert_eq!(alloc_after, alloc_before);
    kprintln!("[test] COW refcount verification test PASSED!");
}
