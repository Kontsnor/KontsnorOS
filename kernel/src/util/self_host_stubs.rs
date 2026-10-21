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

//! Freestanding memory and unwind stubs for self-hosting compilation
//! under `x86_64-unknown-linux-musl` with `-C link-self-contained=no`.

#[cfg(not(target_os = "none"))]
#[no_mangle]
pub extern "C" fn rust_eh_personality() {}

#[cfg(not(target_os = "none"))]
#[no_mangle]
pub extern "C" fn _Unwind_Resume() -> ! {
    loop {}
}

#[cfg(not(target_os = "none"))]
#[allow(suspicious_runtime_symbol_definitions)]
#[no_mangle]
/// # Safety
/// Caller must pass valid pointers for `dest` and `src` with at least `n` bytes.
pub unsafe extern "C" fn memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    // SAFETY: Delegating to core::ptr::copy_nonoverlapping with caller's validity contract.
    unsafe {
        core::ptr::copy_nonoverlapping(src, dest, n);
    }
    dest
}

#[cfg(not(target_os = "none"))]
#[allow(suspicious_runtime_symbol_definitions)]
#[no_mangle]
/// # Safety
/// Caller must pass a valid pointer `s` with at least `n` bytes.
pub unsafe extern "C" fn memset(s: *mut u8, c: i32, n: usize) -> *mut u8 {
    // SAFETY: Delegating to core::ptr::write_bytes with caller's validity contract.
    unsafe {
        core::ptr::write_bytes(s, c as u8, n);
    }
    s
}

#[cfg(not(target_os = "none"))]
#[allow(suspicious_runtime_symbol_definitions)]
#[no_mangle]
/// # Safety
/// Caller must pass valid pointers for `dest` and `src` with at least `n` bytes.
pub unsafe extern "C" fn memmove(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    // SAFETY: Delegating to core::ptr::copy with caller's validity contract.
    unsafe {
        core::ptr::copy(src, dest, n);
    }
    dest
}

#[cfg(not(target_os = "none"))]
#[allow(suspicious_runtime_symbol_definitions)]
#[no_mangle]
/// # Safety
/// Caller must pass valid pointers `s1` and `s2` with at least `n` bytes.
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    // SAFETY: Caller guarantees s1 and s2 are valid for n bytes.
    let slice1 = unsafe { core::slice::from_raw_parts(s1, n) };
    let slice2 = unsafe { core::slice::from_raw_parts(s2, n) };
    match slice1.cmp(slice2) {
        core::cmp::Ordering::Less => -1,
        core::cmp::Ordering::Equal => 0,
        core::cmp::Ordering::Greater => 1,
    }
}
