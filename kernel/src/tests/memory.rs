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

//! Memory address math & alignment unit tests.

use crate::kprintln;
use crate::memory::address::{PhysAddr, VirtAddr};

#[test_case]
fn test_memory_address_math_and_alignment() {
    kprintln!("[test] Starting PhysAddr and VirtAddr unit tests...");

    // 1. PhysAddr 4KiB page alignment
    let unaligned_phys = PhysAddr::new(0x1234_5678_9ABC_D123);
    assert!(!unaligned_phys.is_aligned());
    assert_eq!(
        unaligned_phys.align_down(),
        PhysAddr::new(0x1234_5678_9ABC_D000)
    );
    assert_eq!(
        unaligned_phys.align_up(),
        PhysAddr::new(0x1234_5678_9ABC_E000)
    );

    let aligned_phys = PhysAddr::new(0x0000_0001_0000_0000);
    assert!(aligned_phys.is_aligned());
    assert_eq!(aligned_phys.align_down(), aligned_phys);
    assert_eq!(aligned_phys.align_up(), aligned_phys);

    // PhysAddr addition and subtraction
    assert_eq!(aligned_phys + 0x1000, PhysAddr::new(0x0000_0001_0000_1000));
    assert_eq!(PhysAddr::new(0x2000) - PhysAddr::new(0x1000), 0x1000);

    // 2. 2MiB Huge Page alignment checks
    let huge_page_mask = (2 * 1024 * 1024) - 1;
    let phys_2mb = PhysAddr::new(0x0000_0000_4000_0000); // 1GiB boundary (also 2MiB aligned)
    assert_eq!(phys_2mb.as_u64() & huge_page_mask, 0);

    let phys_2mb_unaligned = PhysAddr::new(0x0000_0000_4010_0000); // +1MiB
    assert_ne!(phys_2mb_unaligned.as_u64() & huge_page_mask, 0);

    // 3. VirtAddr canonical verification and truncation
    let user_canonical = 0x0000_7FFF_1234_5000u64;
    let v1 = VirtAddr::new(user_canonical);
    assert_eq!(v1.as_u64(), user_canonical);

    let kernel_canonical = 0xFFFF_8000_1234_5000u64;
    let v2 = VirtAddr::new(kernel_canonical);
    assert_eq!(v2.as_u64(), kernel_canonical);

    let non_canonical = 0x000F_8000_1234_5678u64;
    let v_trunc = VirtAddr::new_truncate(non_canonical);
    assert_eq!(v_trunc.as_u64(), 0xFFFF_8000_1234_5678u64);

    // VirtAddr page table index extraction
    let test_vaddr = VirtAddr::new(0x0000_7FFF_1234_5678);
    let (l4, l3, l2, l1, offset) = test_vaddr.page_table_indices();
    assert_eq!(offset, 0x678);
    assert_eq!(l1, ((0x1234_5678u64 >> 12) & 0x1FF) as u16);
    assert_eq!(l2, ((0x1234_5678u64 >> 21) & 0x1FF) as u16);

    kprintln!("[test] Memory address unit tests PASSED!");
}
