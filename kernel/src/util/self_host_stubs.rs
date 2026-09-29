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
#[no_mangle]
/// # Safety
/// Caller must pass valid pointers for `dest` and `src` with at least `n` bytes.
pub unsafe extern "C" fn memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    let mut i = 0;
    while i < n {
        // SAFETY: Pointer offsets up to n are valid per caller contract.
        unsafe {
            *dest.add(i) = *src.add(i);
        }
        i += 1;
    }
    dest
}

#[cfg(not(target_os = "none"))]
#[no_mangle]
/// # Safety
/// Caller must pass a valid pointer `s` with at least `n` bytes.
pub unsafe extern "C" fn memset(s: *mut u8, c: i32, n: usize) -> *mut u8 {
    let mut i = 0;
    while i < n {
        // SAFETY: Pointer offset up to n is valid per caller contract.
        unsafe {
            *s.add(i) = c as u8;
        }
        i += 1;
    }
    s
}

#[cfg(not(target_os = "none"))]
#[no_mangle]
/// # Safety
/// Caller must pass valid pointers for `dest` and `src` with at least `n` bytes.
pub unsafe extern "C" fn memmove(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if (dest as usize) < (src as usize) {
        // SAFETY: Delegating to memcpy with the same bounds contract.
        unsafe { memcpy(dest, src, n) }
    } else {
        let mut i = n;
        while i > 0 {
            i -= 1;
            // SAFETY: Decrementing within bounds [0..n).
            unsafe {
                *dest.add(i) = *src.add(i);
            }
        }
        dest
    }
}

#[cfg(not(target_os = "none"))]
#[no_mangle]
/// # Safety
/// Caller must pass valid pointers `s1` and `s2` with at least `n` bytes.
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut i = 0;
    while i < n {
        // SAFETY: Reading within bounds [0..n).
        let (a, b) = unsafe { (*s1.add(i), *s2.add(i)) };
        if a != b {
            return a as i32 - b as i32;
        }
        i += 1;
    }
    0
}
