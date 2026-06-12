//! ACPI (Advanced Configuration and Power Interface) support.
//!
//! Provides hardware-independent power management, processor
//! enumeration, and device configuration.

pub mod tables;

use self::tables::MadtInfo;
use crate::kprintln;
use spin::Mutex;

/// Globally stored MADT information.
static MADT_INFO: Mutex<Option<MadtInfo>> = Mutex::new(None);

/// Retrieve the globally discovered MADT information.
pub fn get_madt_info() -> Option<MadtInfo> {
    MADT_INFO.lock().clone()
}

/// Initialize the ACPI subsystem.
///
/// Searches for the RSDP, traverses the XSDT/RSDT, and parses core tables like MADT.
pub fn init(rsdp_addr: Option<u64>) {
    let rsdp_phys = if let Some(addr) = rsdp_addr {
        kprintln!("[acpi] RSDP address provided by bootloader: {:#x}", addr);
        addr
    } else {
        kprintln!("[acpi] No RSDP address provided by bootloader, scanning...");
        if let Some(addr) = tables::scan_for_rsdp() {
            addr
        } else {
            kprintln!("[acpi] RSDP not found. ACPI unavailable.");
            return;
        }
    };

    kprintln!("[acpi] RSDP found at {:#x}", rsdp_phys);
    match tables::parse_rsdp(rsdp_phys) {
        Ok(rsdp_info) => {
            kprintln!("[acpi] OEM ID: {}", rsdp_info.oem_id);
            kprintln!("[acpi] Revision: {}", rsdp_info.revision);
            kprintln!(
                "[acpi] XSDT/RSDT physical address: {:#x}",
                rsdp_info.xsdt_address
            );

            // Search for MADT ("APIC") in XSDT/RSDT
            match tables::find_table(rsdp_info.xsdt_address, b"APIC", rsdp_info.revision) {
                Ok(madt_phys) => {
                    kprintln!(
                        "[acpi] Found MADT (APIC) table at physical {:#x}",
                        madt_phys
                    );
                    match tables::parse_madt(madt_phys) {
                        Ok(madt_info) => {
                            kprintln!("[acpi] Successfully parsed MADT.");
                            *MADT_INFO.lock() = Some(madt_info);
                        }
                        Err(e) => {
                            kprintln!("[acpi] Failed to parse MADT: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    kprintln!("[acpi] MADT table not found in XSDT/RSDT: {:?}", e);
                }
            }
        }
        Err(e) => {
            kprintln!("[acpi] Failed to parse RSDP: {:?}", e);
        }
    }
}
