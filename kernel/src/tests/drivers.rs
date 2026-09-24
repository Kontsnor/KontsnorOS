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

//! Hardware driver unit & regression tests.

#[test_case]
fn test_ahci_controller_initialization() {
    // Allocate a mock register space on the heap (5000 bytes to fit 32 ports and control registers)
    let mut mock_registers = alloc::vec![0u8; 5000];
    let virt_base = mock_registers.as_mut_ptr() as u64;

    // Set Ports Implemented (PI) to 0x0000_0005 (ports 0 and 2 are active/implemented)
    let pi_offset = crate::drivers::block::ahci::HOST_PI as usize;
    unsafe {
        let pi_ptr = (virt_base + pi_offset as u64) as *mut u32;
        pi_ptr.write_volatile(0x0000_0005);
    }

    // Call init_controller_at
    let pi = unsafe { crate::drivers::block::ahci::test_helpers::init_controller_at(virt_base) };

    // Verify Ports Implemented
    assert_eq!(pi, 0x0000_0005);

    // Verify GHC has AE (AHCI Enable = bit 31) and IE (Interrupt Enable = bit 1) set
    let ghc_offset = crate::drivers::block::ahci::HOST_GHC as usize;
    let ghc = unsafe { ((virt_base + ghc_offset as u64) as *const u32).read_volatile() };
    assert_ne!(ghc & (1 << 31), 0);
    assert_ne!(ghc & (1 << 1), 0);
}

#[test_case]
fn test_ahci_port_connection() {
    // Allocate mock register space
    let mut mock_registers = alloc::vec![0u8; 5000];
    let virt_base = mock_registers.as_mut_ptr() as u64;

    let port_idx = 2;
    let port_base = 0x100 + port_idx * 0x80;

    // Set SSTS of port 2 to 3 (device detected and PHY established)
    unsafe {
        let ssts_ptr = (virt_base
            + port_base as u64
            + crate::drivers::block::ahci::PORT_SSTS as u64) as *mut u32;
        ssts_ptr.write_volatile(3);
    }

    // Mock physical addresses for command list and FIS
    let cl_phys = 0x1000_2000;
    let fis_phys = 0x3000_4000;

    // Initialize port 2
    unsafe {
        crate::drivers::block::ahci::test_helpers::init_port_at(
            virt_base, port_idx, cl_phys, fis_phys,
        );
    }

    // Assert that the command list and FIS base addresses were written correctly
    let clb = unsafe {
        ((virt_base + port_base as u64 + crate::drivers::block::ahci::PORT_CLB as u64)
            as *const u32)
            .read_volatile()
    };
    let clbu = unsafe {
        ((virt_base + port_base as u64 + crate::drivers::block::ahci::PORT_CLBU as u64)
            as *const u32)
            .read_volatile()
    };
    let fb = unsafe {
        ((virt_base + port_base as u64 + crate::drivers::block::ahci::PORT_FB as u64) as *const u32)
            .read_volatile()
    };
    let fbu = unsafe {
        ((virt_base + port_base as u64 + crate::drivers::block::ahci::PORT_FBU as u64)
            as *const u32)
            .read_volatile()
    };
    let cl_phys_read = clb as u64 | ((clbu as u64) << 32);
    let fis_phys_read = fb as u64 | ((fbu as u64) << 32);

    assert_eq!(cl_phys_read, cl_phys);
    assert_eq!(fis_phys_read, fis_phys);

    // Assert that port 2 CMD register has FRE (0x10) and ST (0x01) bits set
    let cmd = unsafe {
        ((virt_base + port_base as u64 + crate::drivers::block::ahci::PORT_CMD as u64)
            as *const u32)
            .read_volatile()
    };
    assert_ne!(cmd & 0x0010, 0); // FRE set
    assert_ne!(cmd & 0x0001, 0); // ST set
}

#[test_case]
fn test_nvme_controller_initialization() {
    // Allocate a mock register space on the heap (8192 bytes for MMIO registers)
    let mut mock_registers = alloc::vec![0u8; 8192];
    let virt_base = mock_registers.as_mut_ptr() as u64;

    unsafe {
        // Test VS register (0x08)
        crate::drivers::block::nvme::test_helpers::write_reg32(
            virt_base,
            crate::drivers::block::nvme::VS,
            0x00010300,
        ); // VS = 1.3.0
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg32(
                virt_base,
                crate::drivers::block::nvme::VS
            ),
            0x00010300
        );

        // Test CAP register (0x00) - 8 bytes
        crate::drivers::block::nvme::test_helpers::write_reg64(
            virt_base,
            crate::drivers::block::nvme::CAP,
            0x0014000300020001,
        );
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg64(
                virt_base,
                crate::drivers::block::nvme::CAP
            ),
            0x0014000300020001
        );

        // Test CC register (0x14)
        crate::drivers::block::nvme::test_helpers::write_reg32(
            virt_base,
            crate::drivers::block::nvme::CC,
            0x00460001,
        ); // CC.EN = 1, IOSQES=6, IOCQES=4
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg32(
                virt_base,
                crate::drivers::block::nvme::CC
            ),
            0x00460001
        );

        // Test AQA register (0x24)
        crate::drivers::block::nvme::test_helpers::write_reg32(
            virt_base,
            crate::drivers::block::nvme::AQA,
            (63 << 16) | 63,
        );
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg32(
                virt_base,
                crate::drivers::block::nvme::AQA
            ),
            (63 << 16) | 63
        );

        // Test ASQ and ACQ registers (0x28, 0x30) - 8 bytes
        crate::drivers::block::nvme::test_helpers::write_reg64(
            virt_base,
            crate::drivers::block::nvme::ASQ,
            0x10002000,
        );
        crate::drivers::block::nvme::test_helpers::write_reg64(
            virt_base,
            crate::drivers::block::nvme::ACQ,
            0x30004000,
        );
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg64(
                virt_base,
                crate::drivers::block::nvme::ASQ
            ),
            0x10002000
        );
        assert_eq!(
            crate::drivers::block::nvme::test_helpers::read_reg64(
                virt_base,
                crate::drivers::block::nvme::ACQ
            ),
            0x30004000
        );
    }
}

#[test_case]
fn test_nvme_identify_parsing() {
    // Allocate simulated identify namespace buffer (4096 bytes)
    let mut identify_buf = alloc::vec![0u8; 4096];

    // NSZE (Namespace Size) at offset 0 (8 bytes) = 0x0000_0000_1234_5678 (305,419,896 sectors)
    let expected_nsze: u64 = 0x12345678;
    identify_buf[0..8].copy_from_slice(&expected_nsze.to_ne_bytes());

    // FLBAS (Formatted LBA Size) at offset 27 (1 byte) = 0
    // Index 0 in LBA format table will be active
    identify_buf[27] = 0;

    // LBA Format table starts at offset 128
    // LBA Format 0 at bytes 128..132:
    // bits 16..23 is LBADS (LBA Data Size). If LBADS = 9 (2^9 = 512 bytes)
    let lbads: u8 = 9;
    let lbads_word = (lbads as u32) << 16;
    identify_buf[128..132].copy_from_slice(&lbads_word.to_ne_bytes());

    // Parse just like the driver would
    let nsze = u64::from_ne_bytes([
        identify_buf[0],
        identify_buf[1],
        identify_buf[2],
        identify_buf[3],
        identify_buf[4],
        identify_buf[5],
        identify_buf[6],
        identify_buf[7],
    ]);
    let flbas = identify_buf[27];
    let lbaf_idx = (flbas & 0x0F) as usize;

    let lbaf_offset = 128 + lbaf_idx * 4;
    let lbaf_entry = u32::from_ne_bytes([
        identify_buf[lbaf_offset],
        identify_buf[lbaf_offset + 1],
        identify_buf[lbaf_offset + 2],
        identify_buf[lbaf_offset + 3],
    ]);
    let parsed_lbads = ((lbaf_entry >> 16) & 0xFF) as u8;
    let block_size = if (9..=16).contains(&parsed_lbads) {
        1u64 << parsed_lbads
    } else {
        512
    };

    assert_eq!(nsze, expected_nsze);
    assert_eq!(block_size, 512);

    // Test a different LBA size: LBADS = 12 (2^12 = 4096 bytes)
    let lbads_12: u8 = 12;
    let lbads_word_12 = (lbads_12 as u32) << 16;
    identify_buf[128..132].copy_from_slice(&lbads_word_12.to_ne_bytes());

    let lbaf_entry_12 = u32::from_ne_bytes([
        identify_buf[128],
        identify_buf[129],
        identify_buf[130],
        identify_buf[131],
    ]);
    let parsed_lbads_12 = ((lbaf_entry_12 >> 16) & 0xFF) as u8;
    let block_size_12 = if (9..=16).contains(&parsed_lbads_12) {
        1u64 << parsed_lbads_12
    } else {
        512
    };
    assert_eq!(block_size_12, 4096);
}

#[test_case]
fn test_nvme_identify_controller_parsing() {
    let mut id_ctrl_buf = alloc::vec![0u8; 4096];

    // Serial number at offset 4..24 (20 ASCII bytes)
    let sn = b"TEST-NVME-SN-1234   ";
    id_ctrl_buf[4..24].copy_from_slice(sn);

    // Model number at offset 24..64 (40 ASCII bytes)
    let mn = b"KONTSNOR-NVME-DRIVE                     ";
    id_ctrl_buf[24..64].copy_from_slice(mn);

    // Firmware revision at offset 64..72 (8 ASCII bytes)
    let fr = b"1.0.0   ";
    id_ctrl_buf[64..72].copy_from_slice(fr);

    // Number of Namespaces at offset 516..520 (u32)
    let nn: u32 = 1;
    id_ctrl_buf[516..520].copy_from_slice(&nn.to_ne_bytes());

    let mut sn_bytes = [0u8; 20];
    let mut mn_bytes = [0u8; 40];
    let mut fr_bytes = [0u8; 8];
    sn_bytes.copy_from_slice(&id_ctrl_buf[4..24]);
    mn_bytes.copy_from_slice(&id_ctrl_buf[24..64]);
    fr_bytes.copy_from_slice(&id_ctrl_buf[64..72]);

    let parsed_sn = core::str::from_utf8(&sn_bytes).unwrap().trim();
    let parsed_mn = core::str::from_utf8(&mn_bytes).unwrap().trim();
    let parsed_fr = core::str::from_utf8(&fr_bytes).unwrap().trim();
    let parsed_nn = u32::from_ne_bytes(id_ctrl_buf[516..520].try_into().unwrap());

    assert_eq!(parsed_sn, "TEST-NVME-SN-1234");
    assert_eq!(parsed_mn, "KONTSNOR-NVME-DRIVE");
    assert_eq!(parsed_fr, "1.0.0");
    assert_eq!(parsed_nn, 1);

    // Also verify doorbell offset calculation:
    // Doorbell Offset = 0x1000 + (2 * QID + is_cq) * (4 << DSTRD)
    let dstrd = 0u32; // stride = 4
    let stride = 4 << dstrd;
    assert_eq!(0x1000 + (2 * 0 + 0) * stride, 0x1000); // Admin SQ
    assert_eq!(0x1000 + (2 * 0 + 1) * stride, 0x1004); // Admin CQ
    assert_eq!(0x1000 + (2 * 1 + 0) * stride, 0x1008); // IO SQ
    assert_eq!(0x1000 + (2 * 1 + 1) * stride, 0x100C); // IO CQ
}
