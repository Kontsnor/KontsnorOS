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

//! ACPI subsystem unit & regression tests.

#[test_case]
fn test_acpi_find_table_edge_cases() {
    let phys_offset = crate::memory::r#virtual::phys_mem_offset();

    // 1. Invalid physical address (0)
    let res = crate::acpi::tables::find_table(0, b"APIC", 2);
    assert!(matches!(
        res,
        Err(crate::acpi::tables::AcpiError::InvalidAddress)
    ));

    // 2. Invalid signature
    let phys1 = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let virt1 = phys1 + phys_offset;
    unsafe {
        (virt1 as *mut crate::acpi::tables::SdtHeader).write(crate::acpi::tables::SdtHeader {
            signature: *b"BADS",
            length: 36,
            revision: 1,
            checksum: 0,
            oem_id: [0; 6],
            oem_table_id: [0; 8],
            oem_revision: 0,
            creator_id: 0,
            creator_revision: 0,
        });
    }

    let res = crate::acpi::tables::find_table(phys1, b"APIC", 2);
    assert!(matches!(
        res,
        Err(crate::acpi::tables::AcpiError::InvalidSignature)
    ));

    let res = crate::acpi::tables::find_table(phys1, b"APIC", 0);
    assert!(matches!(
        res,
        Err(crate::acpi::tables::AcpiError::InvalidSignature)
    ));
    crate::memory::physical::deallocate_frame(phys1);

    // 3. Short table length (< 36 bytes)
    let phys2 = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let virt2 = phys2 + phys_offset;
    unsafe {
        (virt2 as *mut crate::acpi::tables::SdtHeader).write(crate::acpi::tables::SdtHeader {
            signature: *b"XSDT",
            length: 20, // Less than minimum 36 bytes
            revision: 1,
            checksum: 0,
            oem_id: [0; 6],
            oem_table_id: [0; 8],
            oem_revision: 0,
            creator_id: 0,
            creator_revision: 0,
        });
    }

    let res = crate::acpi::tables::find_table(phys2, b"APIC", 2);
    assert!(matches!(
        res,
        Err(crate::acpi::tables::AcpiError::InvalidSignature)
    ));
    crate::memory::physical::deallocate_frame(phys2);

    // 4. Table not found
    let phys3 = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let virt3 = phys3 + phys_offset;
    unsafe {
        (virt3 as *mut crate::acpi::tables::SdtHeader).write(crate::acpi::tables::SdtHeader {
            signature: *b"XSDT",
            length: 36, // Header only, 0 entries
            revision: 1,
            checksum: 0,
            oem_id: [0; 6],
            oem_table_id: [0; 8],
            oem_revision: 0,
            creator_id: 0,
            creator_revision: 0,
        });
    }

    let res = crate::acpi::tables::find_table(phys3, b"APIC", 2);
    assert!(matches!(
        res,
        Err(crate::acpi::tables::AcpiError::TableNotFound)
    ));
    crate::memory::physical::deallocate_frame(phys3);

    // 5. XSDT (64-bit pointers) lookup success with null entry skipping
    let target_phys = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let target_virt = target_phys + phys_offset;
    unsafe {
        (target_virt as *mut crate::acpi::tables::SdtHeader).write(
            crate::acpi::tables::SdtHeader {
                signature: *b"APIC",
                length: 36,
                revision: 1,
                checksum: 0,
                oem_id: [0; 6],
                oem_table_id: [0; 8],
                oem_revision: 0,
                creator_id: 0,
                creator_revision: 0,
            },
        );
    }

    let xsdt_phys = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let xsdt_virt = xsdt_phys + phys_offset;
    unsafe {
        (xsdt_virt as *mut crate::acpi::tables::SdtHeader).write(crate::acpi::tables::SdtHeader {
            signature: *b"XSDT",
            length: 36 + 16, // 36 header + 2 * 8-byte entries
            revision: 1,
            checksum: 0,
            oem_id: [0; 6],
            oem_table_id: [0; 8],
            oem_revision: 0,
            creator_id: 0,
            creator_revision: 0,
        });
        let entries_ptr = (xsdt_virt + 36) as *mut u64;
        core::ptr::write_unaligned(entries_ptr, 0); // Null entry
        core::ptr::write_unaligned(entries_ptr.add(1), target_phys); // Valid entry
    }

    let found_phys =
        crate::acpi::tables::find_table(xsdt_phys, b"APIC", 2).expect("XSDT find_table failed");
    assert_eq!(found_phys, target_phys);

    crate::memory::physical::deallocate_frame(target_phys);
    crate::memory::physical::deallocate_frame(xsdt_phys);

    // 6. RSDT (32-bit pointers) lookup success with null entry skipping
    let target_rsdt_phys =
        crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let target_rsdt_virt = target_rsdt_phys + phys_offset;
    unsafe {
        (target_rsdt_virt as *mut crate::acpi::tables::SdtHeader).write(
            crate::acpi::tables::SdtHeader {
                signature: *b"MCFG",
                length: 36,
                revision: 1,
                checksum: 0,
                oem_id: [0; 6],
                oem_table_id: [0; 8],
                oem_revision: 0,
                creator_id: 0,
                creator_revision: 0,
            },
        );
    }

    let rsdt_phys = crate::memory::physical::allocate_frame().expect("Frame allocation failed");
    let rsdt_virt = rsdt_phys + phys_offset;
    unsafe {
        (rsdt_virt as *mut crate::acpi::tables::SdtHeader).write(crate::acpi::tables::SdtHeader {
            signature: *b"RSDT",
            length: 36 + 8, // 36 header + 2 * 4-byte entries
            revision: 1,
            checksum: 0,
            oem_id: [0; 6],
            oem_table_id: [0; 8],
            oem_revision: 0,
            creator_id: 0,
            creator_revision: 0,
        });
        let entries_ptr = (rsdt_virt + 36) as *mut u32;
        core::ptr::write_unaligned(entries_ptr, 0); // Null entry
        core::ptr::write_unaligned(entries_ptr.add(1), target_rsdt_phys as u32);
        // Valid entry
    }

    let found_rsdt_phys =
        crate::acpi::tables::find_table(rsdt_phys, b"MCFG", 0).expect("RSDT find_table failed");
    assert_eq!(found_rsdt_phys, target_rsdt_phys);

    crate::memory::physical::deallocate_frame(target_rsdt_phys);
    crate::memory::physical::deallocate_frame(rsdt_phys);
}

#[test_case]
fn test_acpi_rsdp_parsing() {
    // 1. Null physical address
    assert_eq!(
        crate::acpi::tables::parse_rsdp(0),
        Err(crate::acpi::tables::AcpiError::InvalidAddress)
    );

    let phys_offset = crate::memory::r#virtual::phys_mem_offset();

    // 2. Invalid RSDP Signature
    let invalid_rsdp = crate::acpi::tables::Rsdp {
        signature: *b"BAD SIG ",
        checksum: 0,
        oem_id: *b"TESTOM",
        revision: 2,
        rsdt_address: 0x1000,
        length: 36,
        xsdt_address: 0x2000,
        extended_checksum: 0,
        reserved: [0; 3],
    };
    let virt_addr1 = &invalid_rsdp as *const _ as u64;
    let phys_addr1 = virt_addr1 - phys_offset;

    assert_eq!(
        crate::acpi::tables::parse_rsdp(phys_addr1),
        Err(crate::acpi::tables::AcpiError::InvalidRsdpSignature)
    );

    // 3. Invalid Checksum
    let bad_checksum_rsdp = crate::acpi::tables::Rsdp {
        signature: *b"RSD PTR ",
        checksum: 0xFF, // Intentionally incorrect checksum
        oem_id: *b"TESTOM",
        revision: 2,
        rsdt_address: 0x1000,
        length: 36,
        xsdt_address: 0x2000,
        extended_checksum: 0,
        reserved: [0; 3],
    };
    let virt_addr2 = &bad_checksum_rsdp as *const _ as u64;
    let phys_addr2 = virt_addr2 - phys_offset;

    assert_eq!(
        crate::acpi::tables::parse_rsdp(phys_addr2),
        Err(crate::acpi::tables::AcpiError::InvalidChecksum)
    );

    // 4. Valid RSDP (Happy Path)
    let mut valid_rsdp = crate::acpi::tables::Rsdp {
        signature: *b"RSD PTR ",
        checksum: 0,
        oem_id: *b"MY OEM",
        revision: 2,
        rsdt_address: 0x1000,
        length: 36,
        xsdt_address: 0x2000_0000,
        extended_checksum: 0,
        reserved: [0; 3],
    };
    // Calculate valid checksum for first 20 bytes
    let bytes =
        unsafe { core::slice::from_raw_parts_mut(&mut valid_rsdp as *mut _ as *mut u8, 20) };
    let sum_without_checksum: u8 = bytes[0..8]
        .iter()
        .chain(&bytes[9..20])
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    valid_rsdp.checksum = (0u8).wrapping_sub(sum_without_checksum);

    let virt_addr3 = &valid_rsdp as *const _ as u64;
    let phys_addr3 = virt_addr3 - phys_offset;

    let parsed = crate::acpi::tables::parse_rsdp(phys_addr3).expect("Valid RSDP parsing failed");
    assert_eq!(parsed.oem_id, "MY OEM");
    assert_eq!(parsed.revision, 2);
    assert_eq!(parsed.xsdt_address, 0x2000_0000);
}
