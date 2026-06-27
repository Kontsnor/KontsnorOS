//! Local APIC and I/O APIC drivers for x86_64.
//!
//! Replaces the legacy 8259 PIC with modern APIC interrupt routing.

use crate::kprintln;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

/// Physical memory offset for virtual memory mapping.
fn phys_offset() -> u64 {
    crate::memory::r#virtual::phys_mem_offset()
}

/// Globally stored Local APIC Virtual Base Address.
static LAPIC_BASE: AtomicU64 = AtomicU64::new(0);

/// Globally stored I/O APIC Virtual Base Address.
static IOAPIC_BASE: AtomicU64 = AtomicU64::new(0);

// ── Local APIC Register Offsets ─────────────────────────────────────
const LAPIC_REG_ID: u32 = 0x20;
const LAPIC_REG_VERSION: u32 = 0x30;
const LAPIC_REG_TPR: u32 = 0x80;
const LAPIC_REG_EOI: u32 = 0xB0;
const LAPIC_REG_SVR: u32 = 0xF0;
const LAPIC_REG_ICR_LOW: u32 = 0x300;
const LAPIC_REG_ICR_HIGH: u32 = 0x310;
const LAPIC_REG_LVT_TIMER: u32 = 0x320;
const LAPIC_REG_TIMER_INIT: u32 = 0x380;
const LAPIC_REG_TIMER_CURRENT: u32 = 0x390;
const LAPIC_REG_TIMER_DIV: u32 = 0x3E0;

// ── Local APIC Helper Functions ─────────────────────────────────────

unsafe fn lapic_read(reg: u32) -> u32 {
    let base = LAPIC_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return 0;
    }
    unsafe { core::ptr::read_volatile((base + reg as u64) as *const u32) }
}

unsafe fn lapic_write(reg: u32, val: u32) {
    let base = LAPIC_BASE.load(Ordering::Relaxed);
    if base != 0 {
        unsafe {
            core::ptr::write_volatile((base + reg as u64) as *mut u32, val);
        }
    }
}

/// Send End of Interrupt (EOI) to the Local APIC.
///
/// Must be called at the end of every hardware interrupt handler.
pub fn lapic_eoi() {
    unsafe {
        lapic_write(LAPIC_REG_EOI, 0);
    }
}

/// Read the current Local APIC ID.
pub fn get_lapic_id() -> u8 {
    if LAPIC_BASE.load(Ordering::Relaxed) == 0 {
        return 0;
    }
    unsafe {
        // APIC ID is in bits 24-31 of the ID register
        (lapic_read(LAPIC_REG_ID) >> 24) as u8
    }
}

// ── I/O APIC Helper Functions ───────────────────────────────────────

unsafe fn ioapic_read(reg: u8) -> u32 {
    let base = IOAPIC_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return 0;
    }
    unsafe {
        core::ptr::write_volatile(base as *mut u32, reg as u32);
        core::ptr::read_volatile((base + 0x10) as *const u32)
    }
}

unsafe fn ioapic_write(reg: u8, val: u32) {
    let base = IOAPIC_BASE.load(Ordering::Relaxed);
    if base != 0 {
        unsafe {
            core::ptr::write_volatile(base as *mut u32, reg as u32);
            core::ptr::write_volatile((base + 0x10) as *mut u32, val);
        }
    }
}

/// Configure a redirection table entry (RTE) on the I/O APIC.
pub fn ioapic_set_routing(pin: u8, vector: u8, apic_id: u8) {
    let low_index = 0x10 + 2 * pin;
    let high_index = low_index + 1;

    unsafe {
        let existing_low = ioapic_read(low_index);
        // Preserve existing flags (trigger mode, polarity, etc.), update the vector, and unmask (clear bit 16)
        let low_val = ((existing_low & 0xFFFF_FF00) | (vector as u32)) & !(1 << 16);
        let high_val = (apic_id as u32) << 24;

        ioapic_write(low_index, low_val);
        ioapic_write(high_index, high_val);
    }
}

/// Mask/disable routing for a specific pin on the I/O APIC.
pub fn ioapic_mask(pin: u8) {
    let low_index = 0x10 + 2 * pin;
    unsafe {
        let low_val = ioapic_read(low_index);
        ioapic_write(low_index, low_val | (1 << 16)); // Bit 16 is Mask
    }
}

// ── Disable legacy PIC 8259 ─────────────────────────────────────────

/// Disable the legacy PIC 8259 controllers.
pub fn disable_8259_pic() {
    let mut master_data = Port::new(0x21);
    let mut slave_data = Port::new(0xA1);
    unsafe {
        master_data.write(0xFFu8);
        slave_data.write(0xFFu8);
    }
    kprintln!("[apic] Disabled legacy 8259 PIC.");
}

// ── Initializer ─────────────────────────────────────────────────────

/// Initialize the Local APIC and I/O APIC.
pub fn init() {
    let madt_info = match crate::acpi::get_madt_info() {
        Some(info) => info,
        None => {
            kprintln!("[apic] Cannot initialize APIC: ACPI MADT not available.");
            return;
        }
    };

    // 1. Disable legacy PIC
    disable_8259_pic();

    // 2. Set up LAPIC base addresses
    let lapic_phys = madt_info.local_apic_address as u64;
    let lapic_virt = lapic_phys + phys_offset();
    LAPIC_BASE.store(lapic_virt, Ordering::SeqCst);

    kprintln!(
        "[apic] Local APIC base physical: {:#x}, virtual: {:#x}",
        lapic_phys,
        lapic_virt
    );

    // 3. Set up I/O APIC base addresses
    if madt_info.io_apics.is_empty() {
        kprintln!("[apic] Warning: No I/O APICs found in MADT!");
        return;
    }
    let ioapic_phys = madt_info.io_apics[0].address as u64;
    let ioapic_virt = ioapic_phys + phys_offset();
    IOAPIC_BASE.store(ioapic_virt, Ordering::SeqCst);

    kprintln!(
        "[apic] I/O APIC ID {} base physical: {:#x}, virtual: {:#x}",
        madt_info.io_apics[0].id,
        ioapic_phys,
        ioapic_virt
    );

    // 4. Initialize Local APIC on the BSP (Bootstrap Processor)
    unsafe {
        // Enable LAPIC by setting spurious vector to 0xFF and bit 8 to 1
        let svr = lapic_read(LAPIC_REG_SVR);
        lapic_write(LAPIC_REG_SVR, svr | 0x1FF); // 0xFF vector | 0x100 enable bit

        // Clear Task Priority to accept all interrupts
        lapic_write(LAPIC_REG_TPR, 0);
    }

    kprintln!("[apic] Local APIC enabled for CPU ID {}", get_lapic_id());

    // 5. Initialize per-core Local APIC periodic timer tick
    init_lapic_timer();
    kprintln!("[apic] Local APIC periodic timer enabled.");

    // 6. Initialize I/O APIC routing entries
    // Mask all 24 pins first
    for pin in 0..24 {
        ioapic_mask(pin);
    }

    // Route IRQ 1 (Keyboard) to IDT vector 33 (pin 1)
    ioapic_set_routing(1, 33, get_lapic_id());

    // Route IRQ 11 (e1000 PCI NIC) to IDT vector 43 (pin 11)
    ioapic_set_routing(11, 43, get_lapic_id());

    kprintln!("[apic] Redirection routing established via I/O APIC.");
}

/// Initialize the Local APIC periodic timer for the active core.
pub fn init_lapic_timer() {
    unsafe {
        // Set the timer divide configuration register to divide by 16 (value 0x3)
        lapic_write(LAPIC_REG_TIMER_DIV, 0x3);

        // Set the LVT Timer register: Periodic mode (bit 17 set to 1) with Vector 32
        lapic_write(LAPIC_REG_LVT_TIMER, 0x20000 | 32);

        // Set the Initial Count Register (10000000 counts per tick under QEMU)
        lapic_write(LAPIC_REG_TIMER_INIT, 10000000);
    }
}

/// Read the current Local APIC timer decrementer count.
pub fn get_lapic_timer_current() -> u32 {
    // SAFETY: Reading MMIO register via LAPIC base address is safe because LAPIC is mapped and active.
    unsafe { lapic_read(LAPIC_REG_TIMER_CURRENT) }
}

/// Send an Inter-Processor Interrupt (IPI) to a specific target Local APIC.
pub fn send_ipi(target_lapic_id: u8, vector: u8) {
    unsafe {
        // Write the target Local APIC ID to the high 32 bits of the ICR (bits 24-31)
        lapic_write(LAPIC_REG_ICR_HIGH, (target_lapic_id as u32) << 24);

        // Write delivery mode (000 = Fixed), dest mode (0 = Physical), level (1 = Assert)
        lapic_write(LAPIC_REG_ICR_LOW, vector as u32 | (1 << 14));

        // Wait until the Delivery Status bit (bit 12) becomes 0 (idle)
        while (lapic_read(LAPIC_REG_ICR_LOW) & (1 << 12)) != 0 {
            core::hint::spin_loop();
        }
    }
}

/// Broadcast an Inter-Processor Interrupt (IPI) to all other cores (excluding self).
pub fn broadcast_ipi_all_excluding_self(vector: u8) {
    unsafe {
        // High 32 bits of ICR is 0 when using shorthand
        lapic_write(LAPIC_REG_ICR_HIGH, 0);

        // Destination Shorthand: 11 (All Excluding Self) -> bits 18-19 set to 3.
        // Delivery Mode: 000 (Fixed), Dest Mode: 0 (Physical), Level: 1 (Assert) -> bit 14 set to 1.
        lapic_write(LAPIC_REG_ICR_LOW, vector as u32 | (1 << 14) | (3 << 18));

        // Wait until the Delivery Status bit (bit 12) becomes 0 (idle)
        while (lapic_read(LAPIC_REG_ICR_LOW) & (1 << 12)) != 0 {
            core::hint::spin_loop();
        }
    }
}
