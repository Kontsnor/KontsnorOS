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

/// Clone a parent page table by allocating a new PML4 frame and recursively
/// deep-cloning all user page table structures (PML4, PDPT, PD, PT) for user
/// space entries (0..256), while marking writable entries as Copy-on-Write (COW).
pub fn clone_parent_page_table(parent_pml4_phys: u64) -> Result<u64, &'static str> {
    // 1. Create a clean child PML4 pre-populated with kernel entries
    let child_pml4_phys = create_user_page_table()?;

    let parent_pml4_virt = VirtAddr::new(parent_pml4_phys + phys_mem_offset());
    let parent_pml4: &PageTable = unsafe { &*parent_pml4_virt.as_ptr() };

    let child_pml4_virt = VirtAddr::new(child_pml4_phys + phys_mem_offset());
    let child_pml4: &mut PageTable = unsafe { &mut *child_pml4_virt.as_mut_ptr() };

    let pml4_index = (phys_mem_offset() >> 39) & 0x1FF;

    // 2. Deep-clone only user-space PML4 entries (0..256)
    for i in 0..256 {
        // Skip the physical memory offset entry if it lies in user space index region
        if i == pml4_index as usize {
            continue;
        }

        let parent_pml4_entry = &parent_pml4[i];
        if parent_pml4_entry.is_unused() {
            continue;
        }

        // Allocate a new child PDPT (Level 3)
        let child_pdpt_phys = super::physical::allocate_frame()
            .ok_or("Failed to allocate child PDPT frame")?;
        let child_pdpt_virt = VirtAddr::new(child_pdpt_phys + phys_mem_offset());
        let child_pdpt: &mut PageTable = unsafe { &mut *child_pdpt_virt.as_mut_ptr() };
        child_pdpt.zero();

        // Copy parent PML4 entry flags
        child_pml4[i].set_addr(PhysAddr::new(child_pdpt_phys), parent_pml4_entry.flags());

        // Map parent PDPT
        let parent_pdpt_phys = parent_pml4_entry.frame().map_err(|_| "Invalid frame in parent PML4")?.start_address().as_u64();
        let parent_pdpt_virt = VirtAddr::new(parent_pdpt_phys + phys_mem_offset());
        let parent_pdpt: &PageTable = unsafe { &*parent_pdpt_virt.as_ptr() };

        for j in 0..512 {
            let parent_pdpt_entry = &parent_pdpt[j];
            if parent_pdpt_entry.is_unused() {
                continue;
            }

            // Allocate a new child PD (Level 2)
            let child_pd_phys = super::physical::allocate_frame()
                .ok_or("Failed to allocate child PD frame")?;
            let child_pd_virt = VirtAddr::new(child_pd_phys + phys_mem_offset());
            let child_pd: &mut PageTable = unsafe { &mut *child_pd_virt.as_mut_ptr() };
            child_pd.zero();

            // Copy PDPT flags
            child_pdpt[j].set_addr(PhysAddr::new(child_pd_phys), parent_pdpt_entry.flags());

            // Map parent PD
            let parent_pd_phys = parent_pdpt_entry.frame().map_err(|_| "Invalid frame in parent PDPT")?.start_address().as_u64();
            let parent_pd_virt = VirtAddr::new(parent_pd_phys + phys_mem_offset());
            let parent_pd: &PageTable = unsafe { &*parent_pd_virt.as_ptr() };

            for k in 0..512 {
                let parent_pd_entry = &parent_pd[k];
                if parent_pd_entry.is_unused() {
                    continue;
                }

                // Allocate a new child PT (Level 1)
                let child_pt_phys = super::physical::allocate_frame()
                    .ok_or("Failed to allocate child PT frame")?;
                let child_pt_virt = VirtAddr::new(child_pt_phys + phys_mem_offset());
                let child_pt: &mut PageTable = unsafe { &mut *child_pt_virt.as_mut_ptr() };
                child_pt.zero();

                // Copy PD flags
                child_pd[k].set_addr(PhysAddr::new(child_pt_phys), parent_pd_entry.flags());

                // Map parent's PT (get mutable reference so we can write protect entries too)
                let parent_pt_phys = parent_pd_entry.frame().map_err(|_| "Invalid frame in parent PD")?.start_address().as_u64();
                let parent_pt_virt = VirtAddr::new(parent_pt_phys + phys_mem_offset());
                let parent_pt_mut: &mut PageTable = unsafe { &mut *parent_pt_virt.as_mut_ptr() };

                for l in 0..512 {
                    let parent_pt_entry = &mut parent_pt_mut[l];
                    if parent_pt_entry.is_unused() {
                        continue;
                    }

                    let frame = parent_pt_entry.frame().map_err(|_| "Invalid frame in parent PT")?;
                    let phys_addr = frame.start_address().as_u64();

                    let mut flags = parent_pt_entry.flags();
                    let is_writable = flags.contains(PageTableFlags::WRITABLE);
                    let is_cow = flags.contains(PageTableFlags::BIT_9);

                    if is_writable || is_cow {
                        // Mark as Copy-on-Write: clear WRITABLE, add BIT_9
                        flags.remove(PageTableFlags::WRITABLE);
                        flags.insert(PageTableFlags::BIT_9);

                        parent_pt_entry.set_addr(PhysAddr::new(phys_addr), flags);

                        // Increment physical page frame reference count
                        super::physical::increment_ref(phys_addr);
                    }

                    child_pt[l].set_addr(PhysAddr::new(phys_addr), flags);
                }
            }
        }
    }

    // Broadcast TLB shootdown to notify other CPU cores
    crate::arch::x86_64::smp::shootdown_tlb();

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

