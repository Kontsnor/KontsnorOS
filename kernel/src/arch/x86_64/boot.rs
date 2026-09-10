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

//! Boot sequence for x86_64.
//!
//! This module handles the early boot process after the bootloader
//! hands control to the kernel. It coordinates the initialization
//! of all architecture-specific components.
//!
//! The boot sequence is:
//! 1. Serial port initialization (for logging)
//! 2. GDT setup (memory segmentation)
//! 3. IDT setup (interrupt handling)
//! 4. PIC initialization (hardware interrupts)
//! 5. Memory subsystem initialization
//! 6. Enable interrupts

/// Initialize all x86_64 architecture components.
///
/// This is called from `kernel_main` and sets up all hardware-specific
/// components in the correct order.
pub fn init() {
    super::serial::init();
    super::gdt::init();
    super::interrupts::init_idt();
    super::interrupts::init_pics();

    // Enable SSE support (required for user-space programs compiled with SSE)
    unsafe {
        enable_sse();
        enable_fsgsbase();
    }
}

pub unsafe fn enable_sse() {
    use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};

    let mut cr4 = Cr4::read();
    cr4.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE);
    unsafe {
        Cr4::write(cr4);
    }

    let mut cr0 = Cr0::read();
    cr0.remove(Cr0Flags::EMULATE_COPROCESSOR);
    cr0.insert(Cr0Flags::MONITOR_COPROCESSOR | Cr0Flags::WRITE_PROTECT);
    unsafe {
        Cr0::write(cr0);
    }
}

pub unsafe fn enable_fsgsbase() {
    // SAFETY: Enabling FSGSBASE CR4 bit is safe on x86_64 processors
    unsafe {
        core::arch::asm!(
            "mov rax, cr4",
            "or rax, 0x10000", // Bit 16 (FSGSBASE)
            "mov cr4, rax",
            out("rax") _,
        );
    }
}

// ── CMOS Real-Time Clock reader ───────────────────────────────────────────────

/// Read a single CMOS register via I/O ports 0x70 (index) and 0x71 (data).
///
/// # Safety
/// Caller must ensure they are in a context where I/O port access is allowed
/// (i.e., kernel ring-0 code). Interrupts should be disabled while reading
/// to avoid split reads across an RTC update cycle.
#[inline]
unsafe fn cmos_read(reg: u8) -> u8 {
    let val: u8;
    // SAFETY: Port 0x70/0x71 are the CMOS index/data registers, always
    // accessible from ring 0. Writing the NMI-disable bit (0x80 | reg) is
    // harmless here; it is cleared once the kernel enables NMI normally.
    unsafe {
        core::arch::asm!(
            "out 0x70, al",   // select register
            "in al, 0x71",    // read data
            in("al") 0x80u8 | reg,
            lateout("al") val,
            options(nostack, nomem),
        );
    }
    val
}

/// Convert a BCD-encoded byte to binary.
#[inline]
fn bcd_to_bin(bcd: u8) -> u8 {
    (bcd & 0x0F) + ((bcd >> 4) * 10)
}

/// Returns `true` while the CMOS RTC is in the middle of an update cycle.
#[inline]
unsafe fn rtc_updating() -> bool {
    // SAFETY: Reading CMOS status register A (reg 0x0A).
    (unsafe { cmos_read(0x0A) } & 0x80) != 0
}

/// Read the CMOS RTC and return the current wall-clock time as a Unix timestamp
/// (seconds since 1970-01-01 00:00:00 UTC).
///
/// QEMU exposes the host's hardware clock through the CMOS RTC, so this gives
/// the kernel an accurate boot-time base for `CLOCK_REALTIME`.
///
/// The read is retried until two consecutive snapshots agree, avoiding split
/// reads across an in-progress RTC update cycle.
pub fn read_rtc_unix_time() -> u64 {
    // Spin until the RTC is not mid-update, then take two reads and compare.
    // SAFETY: I/O port access from ring-0 kernel init context.
    let (sec, min, hour, day, mon, year) = loop {
        while unsafe { rtc_updating() } {
            core::hint::spin_loop();
        }
        let s = unsafe { cmos_read(0x00) };
        let mn = unsafe { cmos_read(0x02) };
        let h = unsafe { cmos_read(0x04) };
        let d = unsafe { cmos_read(0x07) };
        let mo = unsafe { cmos_read(0x08) };
        let y = unsafe { cmos_read(0x09) };

        while unsafe { rtc_updating() } {
            core::hint::spin_loop();
        }
        let s2 = unsafe { cmos_read(0x00) };
        let mn2 = unsafe { cmos_read(0x02) };
        let h2 = unsafe { cmos_read(0x04) };
        let d2 = unsafe { cmos_read(0x07) };
        let mo2 = unsafe { cmos_read(0x08) };
        let y2 = unsafe { cmos_read(0x09) };

        if s == s2 && mn == mn2 && h == h2 && d == d2 && mo == mo2 && y == y2 {
            break (s, mn, h, d, mo, y);
        }
    };

    // Check register B bit 2 to determine if values are binary or BCD.
    // SAFETY: I/O port access from ring-0 kernel init context.
    let reg_b = unsafe { cmos_read(0x0B) };
    let is_binary = (reg_b & 0x04) != 0;

    let to_bin = |v: u8| if is_binary { v } else { bcd_to_bin(v) };

    let sec = to_bin(sec) as u64;
    let min = to_bin(min) as u64;
    let hour = to_bin(hour) as u64;
    let day = to_bin(day) as u64;
    let mon = to_bin(mon) as u64;
    // CMOS stores two-digit year; assume 2000+ for years 0-99.
    let year = to_bin(year) as u64 + 2000;

    // Convert calendar date to Unix timestamp using the standard formula.
    // Months are 1-based.
    let days_in_month: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);

    // Days from 1970-01-01 to start of `year`
    let y = year - 1970;
    let leap_years_before = (y + 1) / 4 - (y + 69) / 100 + (y + 369) / 400;
    let mut days = y * 365 + leap_years_before;

    // Add days for completed months
    for m in 0..(mon.saturating_sub(1) as usize) {
        days += days_in_month[m];
        if m == 1 && is_leap {
            days += 1;
        }
    }

    // Add days within the current month (1-based)
    days += day.saturating_sub(1);

    days * 86400 + hour * 3600 + min * 60 + sec
}
