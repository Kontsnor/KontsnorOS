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
    let (_l4, _l3, l2, l1, offset) = test_vaddr.page_table_indices();
    assert_eq!(offset, 0x678);
    assert_eq!(l1, ((0x1234_5678u64 >> 12) & 0x1FF) as u16);
    assert_eq!(l2, ((0x1234_5678u64 >> 21) & 0x1FF) as u16);

    kprintln!("[test] Memory address unit tests PASSED!");
}

#[test_case]
fn test_memory_allocator() {
    let (initial_used, _, _) = crate::memory::heap::stats();
    {
        let mut vec = alloc::vec::Vec::new();
        for i in 0..1000 {
            vec.push(i);
        }
        let (used_during, _, _) = crate::memory::heap::stats();
        assert!(used_during > initial_used);
    }
    let (final_used, _, _) = crate::memory::heap::stats();
    assert_eq!(initial_used, final_used);
}

#[test_case]
fn test_shared_mapping_communication() {
    // 1. Create and open file on ext via VFS directly
    let disk_dir = crate::fs::vfs::lookup("/disk").expect("Failed to lookup /disk");
    let _ = disk_dir.unlink("shared_test.txt");
    let inode = disk_dir
        .create("shared_test.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create shared_test.txt");

    // 2. Write 4096 bytes directly using InodeOps::write to populate/extend it
    let data = [0u8; 4096];
    let written = inode
        .write(0, &data)
        .expect("Failed to write to shared_test.txt");
    assert_eq!(written, 4096);

    // Allocate file descriptor manually
    let fd = crate::process::fd::current_task_alloc_fd(inode.clone())
        .expect("Failed to allocate file descriptor");

    // 3. mmap it with MAP_SHARED
    let addr1 = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x01, fd, 0); // PROT_READ|WRITE, MAP_SHARED
    assert!(addr1 > 0);

    // Write magic value in parent mapping (this faults in the lazy mapping)
    let ptr = addr1 as *mut u64;
    unsafe {
        ptr.write_volatile(0xDEADBEEF12345678);
    }

    // 4. Simulate fork by cloning page table
    let current_pid = crate::process::scheduler::current_pid().unwrap();
    let parent_task_arc = crate::process::scheduler::get_task_arc(current_pid).unwrap();
    let (parent_cr3, mmap_regions) = {
        let task = parent_task_arc.lock();
        let addr_space = task.address_space.lock();
        (addr_space.page_table_root, addr_space.mmap_regions.clone())
    };
    let child_cr3 = crate::memory::r#virtual::clone_parent_page_table(parent_cr3, &mmap_regions)
        .expect("Failed to clone page table");

    // Verify both point to same physical address
    let vaddr = x86_64::VirtAddr::new(addr1 as u64);
    let pte_parent = unsafe { crate::memory::page_cache::get_page_table_entry(parent_cr3, vaddr) }
        .expect("Parent PTE missing");
    let pte_child = unsafe { crate::memory::page_cache::get_page_table_entry(child_cr3, vaddr) }
        .expect("Child PTE missing");

    let phys_parent = pte_parent.addr().as_u64();
    let phys_child = pte_child.addr().as_u64();
    assert_eq!(phys_parent, phys_child);

    // Read magic value from virtual mapping directly
    let _direct_val = unsafe { ptr.read_volatile() };

    // Read magic value from child's mapped physical address
    let phys_offset = crate::memory::r#virtual::phys_mem_offset();
    let child_ptr = (phys_child + phys_offset) as *const u64;
    let read_val = unsafe { child_ptr.read_volatile() };
    assert_eq!(read_val, 0xDEADBEEF12345678);

    // Clean up
    crate::syscall::memory::sys_munmap(addr1 as u64, 4096);
    crate::process::fd::current_task_close_fd(fd);
    let _ = crate::memory::r#virtual::free_user_page_table(child_cr3);
    let _ = disk_dir.unlink("shared_test.txt");
}

#[test_case]
fn test_page_cache_isolation() {
    // 1. Create and open file on ext via VFS directly
    let disk_dir = crate::fs::vfs::lookup("/disk").expect("Failed to lookup /disk");
    let _ = disk_dir.unlink("private_test.txt");
    let inode = disk_dir
        .create("private_test.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create private_test.txt");

    // 2. Write 4096 bytes directly using InodeOps::write to populate/extend it
    let data = [0u8; 4096];
    let written = inode
        .write(0, &data)
        .expect("Failed to write to private_test.txt");
    assert_eq!(written, 4096);

    // Allocate file descriptor manually
    let fd = crate::process::fd::current_task_alloc_fd(inode.clone())
        .expect("Failed to allocate file descriptor");

    // Map MAP_PRIVATE
    let addr_priv = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x02, fd, 0);
    assert!(addr_priv > 0);

    // Map MAP_SHARED (to monitor the underlying file/cache state)
    let addr_shared = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x01, fd, 0);
    assert!(addr_shared > 0);

    // Write to private mapping (will trigger COW page fault)
    let priv_ptr = addr_priv as *mut u64;
    unsafe {
        priv_ptr.write_volatile(0x1122334455667788);
    }

    // Verify private mapping has the new value
    let priv_val = unsafe { priv_ptr.read_volatile() };
    assert_eq!(priv_val, 0x1122334455667788);

    // Verify shared mapping still has 0 (isolation)
    let shared_ptr = addr_shared as *const u64;
    let shared_val = unsafe { shared_ptr.read_volatile() };
    assert_eq!(shared_val, 0);

    // Verify underlying file still has 0
    let mut read_buf = [0u8; 8];
    let read_res = inode
        .read(0, &mut read_buf)
        .expect("Failed to read from private_test.txt");
    assert_eq!(read_res, 8);
    let file_val = u64::from_ne_bytes(read_buf);
    assert_eq!(file_val, 0);

    // Clean up
    crate::syscall::memory::sys_munmap(addr_priv as u64, 4096);
    crate::syscall::memory::sys_munmap(addr_shared as u64, 4096);
    crate::process::fd::current_task_close_fd(fd);
    let _ = disk_dir.unlink("private_test.txt");
}

#[test_case]
fn test_dirty_page_flush() {
    // 1. Create and open file on ext via VFS directly
    let disk_dir = crate::fs::vfs::lookup("/disk").expect("Failed to lookup /disk");
    let _ = disk_dir.unlink("flush_test.txt");
    let inode = disk_dir
        .create("flush_test.txt", crate::fs::inode::FileType::Regular)
        .expect("Failed to create flush_test.txt");

    // 2. Write 4096 bytes directly using InodeOps::write to populate/extend it
    let data = [0u8; 4096];
    let written = inode
        .write(0, &data)
        .expect("Failed to write to flush_test.txt");
    assert_eq!(written, 4096);

    // Allocate file descriptor manually
    let fd = crate::process::fd::current_task_alloc_fd(inode.clone())
        .expect("Failed to allocate file descriptor");

    // Sync the initial zero-fill to disk so disk backing has zeros before dirty mmap write
    crate::syscall::fs::sys_fsync(fd);

    // Map MAP_SHARED
    let addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x01, fd, 0);
    assert!(addr > 0);

    // Write magic value to the shared mapping
    let ptr = addr as *mut u64;
    unsafe {
        ptr.write_volatile(0x8877665544332211);
    }

    // Verify that the disk still has 0 before fsync (since it's only in page cache / memory)
    let mut read_buf = [0u8; 8];
    let res = inode.read_direct(0, &mut read_buf);
    assert!(res.is_ok());
    let val_before = u64::from_ne_bytes(read_buf);
    assert_eq!(val_before, 0);

    // Call fsync to commit changes
    let fsync_res = crate::syscall::fs::sys_fsync(fd);
    assert_eq!(fsync_res, 0);

    // Verify that the disk now has the magic value after fsync
    let res = inode.read_direct(0, &mut read_buf);
    assert!(res.is_ok());
    let val_after = u64::from_ne_bytes(read_buf);
    assert_eq!(val_after, 0x8877665544332211);

    // Clean up
    crate::syscall::memory::sys_munmap(addr as u64, 4096);
    crate::process::fd::current_task_close_fd(fd);
    let _ = disk_dir.unlink("flush_test.txt");
}

#[test_case]
fn test_unmap_range_geometric_cases_and_benchmark() {
    kprintln!("[test] Starting AddressSpace::unmap_range geometric cases & benchmark test...");
    use crate::process::task::{AddressSpace, MappedRegion};

    let make_region = |start: u64, len: usize| MappedRegion {
        start,
        len,
        inode: None,
        offset: 0,
        is_shared: false,
        prot: 3,
        pathname: None,
        is_stack: false,
    };

    let mut addr_space = AddressSpace {
        page_table_root: 0,
        start_brk: 0,
        brk: 0,
        mmap_bump: 0x500000000000,
        mmap_regions: alloc::vec::Vec::new(),
    };

    // Case 1: No-op unmap (unmapping range where no regions exist)
    addr_space.mmap_regions =
        alloc::vec![make_region(0x1000, 0x1000), make_region(0x3000, 0x1000),];
    addr_space.unmap_range(0x2000, 0x3000);
    assert_eq!(addr_space.mmap_regions.len(), 2);
    assert_eq!(addr_space.mmap_regions[0].start, 0x1000);
    assert_eq!(addr_space.mmap_regions[1].start, 0x3000);

    // Case 2: Full unmap
    addr_space.unmap_range(0x1000, 0x2000);
    assert_eq!(addr_space.mmap_regions.len(), 1);
    assert_eq!(addr_space.mmap_regions[0].start, 0x3000);

    // Case 3: Left-edge trim
    addr_space.mmap_regions = alloc::vec![make_region(0x1000, 0x2000)]; // [0x1000, 0x3000)
    addr_space.unmap_range(0x1000, 0x1800);
    assert_eq!(addr_space.mmap_regions.len(), 1);
    assert_eq!(addr_space.mmap_regions[0].start, 0x1800);
    assert_eq!(addr_space.mmap_regions[0].len, 0x1800); // 0x3000 - 0x1800 = 0x1800

    // Case 4: Right-edge trim
    addr_space.mmap_regions = alloc::vec![make_region(0x1000, 0x2000)]; // [0x1000, 0x3000)
    addr_space.unmap_range(0x2800, 0x3800);
    assert_eq!(addr_space.mmap_regions.len(), 1);
    assert_eq!(addr_space.mmap_regions[0].start, 0x1000);
    assert_eq!(addr_space.mmap_regions[0].len, 0x1800); // 0x2800 - 0x1000 = 0x1800

    // Case 5: Middle split (punching a hole)
    addr_space.mmap_regions = alloc::vec![make_region(0x1000, 0x3000)]; // [0x1000, 0x4000)
    addr_space.unmap_range(0x2000, 0x3000);
    assert_eq!(addr_space.mmap_regions.len(), 2);
    assert_eq!(addr_space.mmap_regions[0].start, 0x1000);
    assert_eq!(addr_space.mmap_regions[0].len, 0x1000);
    assert_eq!(addr_space.mmap_regions[1].start, 0x3000);
    assert_eq!(addr_space.mmap_regions[1].len, 0x1000);

    // Case 6: Multi-region straddle (trims right of R1, deletes R2 entirely, trims left of R3)
    addr_space.mmap_regions = alloc::vec![
        make_region(0x1000, 0x1000), // R1: [0x1000, 0x2000)
        make_region(0x2000, 0x1000), // R2: [0x2000, 0x3000)
        make_region(0x3000, 0x1000), // R3: [0x3000, 0x4000)
    ];
    addr_space.unmap_range(0x1800, 0x3800);
    assert_eq!(addr_space.mmap_regions.len(), 2);
    assert_eq!(addr_space.mmap_regions[0].start, 0x1000);
    assert_eq!(addr_space.mmap_regions[0].len, 0x800);
    assert_eq!(addr_space.mmap_regions[1].start, 0x3800);
    assert_eq!(addr_space.mmap_regions[1].len, 0x800);

    // Benchmark measuring rdtsc cycle counts for 1,000 unmap_range operations
    let mut bench_regions = alloc::vec::Vec::with_capacity(100);
    for i in 0..100 {
        bench_regions.push(make_region(0x10000 + (i as u64) * 0x2000, 0x1000));
    }
    let mut bench_space = AddressSpace {
        page_table_root: 0,
        start_brk: 0,
        brk: 0,
        mmap_bump: 0x500000000000,
        mmap_regions: bench_regions.clone(),
    };

    let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    for _ in 0..1000 {
        bench_space.mmap_regions = bench_regions.clone();
        bench_space.unmap_range(0x11000, 0x18000);
    }
    let end_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let elapsed_cycles = end_tsc.saturating_sub(start_tsc);
    kprintln!(
        "[bench] 1,000 in-place unmap operations completed in {} TSC cycles",
        elapsed_cycles
    );

    kprintln!("[test] AddressSpace::unmap_range geometric cases & benchmark test PASSED!");
}

#[test_case]
fn test_mmap_flag_validation() {
    kprintln!("[test] Starting sys_mmap flag and offset validation test...");

    // Test 1: Invalid mapping type flags (e.g., flags = 0 or flags = 0x20 [MAP_ANONYMOUS without MAP_PRIVATE/SHARED])
    let res_no_type = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x20, -1, 0);
    assert_eq!(res_no_type, -22); // -EINVAL

    let res_zero_flags = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x00, -1, 0);
    assert_eq!(res_zero_flags, -22); // -EINVAL

    let res_invalid_type = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x05, -1, 0);
    assert_eq!(res_invalid_type, -22); // -EINVAL

    // Test 2: Unaligned or negative offset
    let res_unaligned_off = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 100);
    assert_eq!(res_unaligned_off, -22); // -EINVAL

    let res_neg_off = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, -4096);
    assert_eq!(res_neg_off, -22); // -EINVAL

    // Test 3: Valid anonymous mapping (MAP_PRIVATE | MAP_ANONYMOUS = 0x22)
    let addr = crate::syscall::memory::sys_mmap(0, 4096, 3, 0x22, -1, 0);
    assert!(addr > 0);
    crate::syscall::memory::sys_munmap(addr as u64, 4096);

    kprintln!("[test] sys_mmap flag and offset validation test PASSED!");
}
