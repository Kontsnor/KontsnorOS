//! Virtual memory management for x86_64.
//!
//! This module manages the 4-level page table hierarchy used by x86_64
//! for virtual-to-physical address translation.
//!
//! ## Page Table Hierarchy
//!
//! ```text
//! Level 4 (PML4) ──→ Level 3 (PDPT) ──→ Level 2 (PD) ──→ Level 1 (PT) ──→ Physical Frame
//!   9 bits              9 bits             9 bits           9 bits           12 bits
//!   [47:39]             [38:30]            [29:21]          [20:12]          [11:0]
//! ```

use spin::Mutex;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator as X86FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
    Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

/// The physical memory offset provided by the bootloader.
///
/// All physical memory is mapped starting at this virtual address,
/// allowing the kernel to access any physical address by adding this offset.
static PHYS_MEM_OFFSET: Mutex<Option<u64>> = Mutex::new(None);

/// Initialize the virtual memory manager.
///
/// `phys_mem_offset` is the virtual address where physical memory is
/// mapped by the bootloader.
pub fn init(phys_mem_offset: u64) {
    *PHYS_MEM_OFFSET.lock() = Some(phys_mem_offset);
}

/// Get the physical memory offset.
pub fn phys_mem_offset() -> u64 {
    PHYS_MEM_OFFSET
        .lock()
        .expect("Virtual memory not initialized")
}

/// Create an `OffsetPageTable` from the active page table.
///
/// # Safety
///
/// The caller must ensure that:
/// - The complete physical memory is mapped at `phys_mem_offset`
/// - This function is only called once to avoid aliasing `&mut` references
pub unsafe fn active_page_table() -> OffsetPageTable<'static> {
    let (level_4_table_frame, _) = Cr3::read();
    let phys = level_4_table_frame.start_address();
    let virt = VirtAddr::new(phys.as_u64() + phys_mem_offset());
    let page_table = unsafe { &mut *virt.as_mut_ptr() };
    unsafe { OffsetPageTable::new(page_table, VirtAddr::new(phys_mem_offset())) }
}

/// Map a virtual page to a physical frame.
///
/// # Safety
///
/// The caller must ensure that:
/// - The physical frame is valid and not already mapped elsewhere
///   (unless intentionally sharing)
/// - The virtual page is not already mapped
pub unsafe fn map_page(
    page: Page<Size4KiB>,
    frame: PhysFrame<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let mut mapper = unsafe { active_page_table() };
    let mut frame_alloc = BootInfoFrameAllocator;

    // SAFETY: Caller guarantees the mapping is valid.
    unsafe {
        mapper
            .map_to(page, frame, flags, &mut frame_alloc)
            .map_err(|_| "Failed to map page")?
            .flush();
        crate::arch::x86_64::smp::shootdown_tlb();
    }

    Ok(())
}

/// Create a new user page table by allocating a PML4 frame and copying
/// the kernel mappings (entries 256 to 511) from the active PML4 table.
pub fn create_user_page_table() -> Result<u64, &'static str> {
    let pml4_phys = super::physical::allocate_frame()
        .ok_or("Failed to allocate physical frame for user PML4")?;

    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let new_pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };

    // Clear the table first to avoid any garbage mappings
    new_pml4.zero();

    // Copy kernel mappings (entries 256 to 511) from the active PML4
    let (active_pml4_frame, _) = Cr3::read();
    let active_pml4_virt = VirtAddr::new(active_pml4_frame.start_address().as_u64() + phys_mem_offset());
    let active_pml4: &PageTable = unsafe { &*active_pml4_virt.as_ptr() };

    for i in 256..512 {
        new_pml4[i] = active_pml4[i].clone();
    }

    // Copy the physical memory mapping PML4 entry (which resides in the lower half)
    let pml4_index = (phys_mem_offset() >> 39) & 0x1FF;
    crate::kprintln!("[virtual] User PML4: cloning index {}, entry: {:?}", pml4_index, active_pml4[pml4_index as usize]);
    new_pml4[pml4_index as usize] = active_pml4[pml4_index as usize].clone();

    Ok(pml4_phys)
}

/// Clone a parent page table by allocating a new PML4 frame and copying
/// all 512 entries from the parent's PML4 table.
pub fn clone_parent_page_table(parent_pml4_phys: u64) -> Result<u64, &'static str> {
    let child_pml4_phys = super::physical::allocate_frame()
        .ok_or("Failed to allocate physical frame for child PML4")?;

    let child_pml4_virt = VirtAddr::new(child_pml4_phys + phys_mem_offset());
    let new_pml4: &mut PageTable = unsafe { &mut *child_pml4_virt.as_mut_ptr() };

    // Clear the table first to avoid any garbage mappings
    new_pml4.zero();

    // Copy all mappings (entries 0 to 511) from the parent PML4
    let parent_pml4_virt = VirtAddr::new(parent_pml4_phys + phys_mem_offset());
    let parent_pml4: &PageTable = unsafe { &*parent_pml4_virt.as_ptr() };

    for i in 0..512 {
        if i == 255 {
            // Do not clone parent's stack page table entry to avoid parent/child
            // sharing Level 3/2/1 page tables for the stack.
            // Fresh, independent page tables will be dynamically allocated
            // for the child's stack in sys_fork.
            continue;
        }
        new_pml4[i] = parent_pml4[i].clone();
    }

    Ok(child_pml4_phys)
}

/// Map a virtual page to a physical frame in a non-active PML4 page table.
///
/// # Safety
///
/// The caller must ensure that:
/// - The targeted PML4 physical address is valid.
/// - The physical frame is valid and not already mapped elsewhere.
/// - The virtual page is not already mapped.
pub unsafe fn map_user_page(
    pml4_phys: u64,
    page: Page<Size4KiB>,
    frame: PhysFrame<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };
    let mut mapper = unsafe { OffsetPageTable::new(pml4, VirtAddr::new(phys_mem_offset())) };
    let mut frame_alloc = BootInfoFrameAllocator;

    // SAFETY: The caller guarantees that the mapping is valid and safe.
    unsafe {
        mapper
            .map_to(page, frame, flags, &mut frame_alloc)
            .map_err(|_| "Failed to map user page")?
            .flush();
        crate::arch::x86_64::smp::shootdown_tlb();
    }

    Ok(())
}

/// Translate a virtual address to a physical address.
pub fn translate_addr(addr: VirtAddr) -> Option<PhysAddr> {
    let mapper = unsafe { active_page_table() };
    use x86_64::structures::paging::Translate;
    mapper.translate_addr(addr)
}

/// A frame allocator that wraps our physical frame allocator to implement
/// the `x86_64` crate's `FrameAllocator` trait.
struct BootInfoFrameAllocator;

unsafe impl X86FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        super::physical::allocate_frame()
            .map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

/// Unmap a virtual page from a targeted, non-active PML4 page table, and return the physical frame address.
///
/// # Safety
///
/// The caller must ensure that the targeted PML4 physical address is valid.
pub unsafe fn unmap_user_page(
    pml4_phys: u64,
    page: Page<Size4KiB>,
) -> Result<u64, &'static str> {
    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };
    let mut mapper = unsafe { OffsetPageTable::new(pml4, VirtAddr::new(phys_mem_offset())) };

    use x86_64::structures::paging::Mapper;
    match mapper.unmap(page) {
        Ok((frame, flush)) => {
            flush.flush();
            crate::arch::x86_64::smp::shootdown_tlb();
            Ok(frame.start_address().as_u64())
        }
        Err(_) => Err("Page not mapped"),
    }
}

