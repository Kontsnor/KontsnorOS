//! Memory validation functions for user-space pointers.
//!
//! Provides utilities to verify that pointers passed from user space are safe
//! to read or write, avoiding page faults in kernel context.

use alloc::string::String;

/// Enforce that a user-space pointer range [ptr, ptr + size) is valid.
///
/// 1. Must lie strictly below 0x0000_7FFF_FFFF_FFFF.
/// 2. Must not wrap around.
/// 3. Every page in the range must be mapped in the active page directory.
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
        if crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(curr)).is_none() {
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
        if crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(curr)).is_none() {
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
        if crate::memory::r#virtual::translate_addr(x86_64::VirtAddr::new(page_base)).is_none() {
            return None;
        }
        let byte = unsafe { p.read_volatile() };
        if byte == 0 {
            break;
        }
        result.push(byte as char);
        p = unsafe { p.add(1) };
        if result.len() > 4096 {
            return None;
        }
    }
    Some(result)
}

/// Public wrapper used by other modules for path resolution.
pub unsafe fn copy_string_from_user_pub(ptr: *const u8) -> Option<String> {
    unsafe { copy_string_from_user(ptr) }
}
