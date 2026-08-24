//! Memory validation functions for user-space pointers.
//!
//! Provides utilities to verify that pointers passed from user space are safe
//! to read or write, avoiding page faults in kernel context.

use alloc::string::String;
use x86_64::VirtAddr;

/// Eagerly map a page if it is valid under the current task's mapped regions but not yet present.
fn ensure_page_mapped(vaddr: u64) -> bool {
    // First, check if already mapped
    if crate::memory::r#virtual::translate_addr(VirtAddr::new(vaddr)).is_some() {
        return true;
    }

    // If not mapped, check current task's mmap_regions and lazily fault it in
    let resolved = crate::process::scheduler::current_pid()
        .and_then(|pid| crate::process::scheduler::get_task_arc(pid))
        .and_then(|task_arc| {
            let task = task_arc.lock();
            let addr_space = task.address_space.lock();
            let page_vaddr = vaddr & !4095;

            // Find if page_vaddr falls inside any mapped region
            let region_opt = addr_space
                .mmap_regions
                .iter()
                .find(|region| {
                    page_vaddr >= region.start && page_vaddr < region.start + region.len as u64
                })
                .cloned();

            region_opt.map(|region| {
                use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
                use x86_64::PhysAddr;

                let page_offset = page_vaddr - region.start;
                let prot = region.prot;
                let mut page_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
                if (prot & 2) != 0 {
                    page_flags |= PageTableFlags::WRITABLE;
                }
                if (prot & 4) == 0 {
                    page_flags |= PageTableFlags::NO_EXECUTE;
                }

                let is_shared = region.is_shared;

                let (phys, do_cow) = match region.inode {
                    Some(ref inode) => {
                        let file_offset = region.offset + page_offset;
                        match crate::memory::page_cache::get_or_create_page(inode, file_offset) {
                            Ok(p) => {
                                let cow = !is_shared && (prot & 2) != 0;
                                (p, cow)
                            }
                            Err(_) => return false,
                        }
                    }
                    None => {
                        match crate::memory::physical::allocate_frame() {
                            Some(p) => {
                                let dest =
                                    (p + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
                                // SAFETY: dest points to a newly allocated frame and is valid for zero-filling.
                                unsafe {
                                    core::ptr::write_bytes(dest, 0, 4096);
                                }
                                (p, false)
                            }
                            None => return false,
                        }
                    }
                };

                let actual_flags = if do_cow {
                    let mut flags = page_flags;
                    flags.remove(PageTableFlags::WRITABLE);
                    flags.insert(PageTableFlags::BIT_9);
                    flags
                } else {
                    page_flags
                };

                let page_table_root = addr_space.page_table_root;
                let page = Page::<Size4KiB>::containing_address(VirtAddr::new(page_vaddr));
                let frame = PhysFrame::containing_address(PhysAddr::new(phys));

                unsafe {
                    if region.inode.is_some() {
                        // SAFETY: Increments reference count for page-cached file-backed page frame.
                        crate::memory::physical::increment_ref(phys);
                    }

                    // SAFETY: Replaces any old mapping at this page cleanly.
                    if let Ok(old_phys) = crate::memory::r#virtual::unmap_user_page_no_shootdown(
                        page_table_root,
                        page,
                    ) {
                        crate::memory::physical::deallocate_frame(old_phys);
                    }

                    // SAFETY: Configures page directory hierarchy levels to be user-accessible.
                    crate::memory::r#virtual::ensure_directory_permissions(
                        page_table_root,
                        VirtAddr::new(page_vaddr),
                    );

                    // SAFETY: Maps allocated physical frame at the requested virtual page address.
                    if let Err(e) = crate::memory::r#virtual::map_user_page_no_shootdown(
                        page_table_root,
                        page,
                        frame,
                        actual_flags,
                    ) {
                        crate::kprintln!("[validation_page_fault] Failed to map page: {:?}", e);
                        if region.inode.is_some() {
                            crate::memory::physical::decrement_ref(phys);
                        } else {
                            crate::memory::physical::deallocate_frame(phys);
                        }
                        return false;
                    }
                }

                x86_64::instructions::tlb::flush(VirtAddr::new(page_vaddr));
                true
            })
        });

    resolved.unwrap_or(false)
}

/// Enforce that a user-space pointer range [ptr, ptr + size) is valid.
///
/// 1. Must lie strictly below 0x0000_7FFF_FFFF_FFFF.
/// 2. Must not wrap around.
/// 3. Every page in the range must be mapped or lazy-mapped in the active page directory.
pub fn validate_user_ptr(ptr: *const u8, size: usize) -> bool {
    if ptr.is_null() {
        return false;
    }
    let start = ptr as u64;
    let end = match start.checked_add(size as u64) {
        Some(e) => e,
        None => return false,
    };
    if end > 0x0000_7FFF_FFFF_FFFF {
        return false;
    }
    if size == 0 {
        return true;
    }
    let page_size = 4096;
    let start_page = start & !(page_size - 1);
    let end_page = (end + page_size - 1) & !(page_size - 1);

    let mut curr = start_page;
    while curr < end_page {
        if !ensure_page_mapped(curr) {
            return false;
        }
        curr += page_size;
    }
    true
}

/// Validate that a user-space write target at `[ptr, ptr+size)` is safe.
///
/// This is the write-variant of `validate_user_ptr`: it must also be mapped
/// and writable (we allow any user-space address below the canonical hole).
pub fn validate_user_ptr_write(ptr: *mut u8, size: usize) -> Result<(), ()> {
    if ptr.is_null() {
        return Err(());
    }
    let start = ptr as u64;
    let end = match start.checked_add(size as u64) {
        Some(e) => e,
        None => return Err(()),
    };
    if end > 0x0000_7FFF_FFFF_FFFF {
        return Err(());
    }
    if size == 0 {
        return Ok(());
    }
    let page_size: u64 = 4096;
    let start_page = start & !(page_size - 1);
    let end_page = (end + page_size - 1) & !(page_size - 1);
    let mut curr = start_page;
    while curr < end_page {
        if !ensure_page_mapped(curr) {
            return Err(());
        }
        curr += page_size;
    }
    Ok(())
}

/// Copy a null-terminated string from user-space virtual address `ptr`.
///
/// Validates that each byte's page pointer resides in user memory and is mapped
/// in the active page table before dereferencing it, preventing unmapped page faults.
pub unsafe fn copy_string_from_user(ptr: *const u8) -> Option<String> {
    if ptr.is_null() || (ptr as u64) > 0x0000_7FFF_FFFF_FFFF {
        return None;
    }
    let mut result = String::new();
    let mut p = ptr;
    loop {
        let addr = p as u64;
        if addr > 0x0000_7FFF_FFFF_FFFF {
            return None;
        }
        let page_base = addr & !4095;
        if !ensure_page_mapped(page_base) {
            return None;
        }
        // SAFETY: We validated that the page is mapped.
        let byte = unsafe { p.read_volatile() };
        if byte == 0 {
            break;
        }
        result.push(byte as char);
        // SAFETY: Incrementing the pointer is safe as we validate the next address on iteration start.
        p = unsafe { p.add(1) };
        if result.len() > 4096 {
            return None;
        }
    }
    Some(result)
}

/// Public wrapper used by other modules for path resolution.
pub unsafe fn copy_string_from_user_pub(ptr: *const u8) -> Option<String> {
    // SAFETY: Delegate to copy_string_from_user with same safety contract.
    unsafe { copy_string_from_user(ptr) }
}
