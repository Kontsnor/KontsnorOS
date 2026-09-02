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

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator as X86FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags,
    PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

/// The physical memory offset provided by the bootloader.
///
/// All physical memory is mapped starting at this virtual address,
/// allowing the kernel to access any physical address by adding this offset.
static PHYS_MEM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// The kernel's active PML4 page table root physical address.
static KERNEL_PML4_PHYS: AtomicU64 = AtomicU64::new(0);

/// Global lock to serialize page table modifications across SMP cores.
pub static PAGE_TABLE_LOCK: spin::Mutex<()> = spin::Mutex::new(());

/// Initialize the virtual memory manager.
///
/// `phys_mem_offset` is the virtual address where physical memory is
/// mapped by the bootloader.
pub fn init(phys_mem_offset: u64) {
    let pml4_index = (phys_mem_offset >> 39) & 0x1FF;
    if pml4_index < 256 {
        panic!("Physical memory mapping offset {:#x} resides in lower half of PML4 (index {}), overlapping user space!", phys_mem_offset, pml4_index);
    }
    PHYS_MEM_OFFSET.store(phys_mem_offset, Ordering::SeqCst);
    let (level_4_table_frame, _) = Cr3::read();
    KERNEL_PML4_PHYS.store(
        level_4_table_frame.start_address().as_u64(),
        Ordering::SeqCst,
    );
}

/// Get the physical memory offset.
pub fn phys_mem_offset() -> u64 {
    let val = PHYS_MEM_OFFSET.load(Ordering::SeqCst);
    if val == 0 {
        panic!("Virtual memory not initialized");
    }
    val
}

/// Get the kernel PML4 physical address.
pub fn kernel_pml4_phys() -> u64 {
    let val = KERNEL_PML4_PHYS.load(Ordering::SeqCst);
    if val == 0 {
        panic!("Virtual memory not initialized");
    }
    val
}

/// Dump page table entries for debugging.
pub fn debug_dump_mapping(vaddr: u64) {
    let (pml4_frame, _) = Cr3::read();
    let pml4_phys = pml4_frame.start_address().as_u64();
    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let pml4: &PageTable = unsafe { &*pml4_virt.as_ptr() };

    let addr = VirtAddr::new(vaddr);
    let p4 = addr.p4_index();
    let p3 = addr.p3_index();
    let p2 = addr.p2_index();
    let p1 = addr.p1_index();

    crate::kprintln!("[debug_map] Vaddr: {:#x} CR3: {:#x}", vaddr, pml4_phys);
    let pml4_entry = &pml4[p4];
    crate::kprintln!("  PML4[{:?}] entry: {:?}", p4, pml4_entry);
    if pml4_entry.is_unused() {
        return;
    }

    if let Ok(pdpt_frame) = pml4_entry.frame() {
        let pdpt_phys = pdpt_frame.start_address().as_u64();
        let pdpt_virt = VirtAddr::new(pdpt_phys + phys_mem_offset());
        let pdpt: &PageTable = unsafe { &*pdpt_virt.as_ptr() };
        let pdpt_entry = &pdpt[p3];
        crate::kprintln!("  PDPT[{:?}] entry: {:?}", p3, pdpt_entry);
        if pdpt_entry.is_unused() {
            return;
        }

        if let Ok(pd_frame) = pdpt_entry.frame() {
            let pd_phys = pd_frame.start_address().as_u64();
            let pd_virt = VirtAddr::new(pd_phys + phys_mem_offset());
            let pd: &PageTable = unsafe { &*pd_virt.as_ptr() };
            let pd_entry = &pd[p2];
            crate::kprintln!("  PD[{:?}] entry: {:?}", p2, pd_entry);
            if pd_entry.is_unused() {
                return;
            }

            if let Ok(pt_frame) = pd_entry.frame() {
                let pt_phys = pt_frame.start_address().as_u64();
                let pt_virt = VirtAddr::new(pt_phys + phys_mem_offset());
                let pt: &PageTable = unsafe { &*pt_virt.as_ptr() };
                let pt_entry = &pt[p1];
                crate::kprintln!("  PT[{:?}] entry: {:?}", p1, pt_entry);
            }
        }
    }
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
    let _lock = PAGE_TABLE_LOCK.lock();
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
    let _lock = PAGE_TABLE_LOCK.lock();
    create_user_page_table_unlocked()
}

fn create_user_page_table_unlocked() -> Result<u64, &'static str> {
    let pml4_phys = super::physical::allocate_frame()
        .ok_or("Failed to allocate physical frame for user PML4")?;

    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let new_pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };

    // Clear the table first to avoid any garbage mappings
    new_pml4.zero();

    // Copy kernel mappings (entries 256 to 511) from the active PML4
    let (active_pml4_frame, _) = Cr3::read();
    let active_pml4_virt =
        VirtAddr::new(active_pml4_frame.start_address().as_u64() + phys_mem_offset());
    let active_pml4: &PageTable = unsafe { &*active_pml4_virt.as_ptr() };

    for i in 256..512 {
        new_pml4[i] = active_pml4[i].clone();
    }

    // Clone kernel code / data (PML4 index 1: 0x80_0000_0000)
    new_pml4[1] = active_pml4[1].clone();

    // Copy the physical memory mapping PML4 entry (which resides in the lower half)
    let pml4_index = (phys_mem_offset() >> 39) & 0x1FF;
    if pml4_index < 256 && pml4_index != 1 {
        return Err("Physical memory mapping overlaps user space");
    }
    crate::kprintln!(
        "[virtual] User PML4: cloning index {}, entry: {:?}",
        pml4_index,
        active_pml4[pml4_index as usize]
    );
    new_pml4[pml4_index as usize] = active_pml4[pml4_index as usize].clone();

    Ok(pml4_phys)
}

/// Clone a parent page table by allocating a new PML4 frame and recursively
/// deep-cloning all user page table structures (PML4, PDPT, PD, PT) for user
/// space entries (0..256), while marking writable entries as Copy-on-Write (COW).
pub fn clone_parent_page_table(
    parent_pml4_phys: u64,
    mmap_regions: &[crate::process::task::MappedRegion],
) -> Result<u64, &'static str> {
    let _lock = PAGE_TABLE_LOCK.lock();
    // 1. Create a clean child PML4 pre-populated with kernel entries
    let child_pml4_phys = create_user_page_table_unlocked()?;

    let parent_pml4_virt = VirtAddr::new(parent_pml4_phys + phys_mem_offset());
    let parent_pml4: &PageTable = unsafe { &*parent_pml4_virt.as_ptr() };

    let child_pml4_virt = VirtAddr::new(child_pml4_phys + phys_mem_offset());
    let child_pml4: &mut PageTable = unsafe { &mut *child_pml4_virt.as_mut_ptr() };

    let pml4_index = (phys_mem_offset() >> 39) & 0x1FF;

    // 2. Deep-clone only user-space PML4 entries (0..256), skipping kernel PML4 index 1
    for i in 0..256 {
        if i == 1 || i == pml4_index as usize {
            continue;
        }

        let parent_pml4_entry = &parent_pml4[i];
        if parent_pml4_entry.is_unused() {
            continue;
        }

        // Allocate a new child PDPT (Level 3)
        let child_pdpt_phys =
            super::physical::allocate_frame().ok_or("Failed to allocate child PDPT frame")?;
        let child_pdpt_virt = VirtAddr::new(child_pdpt_phys + phys_mem_offset());
        let child_pdpt: &mut PageTable = unsafe { &mut *child_pdpt_virt.as_mut_ptr() };
        child_pdpt.zero();

        // Copy parent PML4 entry flags
        child_pml4[i].set_addr(PhysAddr::new(child_pdpt_phys), parent_pml4_entry.flags());

        // Map parent PDPT
        let parent_pdpt_phys = parent_pml4_entry
            .frame()
            .map_err(|_| "Invalid frame in parent PML4")?
            .start_address()
            .as_u64();
        let parent_pdpt_virt = VirtAddr::new(parent_pdpt_phys + phys_mem_offset());
        let parent_pdpt: &PageTable = unsafe { &*parent_pdpt_virt.as_ptr() };

        for j in 0..512 {
            let parent_pdpt_entry = &parent_pdpt[j];
            if parent_pdpt_entry.is_unused() {
                continue;
            }

            // Allocate a new child PD (Level 2)
            let child_pd_phys =
                super::physical::allocate_frame().ok_or("Failed to allocate child PD frame")?;
            let child_pd_virt = VirtAddr::new(child_pd_phys + phys_mem_offset());
            let child_pd: &mut PageTable = unsafe { &mut *child_pd_virt.as_mut_ptr() };
            child_pd.zero();

            // Copy PDPT flags
            child_pdpt[j].set_addr(PhysAddr::new(child_pd_phys), parent_pdpt_entry.flags());

            // Map parent PD
            let parent_pd_phys = parent_pdpt_entry
                .frame()
                .map_err(|_| "Invalid frame in parent PDPT")?
                .start_address()
                .as_u64();
            let parent_pd_virt = VirtAddr::new(parent_pd_phys + phys_mem_offset());
            let parent_pd: &PageTable = unsafe { &*parent_pd_virt.as_ptr() };

            for k in 0..512 {
                let parent_pd_entry = &parent_pd[k];
                if parent_pd_entry.is_unused() {
                    continue;
                }

                // Allocate a new child PT (Level 1)
                let child_pt_phys =
                    super::physical::allocate_frame().ok_or("Failed to allocate child PT frame")?;
                let child_pt_virt = VirtAddr::new(child_pt_phys + phys_mem_offset());
                let child_pt: &mut PageTable = unsafe { &mut *child_pt_virt.as_mut_ptr() };
                child_pt.zero();

                // Copy PD flags
                child_pd[k].set_addr(PhysAddr::new(child_pt_phys), parent_pd_entry.flags());

                // Map parent's PT (get mutable reference so we can write protect entries too)
                let parent_pt_phys = parent_pd_entry
                    .frame()
                    .map_err(|_| "Invalid frame in parent PD")?
                    .start_address()
                    .as_u64();
                let parent_pt_virt = VirtAddr::new(parent_pt_phys + phys_mem_offset());
                let parent_pt_mut: &mut PageTable = unsafe { &mut *parent_pt_virt.as_mut_ptr() };

                for l in 0..512 {
                    let parent_pt_entry = &mut parent_pt_mut[l];
                    if parent_pt_entry.is_unused() {
                        continue;
                    }

                    let vaddr = ((i as u64) << 39)
                        | ((j as u64) << 30)
                        | ((k as u64) << 21)
                        | ((l as u64) << 12);

                    if parent_pml4_phys == kernel_pml4_phys()
                        && !mmap_regions
                            .iter()
                            .any(|r| vaddr >= r.start && vaddr < r.start + r.len as u64)
                    {
                        continue;
                    }

                    let frame = parent_pt_entry
                        .frame()
                        .map_err(|_| "Invalid frame in parent PT")?;
                    let phys_addr = frame.start_address().as_u64();

                    let mut flags = parent_pt_entry.flags();
                    let is_writable = flags.contains(PageTableFlags::WRITABLE);
                    let is_cow = flags.contains(PageTableFlags::BIT_9);

                    let is_shared_mapping = mmap_regions
                        .iter()
                        .any(|r| vaddr >= r.start && vaddr < r.start + r.len as u64 && r.is_shared);

                    if (is_writable || is_cow) && !is_shared_mapping {
                        // Mark as Copy-on-Write: clear WRITABLE, add BIT_9
                        flags.remove(PageTableFlags::WRITABLE);
                        flags.insert(PageTableFlags::BIT_9);

                        parent_pt_entry.set_addr(PhysAddr::new(phys_addr), flags);
                        child_pt[l].set_addr(PhysAddr::new(phys_addr), flags);
                    } else {
                        // Read-only or shared mapping: direct map with same flags
                        child_pt[l].set_addr(PhysAddr::new(phys_addr), flags);
                    }

                    // Increment physical page frame reference count for ALL shared user pages
                    super::physical::increment_ref(phys_addr);
                }
            }
        }
    }

    // Flush local CPU TLB since we modified parent page write flags to COW
    x86_64::instructions::tlb::flush_all();

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
    let _lock = PAGE_TABLE_LOCK.lock();
    unsafe {
        map_user_page_no_shootdown(pml4_phys, page, frame, flags)?;
        crate::arch::x86_64::smp::shootdown_tlb();
    }
    Ok(())
}

/// Map a virtual page to a physical frame in a non-active PML4 page table without triggering TLB shootdown.
///
/// # Safety
///
/// The caller must ensure that:
/// - The targeted PML4 physical address is valid.
/// - The physical frame is valid and not already mapped elsewhere.
/// - The virtual page is not already mapped.
pub unsafe fn map_user_page_no_shootdown(
    pml4_phys: u64,
    page: Page<Size4KiB>,
    frame: PhysFrame<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let _lock = PAGE_TABLE_LOCK.lock();
    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };
    let mut mapper = unsafe { OffsetPageTable::new(pml4, VirtAddr::new(phys_mem_offset())) };
    let mut frame_alloc = BootInfoFrameAllocator;

    // SAFETY: The caller guarantees that the mapping is valid and safe.
    unsafe {
        match mapper.map_to(page, frame, flags, &mut frame_alloc) {
            Ok(flush) => {
                flush.flush();
                ensure_directory_permissions_unlocked(pml4_phys, page.start_address());
                Ok(())
            }
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                ensure_directory_permissions_unlocked(pml4_phys, page.start_address());
                Err("PageAlreadyMapped")
            }
            Err(_) => Err("Failed to map user page"),
        }
    }
}

/// Ensure that the intermediate page table directories (PML4, PDPT, PD) for the given virtual address
/// have USER_ACCESSIBLE and WRITABLE flags set.
pub unsafe fn ensure_directory_permissions(pml4_phys: u64, addr: VirtAddr) {
    let _lock = PAGE_TABLE_LOCK.lock();
    unsafe {
        ensure_directory_permissions_unlocked(pml4_phys, addr);
    }
}

pub(crate) unsafe fn ensure_directory_permissions_unlocked(pml4_phys: u64, addr: VirtAddr) {
    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };

    let p4_idx = addr.p4_index();
    let p3_idx = addr.p3_index();
    let p2_idx = addr.p2_index();

    let pml4_entry = &mut pml4[p4_idx];
    if !pml4_entry.is_unused() {
        let mut flags = pml4_entry.flags();
        if !flags.contains(PageTableFlags::WRITABLE)
            || !flags.contains(PageTableFlags::USER_ACCESSIBLE)
        {
            flags |= PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
            pml4_entry.set_addr(pml4_entry.addr(), flags);
        }
        if let Ok(pdpt_frame) = pml4_entry.frame() {
            let pdpt_phys = pdpt_frame.start_address().as_u64();
            let pdpt_virt = VirtAddr::new(pdpt_phys + phys_mem_offset());
            let pdpt: &mut PageTable = unsafe { &mut *pdpt_virt.as_mut_ptr() };

            let pdpt_entry = &mut pdpt[p3_idx];
            if !pdpt_entry.is_unused() {
                let mut flags = pdpt_entry.flags();
                if !flags.contains(PageTableFlags::WRITABLE)
                    || !flags.contains(PageTableFlags::USER_ACCESSIBLE)
                {
                    flags |= PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
                    pdpt_entry.set_addr(pdpt_entry.addr(), flags);
                }
                if let Ok(pd_frame) = pdpt_entry.frame() {
                    let pd_phys = pd_frame.start_address().as_u64();
                    let pd_virt = VirtAddr::new(pd_phys + phys_mem_offset());
                    let pd: &mut PageTable = unsafe { &mut *pd_virt.as_mut_ptr() };

                    let pd_entry = &mut pd[p2_idx];
                    if !pd_entry.is_unused() {
                        let mut flags = pd_entry.flags();
                        if !flags.contains(PageTableFlags::WRITABLE)
                            || !flags.contains(PageTableFlags::USER_ACCESSIBLE)
                        {
                            flags |= PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
                            pd_entry.set_addr(pd_entry.addr(), flags);
                        }
                    }
                }
            }
        }
    }
}

/// Translate a virtual address to a physical address.
pub fn translate_addr(addr: VirtAddr) -> Option<PhysAddr> {
    let mapper = unsafe { active_page_table() };
    use x86_64::structures::paging::Translate;
    mapper.translate_addr(addr)
}

/// Translate a virtual page in a targeted PML4 page table.
pub fn translate_page_in_table(
    pml4_phys: u64,
    page: Page<Size4KiB>,
) -> Option<PhysFrame<Size4KiB>> {
    let _lock = PAGE_TABLE_LOCK.lock();
    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };
    let mapper = unsafe { OffsetPageTable::new(pml4, VirtAddr::new(phys_mem_offset())) };
    use x86_64::structures::paging::Translate;
    mapper.translate_page(page).ok()
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
pub unsafe fn unmap_user_page(pml4_phys: u64, page: Page<Size4KiB>) -> Result<u64, &'static str> {
    unsafe {
        let phys = unmap_user_page_no_shootdown(pml4_phys, page)?;
        crate::arch::x86_64::smp::shootdown_tlb();
        Ok(phys)
    }
}

/// Unmap a virtual page from a targeted, non-active PML4 page table without triggering TLB shootdown.
///
/// # Safety
///
/// The caller must ensure that the targeted PML4 physical address is valid.
pub unsafe fn unmap_user_page_no_shootdown(
    pml4_phys: u64,
    page: Page<Size4KiB>,
) -> Result<u64, &'static str> {
    let _lock = PAGE_TABLE_LOCK.lock();
    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };
    let mut mapper = unsafe { OffsetPageTable::new(pml4, VirtAddr::new(phys_mem_offset())) };

    use x86_64::structures::paging::Mapper;
    match mapper.unmap(page) {
        Ok((frame, flush)) => {
            flush.flush();
            Ok(frame.start_address().as_u64())
        }
        Err(_) => Err("Page not mapped"),
    }
}

/// Update flags of a virtual page in a targeted, non-active PML4 page table.
///
/// # Safety
///
/// The caller must ensure that the targeted PML4 physical address is valid.
pub unsafe fn update_user_page_flags(
    pml4_phys: u64,
    page: Page<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    unsafe {
        update_user_page_flags_no_shootdown(pml4_phys, page, flags)?;
        crate::arch::x86_64::smp::shootdown_tlb();
    }
    Ok(())
}

/// Update flags of a virtual page in a targeted, non-active PML4 page table without triggering TLB shootdown.
///
/// # Safety
///
/// The caller must ensure that the targeted PML4 physical address is valid.
pub unsafe fn update_user_page_flags_no_shootdown(
    pml4_phys: u64,
    page: Page<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let _lock = PAGE_TABLE_LOCK.lock();
    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let pml4: &mut PageTable = unsafe { &mut *pml4_virt.as_mut_ptr() };
    let mut mapper = unsafe { OffsetPageTable::new(pml4, VirtAddr::new(phys_mem_offset())) };

    use x86_64::structures::paging::Mapper;
    match unsafe { mapper.update_flags(page, flags) } {
        Ok(flush) => {
            flush.flush();
            Ok(())
        }
        Err(_) => Err("Failed to update page flags"),
    }
}

/// Recursively unmaps and deallocates user space page tables and leaf pages.
pub fn free_user_page_table(pml4_phys: u64) -> Result<(), &'static str> {
    let _lock = PAGE_TABLE_LOCK.lock();
    let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset());
    let pml4: &PageTable = unsafe { &*pml4_virt.as_ptr() };
    let pml4_index = (phys_mem_offset() >> 39) & 0x1FF;
    let mut freed_pages = 0usize;

    for i in 0..256 {
        if i == 1 || i == pml4_index as usize {
            continue;
        }
        let pml4_entry = &pml4[i];
        if pml4_entry.is_unused() {
            continue;
        }

        let pdpt_phys = pml4_entry
            .frame()
            .map_err(|_| "Invalid frame in PML4")?
            .start_address()
            .as_u64();
        let pdpt_virt = VirtAddr::new(pdpt_phys + phys_mem_offset());
        let pdpt: &PageTable = unsafe { &*pdpt_virt.as_ptr() };

        for j in 0..512 {
            let pdpt_entry = &pdpt[j];
            if pdpt_entry.is_unused() {
                continue;
            }

            let pd_phys = pdpt_entry
                .frame()
                .map_err(|_| "Invalid frame in PDPT")?
                .start_address()
                .as_u64();
            let pd_virt = VirtAddr::new(pd_phys + phys_mem_offset());
            let pd: &PageTable = unsafe { &*pd_virt.as_ptr() };

            for k in 0..512 {
                let pd_entry = &pd[k];
                if pd_entry.is_unused() {
                    continue;
                }

                let pt_phys = pd_entry
                    .frame()
                    .map_err(|_| "Invalid frame in PD")?
                    .start_address()
                    .as_u64();
                let pt_virt = VirtAddr::new(pt_phys + phys_mem_offset());
                let pt: &PageTable = unsafe { &*pt_virt.as_ptr() };

                for l in 0..512 {
                    let pt_entry = &pt[l];
                    if pt_entry.is_unused() {
                        continue;
                    }

                    let leaf_phys = pt_entry
                        .frame()
                        .map_err(|_| "Invalid frame in PT")?
                        .start_address()
                        .as_u64();
                    super::physical::deallocate_frame(leaf_phys);
                    freed_pages += 1;
                }

                super::physical::deallocate_frame(pt_phys);
            }

            super::physical::deallocate_frame(pd_phys);
        }

        super::physical::deallocate_frame(pdpt_phys);
    }

    super::physical::deallocate_frame(pml4_phys);

    crate::kprintln!(
        "[virtual] free_user_page_table({:#x}): freed {} physical pages",
        pml4_phys,
        freed_pages
    );

    // Broadcast TLB shootdown to notify other CPU cores
    crate::arch::x86_64::smp::shootdown_tlb();

    Ok(())
}
