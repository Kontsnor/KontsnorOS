//! ACPI table definitions and parsing.
//!
//! Implements parsing for the core ACPI tables needed for
//! hardware discovery and power management.

use alloc::string::String;
use alloc::vec::Vec;

use crate::kprintln;

/// ACPI parsing errors.
#[derive(Debug, Clone, Copy)]
pub enum AcpiError {
    /// Invalid RSDP signature.
    InvalidRsdpSignature,
    /// Invalid RSDP checksum.
    InvalidChecksum,
    /// Table not found.
    TableNotFound,
    /// Invalid table signature.
    InvalidSignature,
    /// Address is null or invalid.
    InvalidAddress,
}

/// Information extracted from the RSDP.
#[derive(Debug)]
pub struct RsdpInfo {
    /// OEM identifier string.
    pub oem_id: String,
    /// ACPI revision (0 = 1.0, 2 = 2.0+).
    pub revision: u8,
    /// Address of the XSDT (or RSDT for ACPI 1.0).
    pub xsdt_address: u64,
}

/// RSDP (Root System Description Pointer) structure.
///
/// Located in the BIOS data area or UEFI system table.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Rsdp {
    /// "RSD PTR " signature.
    pub signature: [u8; 8],
    /// Checksum of the first 20 bytes.
    pub checksum: u8,
    /// OEM identifier.
    pub oem_id: [u8; 6],
    /// Revision (0 = ACPI 1.0, 2 = ACPI 2.0+).
    pub revision: u8,
    /// Physical address of the RSDT.
    pub rsdt_address: u32,
    // --- ACPI 2.0+ fields follow ---
    /// Length of the full RSDP (including extended fields).
    pub length: u32,
    /// Physical address of the XSDT.
    pub xsdt_address: u64,
    /// Extended checksum.
    pub extended_checksum: u8,
    /// Reserved bytes.
    pub reserved: [u8; 3],
}

/// ACPI System Description Table header.
///
/// All ACPI tables start with this common header.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct SdtHeader {
    /// 4-byte ASCII signature identifying the table.
    pub signature: [u8; 4],
    /// Length of the table including the header.
    pub length: u32,
    /// Table revision.
    pub revision: u8,
    /// Checksum (all bytes must sum to 0).
    pub checksum: u8,
    /// OEM identifier.
    pub oem_id: [u8; 6],
    /// OEM table identifier.
    pub oem_table_id: [u8; 8],
    /// OEM revision.
    pub oem_revision: u32,
    /// Creator ID.
    pub creator_id: u32,
    /// Creator revision.
    pub creator_revision: u32,
}

/// MADT (Multiple APIC Description Table) entry types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MadtEntryType {
    /// Processor Local APIC.
    LocalApic = 0,
    /// I/O APIC.
    IoApic = 1,
    /// Interrupt Source Override.
    InterruptSourceOverride = 2,
    /// NMI Source.
    NmiSource = 3,
    /// Local APIC NMI.
    LocalApicNmi = 4,
    /// Local APIC Address Override.
    LocalApicAddrOverride = 5,
    /// Processor Local x2APIC.
    LocalX2Apic = 9,
}

/// A CPU core discovered via the MADT.
#[derive(Debug, Clone)]
pub struct CpuInfo {
    /// ACPI processor UID.
    pub processor_id: u8,
    /// Local APIC ID.
    pub apic_id: u8,
    /// Whether this processor is enabled.
    pub enabled: bool,
    /// Whether this processor can be online-enabled.
    pub online_capable: bool,
}

/// An I/O APIC discovered via the MADT.
#[derive(Debug, Clone)]
pub struct IoApicInfo {
    /// I/O APIC ID.
    pub id: u8,
    /// Base physical address of the I/O APIC registers.
    pub address: u32,
    /// Global system interrupt base.
    pub gsi_base: u32,
}

/// MADT parsed information.
#[derive(Debug, Clone)]
pub struct MadtInfo {
    /// Local APIC physical address.
    pub local_apic_address: u32,
    /// Whether the dual-8259 PIC is installed.
    pub has_8259_pic: bool,
    /// CPUs discovered.
    pub cpus: Vec<CpuInfo>,
    /// I/O APICs discovered.
    pub io_apics: Vec<IoApicInfo>,
}

/// RSDP expected signature.
const RSDP_SIGNATURE: &[u8; 8] = b"RSD PTR ";

/// Parse the RSDP at the given physical address.
///
/// # Safety
///
/// The address must point to valid physical memory containing an RSDP.
pub fn parse_rsdp(phys_addr: u64) -> Result<RsdpInfo, AcpiError> {
    if phys_addr == 0 {
        return Err(AcpiError::InvalidAddress);
    }

    let phys_offset = crate::memory::r#virtual::phys_mem_offset();
    let virt_addr = phys_addr + phys_offset;

    // SAFETY: The caller guarantees this address is valid.
    let rsdp = unsafe { &*(virt_addr as *const Rsdp) };

    // Validate signature
    if &rsdp.signature != RSDP_SIGNATURE {
        return Err(AcpiError::InvalidRsdpSignature);
    }

    // Validate checksum (first 20 bytes)
    let bytes = unsafe {
        core::slice::from_raw_parts(virt_addr as *const u8, 20)
    };
    let sum: u8 = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    if sum != 0 {
        return Err(AcpiError::InvalidChecksum);
    }

    let oem_id = core::str::from_utf8(&rsdp.oem_id)
        .unwrap_or("??????")
        .trim()
        .into();

    let xsdt_address = if rsdp.revision >= 2 {
        rsdp.xsdt_address
    } else {
        rsdp.rsdt_address as u64
    };

    Ok(RsdpInfo {
        oem_id,
        revision: rsdp.revision,
        xsdt_address,
    })
}

/// Scan standard memory regions for the RSDP.
///
/// Searches:
/// 1. EBDA (Extended BIOS Data Area)
/// 2. BIOS ROM area (0xE0000 - 0xFFFFF)
pub fn scan_for_rsdp() -> Option<u64> {
    let phys_offset = crate::memory::r#virtual::phys_mem_offset();
    
    // Scan the BIOS ROM area
    let start = 0xE0000u64;
    let end = 0xFFFFFu64;

    let mut addr = start;
    while addr < end {
        // SAFETY: We're scanning known BIOS memory regions.
        let ptr = (addr + phys_offset) as *const [u8; 8];
        let sig = unsafe { &*ptr };

        if sig == RSDP_SIGNATURE {
            // Verify checksum
            let bytes = unsafe {
                core::slice::from_raw_parts((addr + phys_offset) as *const u8, 20)
            };
            let sum: u8 = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
            if sum == 0 {
                return Some(addr);
            }
        }

        addr += 16; // RSDP is always 16-byte aligned
    }

    None
}

/// Traverse the XSDT/RSDT to locate a table by signature.
pub fn find_table(xsdt_phys_addr: u64, signature: &[u8; 4], revision: u8) -> Result<u64, AcpiError> {
    if xsdt_phys_addr == 0 {
        return Err(AcpiError::InvalidAddress);
    }

    let phys_offset = crate::memory::r#virtual::phys_mem_offset();
    let xsdt_virt_addr = xsdt_phys_addr + phys_offset;
    let header = unsafe { &*(xsdt_virt_addr as *const SdtHeader) };

    let expected_sig = if revision >= 2 { b"XSDT" } else { b"RSDT" };
    if &header.signature != expected_sig {
        return Err(AcpiError::InvalidSignature);
    }

    let length = header.length as usize;
    if length < 36 {
        return Err(AcpiError::InvalidSignature);
    }

    if revision >= 2 {
        // XSDT contains 64-bit pointers
        let entry_count = (length - 36) / 8;
        let entries_ptr = (xsdt_virt_addr + 36) as *const u64;

        for i in 0..entry_count {
            let entry = unsafe { core::ptr::read_unaligned(entries_ptr.add(i)) };
            if entry != 0 {
                let entry_virt = entry + phys_offset;
                let table_header = unsafe { &*(entry_virt as *const SdtHeader) };
                if &table_header.signature == signature {
                    return Ok(entry);
                }
            }
        }
    } else {
        // RSDT contains 32-bit pointers
        let entry_count = (length - 36) / 4;
        let entries_ptr = (xsdt_virt_addr + 36) as *const u32;

        for i in 0..entry_count {
            let entry = unsafe { core::ptr::read_unaligned(entries_ptr.add(i)) };
            if entry != 0 {
                let entry_u64 = entry as u64;
                let entry_virt = entry_u64 + phys_offset;
                let table_header = unsafe { &*(entry_virt as *const SdtHeader) };
                if &table_header.signature == signature {
                    return Ok(entry_u64);
                }
            }
        }
    }

    Err(AcpiError::TableNotFound)
}

/// Parse the MADT to discover CPUs and I/O APICs.
pub fn parse_madt(madt_addr: u64) -> Result<MadtInfo, AcpiError> {
    if madt_addr == 0 {
        return Err(AcpiError::InvalidAddress);
    }

    let phys_offset = crate::memory::r#virtual::phys_mem_offset();
    let virt_addr = madt_addr + phys_offset;

    let header = unsafe { &*(virt_addr as *const SdtHeader) };

    // Verify signature is "APIC"
    if &header.signature != b"APIC" {
        return Err(AcpiError::InvalidSignature);
    }

    // After the SDT header (36 bytes), the MADT has:
    // - 4 bytes: Local APIC Address
    // - 4 bytes: Flags
    let madt_data = unsafe {
        core::slice::from_raw_parts(
            virt_addr as *const u8,
            header.length as usize,
        )
    };

    let local_apic_address = u32::from_le_bytes([
        madt_data[36], madt_data[37], madt_data[38], madt_data[39],
    ]);

    let flags = u32::from_le_bytes([
        madt_data[40], madt_data[41], madt_data[42], madt_data[43],
    ]);

    let has_8259_pic = flags & 1 != 0;

    let mut cpus = Vec::new();
    let mut io_apics = Vec::new();

    // Parse MADT entries (start at offset 44)
    let mut offset = 44;
    while offset + 2 <= header.length as usize {
        let entry_type = madt_data[offset];
        let entry_len = madt_data[offset + 1] as usize;

        if entry_len < 2 || offset + entry_len > header.length as usize {
            break;
        }

        match entry_type {
            0 => {
                // Local APIC
                if entry_len >= 8 {
                    let processor_id = madt_data[offset + 2];
                    let apic_id = madt_data[offset + 3];
                    let apic_flags = u32::from_le_bytes([
                        madt_data[offset + 4],
                        madt_data[offset + 5],
                        madt_data[offset + 6],
                        madt_data[offset + 7],
                    ]);

                    cpus.push(CpuInfo {
                        processor_id,
                        apic_id,
                        enabled: apic_flags & 1 != 0,
                        online_capable: apic_flags & 2 != 0,
                    });
                }
            }
            1 => {
                // I/O APIC
                if entry_len >= 12 {
                    let id = madt_data[offset + 2];
                    let address = u32::from_le_bytes([
                        madt_data[offset + 4],
                        madt_data[offset + 5],
                        madt_data[offset + 6],
                        madt_data[offset + 7],
                    ]);
                    let gsi_base = u32::from_le_bytes([
                        madt_data[offset + 8],
                        madt_data[offset + 9],
                        madt_data[offset + 10],
                        madt_data[offset + 11],
                    ]);

                    io_apics.push(IoApicInfo {
                        id,
                        address,
                        gsi_base,
                    });
                }
            }
            _ => {
                // Skip other entry types for now
                // (e.g. Interrupt Source Override, NMI, Local APIC Address Override)
            }
        }

        offset += entry_len;
    }

    kprintln!("[acpi] MADT: {} CPUs, {} I/O APICs", cpus.len(), io_apics.len());

    Ok(MadtInfo {
        local_apic_address,
        has_8259_pic,
        cpus,
        io_apics,
    })
}
