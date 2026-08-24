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

//! ext writable filesystem driver for KontsnorOS.

use crate::drivers::traits::BlockDevice;
use crate::fs::inode::{DirEntry, FilePermissions, FileType, Inode, InodeOps};
use crate::fs::vfs::{FileSystem, FsStats};
use crate::kprintln;
use crate::sync::spinlock::TicketLock;
use ::alloc::sync::Arc;
use ::alloc::vec::Vec;
use spin::RwLock;

pub mod alloc;
pub mod dir;
pub mod file;
pub mod types;

pub use types::{
    Ext4Extent, Ext4ExtentHeader, Ext4ExtentIdx, ExtRawInode, GroupDescriptor, JournalSuperblock,
    Superblock,
};

/// Helper to count free bits (zeros) in a bitmap buffer.
pub(crate) fn count_free_bits(bitmap: &[u8], total_count: u32) -> u32 {
    let mut count = 0;
    for i in 0..total_count {
        let byte_idx = (i / 8) as usize;
        let bit_idx = i % 8;
        if byte_idx < bitmap.len() {
            if (bitmap[byte_idx] & (1 << bit_idx)) == 0 {
                count += 1;
            }
        }
    }
    count
}

/// Logical-to-physical block reading helper.
pub(crate) fn read_blocks(
    device: &dyn BlockDevice,
    block: u64,
    buf: &mut [u8],
    block_size: u32,
) -> Result<(), &'static str> {
    let dev_block_size = device.block_size();
    let dev_blocks_per_fs_block = (block_size as u64) / dev_block_size;
    let start_dev_block = block * dev_blocks_per_fs_block;
    device
        .read_block(start_dev_block, buf)
        .map_err(|_| "Block device read error")
}

/// Logical-to-physical block writing helper.
pub(crate) fn write_blocks(
    device: &dyn BlockDevice,
    block: u64,
    buf: &[u8],
    block_size: u32,
) -> Result<(), &'static str> {
    let dev_block_size = device.block_size();
    let dev_blocks_per_fs_block = (block_size as u64) / dev_block_size;
    let start_dev_block = block * dev_blocks_per_fs_block;
    device
        .write_block(start_dev_block, buf)
        .map_err(|_| "Block device write error")
}

/// ext FileSystem implementation.
pub struct ExtFileSystem {
    pub(crate) device: Arc<dyn BlockDevice>,
    pub(crate) block_size: u32,
    pub(crate) inodes_per_block: u32,
    pub(crate) inodes_per_group: u32,
    pub(crate) inode_size: u16,
    pub(crate) superblock: TicketLock<Superblock>,
    pub(crate) group_descriptors: TicketLock<Vec<GroupDescriptor>>,
    pub(crate) root_node: TicketLock<Option<Arc<dyn InodeOps>>>,
}

impl ExtFileSystem {
    /// Mount an ext volume on a block device.
    pub fn mount(device: Arc<dyn BlockDevice>) -> Result<Arc<Self>, &'static str> {
        let mut sb_buf = [0u8; 1024];

        // Superblock starts at offset 1024 (sectors 2 and 3 of 512-byte physical sectors)
        device
            .read_block(2, &mut sb_buf[0..512])
            .map_err(|_| "Error reading superblock low")?;
        device
            .read_block(3, &mut sb_buf[512..1024])
            .map_err(|_| "Error reading superblock high")?;

        // SAFETY: sb_buf is a local stack-allocated array of 1024 bytes which is sufficiently large and aligned for Superblock.
        let mut sb = unsafe { core::ptr::read_unaligned(sb_buf.as_ptr() as *const Superblock) };
        let s_magic = sb.s_magic;
        let s_log_block_size = sb.s_log_block_size;
        let s_inode_size = sb.s_inode_size;
        let s_inodes_per_group = sb.s_inodes_per_group;

        if s_magic != 0xEF53 {
            return Err("Invalid ext superblock magic");
        }

        // Validate metadata parameters
        if s_log_block_size > 10 || s_inode_size == 0 || s_inodes_per_group == 0 {
            return Err("Malformed ext superblock fields");
        }

        if sb.s_inodes_count == 0 || sb.s_blocks_count == 0 {
            return Err("Malformed ext superblock: inodes or blocks count is zero");
        }

        let blocks_per_group = sb.s_blocks_per_group;
        let num_groups = core::cmp::max(
            1,
            ((sb.s_blocks_count - sb.s_first_data_block + blocks_per_group - 1) / blocks_per_group)
                as usize,
        );

        let block_size = 1024 << s_log_block_size;
        let inode_size = s_inode_size;
        let inodes_per_group = s_inodes_per_group;
        let inodes_per_block = block_size / inode_size as u32;

        kprintln!(
            "[ext] Volume detected. s_magic: {:#x}, Block Size: {}, Inode Size: {}",
            s_magic,
            block_size,
            inode_size
        );

        // Read Group Descriptor Table
        let gdt_block = if block_size == 1024 { 2 } else { 1 };
        let gd_size = core::mem::size_of::<GroupDescriptor>();
        let gdt_size = num_groups * gd_size;
        let gdt_blocks = (gdt_size + block_size as usize - 1) / block_size as usize;
        let mut gds = Vec::with_capacity(num_groups);

        let mut gdt_buf = ::alloc::vec![0u8; gdt_blocks * block_size as usize];
        for b in 0..gdt_blocks {
            let offset = b * block_size as usize;
            read_blocks(
                &*device,
                (gdt_block + b) as u64,
                &mut gdt_buf[offset..(offset + block_size as usize)],
                block_size,
            )?;
        }

        for i in 0..num_groups {
            let gd_offset = i * gd_size;
            // SAFETY: gdt_buf contains at least gd_offset + gd_size bytes read from the block device, which is valid for GroupDescriptor.
            let gd = unsafe {
                core::ptr::read_unaligned(gdt_buf[gd_offset..].as_ptr() as *const GroupDescriptor)
            };

            // Validate GDT offsets are within filesystem bounds
            if gd.bg_block_bitmap >= sb.s_blocks_count
                || gd.bg_inode_bitmap >= sb.s_blocks_count
                || gd.bg_inode_table >= sb.s_blocks_count
            {
                return Err("Metadata blocks exceed filesystem blocks count");
            }
            gds.push(gd);
        }

        // --- Self-healing bitmaps check on mount ---
        let mut bitmap_healed = false;
        for g in 0..num_groups {
            let gd = &mut gds[g];
            let mut block_bitmap = ::alloc::vec![0u8; block_size as usize];
            read_blocks(
                &*device,
                gd.bg_block_bitmap as u64,
                &mut block_bitmap,
                block_size,
            )?;
            let mut inode_bitmap = ::alloc::vec![0u8; block_size as usize];
            read_blocks(
                &*device,
                gd.bg_inode_bitmap as u64,
                &mut inode_bitmap,
                block_size,
            )?;

            let mut block_bitmap_changed = false;
            if block_bitmap.iter().all(|&x| x == 0) {
                kprintln!(
                    "[ext] Block bitmap for group {} is all zeros, healing...",
                    g
                );
                // Mark metadata blocks for this group as allocated in the bitmap
                let it_blocks = match (s_inodes_per_group as u64).checked_mul(s_inode_size as u64) {
                    Some(prod) => ((prod + block_size as u64 - 1) / block_size as u64) as u32,
                    None => return Err("Overflow in metadata size calculation"),
                };
                let start_block = (g as u32) * blocks_per_group + sb.s_first_data_block;
                let end_block =
                    core::cmp::min(gd.bg_inode_table as u32 + it_blocks, sb.s_blocks_count);
                for b in start_block..end_block {
                    let local_b = b - start_block;
                    let byte = (local_b / 8) as usize;
                    let bit = local_b % 8;
                    if byte < block_bitmap.len() {
                        block_bitmap[byte] |= 1 << bit;
                    }
                }

                // Initialize padding bits for block bitmap
                let group_blocks = if g == num_groups - 1 {
                    sb.s_blocks_count - sb.s_first_data_block - (g as u32) * blocks_per_group
                } else {
                    blocks_per_group
                };
                for b in group_blocks..((block_size * 8) as u32) {
                    let byte = (b / 8) as usize;
                    let bit = b % 8;
                    if byte < block_bitmap.len() {
                        block_bitmap[byte] |= 1 << bit;
                    }
                }

                write_blocks(
                    &*device,
                    gd.bg_block_bitmap as u64,
                    &block_bitmap,
                    block_size,
                )?;
                block_bitmap_changed = true;
            }

            let mut inode_bitmap_changed = false;
            if inode_bitmap.iter().all(|&x| x == 0) {
                kprintln!(
                    "[ext] Inode bitmap for group {} is all zeros, healing...",
                    g
                );
                let start_ino = if g == 0 { 10 } else { 0 }; // reserve first 10 inodes in group 0
                for i in 0..start_ino {
                    let byte = (i / 8) as usize;
                    let bit = i % 8;
                    if byte < inode_bitmap.len() {
                        inode_bitmap[byte] |= 1 << bit;
                    }
                }

                // Initialize padding bits for inode bitmap
                let group_inodes = if g == num_groups - 1 {
                    sb.s_inodes_count - (g as u32) * s_inodes_per_group
                } else {
                    s_inodes_per_group
                };
                for i in group_inodes..((block_size * 8) as u32) {
                    let byte = (i / 8) as usize;
                    let bit = i % 8;
                    if byte < inode_bitmap.len() {
                        inode_bitmap[byte] |= 1 << bit;
                    }
                }

                write_blocks(
                    &*device,
                    gd.bg_inode_bitmap as u64,
                    &inode_bitmap,
                    block_size,
                )?;
                inode_bitmap_changed = true;
            }

            if block_bitmap_changed || inode_bitmap_changed {
                let group_blocks = if g == num_groups - 1 {
                    sb.s_blocks_count - sb.s_first_data_block - (g as u32) * blocks_per_group
                } else {
                    blocks_per_group
                };
                let group_inodes = if g == num_groups - 1 {
                    sb.s_inodes_count - (g as u32) * s_inodes_per_group
                } else {
                    s_inodes_per_group
                };
                let free_b = count_free_bits(&block_bitmap, group_blocks);
                let free_i = count_free_bits(&inode_bitmap, group_inodes);
                gd.bg_free_blocks_count = free_b as u16;
                gd.bg_free_inodes_count = free_i as u16;
                bitmap_healed = true;
            }
        }

        if bitmap_healed {
            let mut total_free_blocks = 0u32;
            let mut total_free_inodes = 0u32;
            for g in 0..num_groups {
                total_free_blocks += gds[g].bg_free_blocks_count as u32;
                total_free_inodes += gds[g].bg_free_inodes_count as u32;
            }
            sb.s_free_blocks_count = total_free_blocks;
            sb.s_free_inodes_count = total_free_inodes;

            // Write superblock back
            let sb_ptr = &sb as *const Superblock as *const u8;
            let mut sb_buf_write = [0u8; 1024];
            // SAFETY: sb is stack-allocated, sb_buf_write is 1024 bytes, copy is within bounds.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    sb_ptr,
                    sb_buf_write.as_mut_ptr(),
                    core::mem::size_of::<Superblock>(),
                );
            }
            device
                .write_block(2, &sb_buf_write[0..512])
                .map_err(|_| "Error writing superblock low")?;
            device
                .write_block(3, &sb_buf_write[512..1024])
                .map_err(|_| "Error writing superblock high")?;

            // Write gd back
            let mut gdt_buf_write = ::alloc::vec![0u8; gdt_blocks * block_size as usize];
            for g in 0..num_groups {
                let gd_offset = g * gd_size;
                let src_ptr = &gds[g] as *const GroupDescriptor as *const u8;
                // SAFETY: source is &gds[g], destination is inside gdt_buf_write, copy size is gd_size.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src_ptr,
                        gdt_buf_write.as_mut_ptr().add(gd_offset),
                        gd_size,
                    );
                }
            }
            for b in 0..gdt_blocks {
                let offset = b * block_size as usize;
                write_blocks(
                    &*device,
                    (gdt_block + b) as u64,
                    &gdt_buf_write[offset..(offset + block_size as usize)],
                    block_size,
                )?;
            }
        }

        // --- Consistency Check and Self-Healing (FSCK) ---
        let mut calc_block_bitmaps =
            ::alloc::vec![::alloc::vec![0u8; block_size as usize]; num_groups];
        let mut calc_inode_bitmaps =
            ::alloc::vec![::alloc::vec![0u8; block_size as usize]; num_groups];

        let it_blocks = match (s_inodes_per_group as u64).checked_mul(s_inode_size as u64) {
            Some(prod) => ((prod + block_size as u64 - 1) / block_size as u64) as u32,
            None => return Err("Overflow in metadata size calculation"),
        };

        // 1. Mark reserved metadata blocks as allocated
        for g in 0..num_groups {
            let start_block = (g as u32) * blocks_per_group + sb.s_first_data_block;
            let end_block =
                core::cmp::min(gds[g].bg_inode_table as u32 + it_blocks, sb.s_blocks_count);
            for b in start_block..end_block {
                let local_b = b - start_block;
                let byte = (local_b / 8) as usize;
                let bit = local_b % 8;
                if byte < calc_block_bitmaps[g].len() {
                    calc_block_bitmaps[g][byte] |= 1 << bit;
                }
            }
        }

        // 2. Mark reserved inodes (1 to 10) in group 0 as allocated
        for i in 0..10 {
            let byte = (i / 8) as usize;
            let bit = i % 8;
            calc_inode_bitmaps[0][byte] |= 1 << bit;
        }

        // 3. Scan all inodes from 2 to sb.s_inodes_count
        let mut block_cache_idx = 0u64;
        let mut block_cache_buf = ::alloc::vec![0u8; block_size as usize];

        for ino in 2..=sb.s_inodes_count {
            let group = ((ino - 1) / s_inodes_per_group) as usize;
            let index = (ino - 1) % s_inodes_per_group;
            if group >= num_groups {
                break;
            }
            let table_block = gds[group].bg_inode_table as u64;
            let inode_offset_in_table = (index * s_inode_size as u32) as u64;
            let logical_block = table_block + (inode_offset_in_table / block_size as u64);
            let offset_in_block = (inode_offset_in_table % block_size as u64) as usize;

            if block_cache_idx != logical_block {
                read_blocks(&*device, logical_block, &mut block_cache_buf, block_size)?;
                block_cache_idx = logical_block;
            }

            // SAFETY: block_cache_buf has size block_size, offset_in_block is within bounds, and raw inode layout matches ExtRawInode structure.
            let raw_inode = unsafe {
                core::ptr::read_unaligned(
                    block_cache_buf[offset_in_block..].as_ptr() as *const ExtRawInode
                )
            };

            if raw_inode.i_links_count > 0 {
                // Mark inode as allocated
                let i_idx = (ino - 1) % s_inodes_per_group;
                let byte = (i_idx / 8) as usize;
                let bit = i_idx % 8;
                if byte < calc_inode_bitmaps[group].len() {
                    calc_inode_bitmaps[group][byte] |= 1 << bit;
                }

                // Trace block pointers
                for file_block in 0..12 {
                    let block_num = raw_inode.i_block[file_block];
                    if block_num != 0 && block_num < sb.s_blocks_count {
                        let b_group =
                            ((block_num - sb.s_first_data_block) / blocks_per_group) as usize;
                        let local_b = (block_num - sb.s_first_data_block) % blocks_per_group;
                        let byte = (local_b / 8) as usize;
                        let bit = local_b % 8;
                        if b_group < num_groups && byte < calc_block_bitmaps[b_group].len() {
                            calc_block_bitmaps[b_group][byte] |= 1 << bit;
                        }
                    }
                }

                let sib = raw_inode.i_block[12];
                if sib != 0 && sib < sb.s_blocks_count {
                    // Mark indirect block as allocated
                    let sib_group = ((sib - sb.s_first_data_block) / blocks_per_group) as usize;
                    let sib_local = (sib - sb.s_first_data_block) % blocks_per_group;
                    let sib_byte = (sib_local / 8) as usize;
                    let sib_bit = sib_local % 8;
                    if sib_group < num_groups && sib_byte < calc_block_bitmaps[sib_group].len() {
                        calc_block_bitmaps[sib_group][sib_byte] |= 1 << sib_bit;
                    }

                    // Read indirect block and trace its pointers
                    let mut ind_buf = ::alloc::vec![0u8; block_size as usize];
                    if read_blocks(&*device, sib as u64, &mut ind_buf, block_size).is_ok() {
                        let refs_per_block = block_size / 4;
                        for r in 0..refs_per_block {
                            let ptr_offset = (r * 4) as usize;
                            let phys_block = u32::from_le_bytes([
                                ind_buf[ptr_offset],
                                ind_buf[ptr_offset + 1],
                                ind_buf[ptr_offset + 2],
                                ind_buf[ptr_offset + 3],
                            ]);
                            if phys_block != 0 && phys_block < sb.s_blocks_count {
                                let b_group = ((phys_block - sb.s_first_data_block)
                                    / blocks_per_group)
                                    as usize;
                                let local_b =
                                    (phys_block - sb.s_first_data_block) % blocks_per_group;
                                let byte = (local_b / 8) as usize;
                                let bit = local_b % 8;
                                if b_group < num_groups && byte < calc_block_bitmaps[b_group].len()
                                {
                                    calc_block_bitmaps[b_group][byte] |= 1 << bit;
                                }
                            }
                        }
                    }
                }

                let dib = raw_inode.i_block[13];
                if dib != 0 && dib < sb.s_blocks_count {
                    // Mark double indirect block as allocated
                    let dib_group = ((dib - sb.s_first_data_block) / blocks_per_group) as usize;
                    let dib_local = (dib - sb.s_first_data_block) % blocks_per_group;
                    let dib_byte = (dib_local / 8) as usize;
                    let dib_bit = dib_local % 8;
                    if dib_group < num_groups && dib_byte < calc_block_bitmaps[dib_group].len() {
                        calc_block_bitmaps[dib_group][dib_byte] |= 1 << dib_bit;
                    }

                    // Read double indirect block and trace its single indirect blocks
                    let mut dib_buf = ::alloc::vec![0u8; block_size as usize];
                    if read_blocks(&*device, dib as u64, &mut dib_buf, block_size).is_ok() {
                        let refs_per_block = block_size / 4;
                        for i in 0..refs_per_block {
                            let ptr_offset = (i * 4) as usize;
                            let sib = u32::from_le_bytes([
                                dib_buf[ptr_offset],
                                dib_buf[ptr_offset + 1],
                                dib_buf[ptr_offset + 2],
                                dib_buf[ptr_offset + 3],
                            ]);
                            if sib != 0 && sib < sb.s_blocks_count {
                                // Mark indirect block as allocated
                                let sib_group =
                                    ((sib - sb.s_first_data_block) / blocks_per_group) as usize;
                                let sib_local = (sib - sb.s_first_data_block) % blocks_per_group;
                                let sib_byte = (sib_local / 8) as usize;
                                let sib_bit = sib_local % 8;
                                if sib_group < num_groups
                                    && sib_byte < calc_block_bitmaps[sib_group].len()
                                {
                                    calc_block_bitmaps[sib_group][sib_byte] |= 1 << sib_bit;
                                }

                                // Read indirect block and trace its pointers
                                let mut ind_buf = ::alloc::vec![0u8; block_size as usize];
                                if read_blocks(&*device, sib as u64, &mut ind_buf, block_size)
                                    .is_ok()
                                {
                                    for r in 0..refs_per_block {
                                        let ptr_offset2 = (r * 4) as usize;
                                        let phys_block = u32::from_le_bytes([
                                            ind_buf[ptr_offset2],
                                            ind_buf[ptr_offset2 + 1],
                                            ind_buf[ptr_offset2 + 2],
                                            ind_buf[ptr_offset2 + 3],
                                        ]);
                                        if phys_block != 0 && phys_block < sb.s_blocks_count {
                                            let b_group = ((phys_block - sb.s_first_data_block)
                                                / blocks_per_group)
                                                as usize;
                                            let local_b = (phys_block - sb.s_first_data_block)
                                                % blocks_per_group;
                                            let byte = (local_b / 8) as usize;
                                            let bit = local_b % 8;
                                            if b_group < num_groups
                                                && byte < calc_block_bitmaps[b_group].len()
                                            {
                                                calc_block_bitmaps[b_group][byte] |= 1 << bit;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Initialize padding bits for calculated bitmaps
        for g in 0..num_groups {
            let group_blocks = if g == num_groups - 1 {
                sb.s_blocks_count - sb.s_first_data_block - (g as u32) * blocks_per_group
            } else {
                blocks_per_group
            };
            for b in group_blocks..((block_size * 8) as u32) {
                let byte = (b / 8) as usize;
                let bit = b % 8;
                if byte < calc_block_bitmaps[g].len() {
                    calc_block_bitmaps[g][byte] |= 1 << bit;
                }
            }

            let group_inodes = if g == num_groups - 1 {
                sb.s_inodes_count - (g as u32) * s_inodes_per_group
            } else {
                s_inodes_per_group
            };
            for i in group_inodes..((block_size * 8) as u32) {
                let byte = (i / 8) as usize;
                let bit = i % 8;
                if byte < calc_inode_bitmaps[g].len() {
                    calc_inode_bitmaps[g][byte] |= 1 << bit;
                }
            }
        }

        // 4. Compare bitmaps and self-heal if necessary
        let mut mismatch = false;
        for g in 0..num_groups {
            let gd = &gds[g];
            let mut block_bitmap = ::alloc::vec![0u8; block_size as usize];
            read_blocks(
                &*device,
                gd.bg_block_bitmap as u64,
                &mut block_bitmap,
                block_size,
            )?;
            if block_bitmap != calc_block_bitmaps[g] {
                mismatch = true;
                break;
            }

            let mut inode_bitmap = ::alloc::vec![0u8; block_size as usize];
            read_blocks(
                &*device,
                gd.bg_inode_bitmap as u64,
                &mut inode_bitmap,
                block_size,
            )?;
            if inode_bitmap != calc_inode_bitmaps[g] {
                mismatch = true;
                break;
            }
        }

        if mismatch {
            kprintln!("[ext] Integrity mismatch found. Self-healing filesystem metadata...");

            let mut total_free_blocks = 0u32;
            let mut total_free_inodes = 0u32;

            for g in 0..num_groups {
                let gd = &mut gds[g];
                write_blocks(
                    &*device,
                    gd.bg_block_bitmap as u64,
                    &calc_block_bitmaps[g],
                    block_size,
                )?;
                write_blocks(
                    &*device,
                    gd.bg_inode_bitmap as u64,
                    &calc_inode_bitmaps[g],
                    block_size,
                )?;

                let group_blocks = if g == num_groups - 1 {
                    sb.s_blocks_count - sb.s_first_data_block - (g as u32) * blocks_per_group
                } else {
                    blocks_per_group
                };
                let group_inodes = if g == num_groups - 1 {
                    sb.s_inodes_count - (g as u32) * s_inodes_per_group
                } else {
                    s_inodes_per_group
                };

                let free_b = count_free_bits(&calc_block_bitmaps[g], group_blocks);
                let free_i = count_free_bits(&calc_inode_bitmaps[g], group_inodes);
                gd.bg_free_blocks_count = free_b as u16;
                gd.bg_free_inodes_count = free_i as u16;

                total_free_blocks += free_b;
                total_free_inodes += free_i;
            }

            sb.s_free_blocks_count = total_free_blocks;
            sb.s_free_inodes_count = total_free_inodes;

            // Write superblock back
            let sb_ptr = &sb as *const Superblock as *const u8;
            let mut sb_buf_write = [0u8; 1024];
            // SAFETY: sb is stack-allocated, sb_buf_write is 1024 bytes, copy is within bounds.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    sb_ptr,
                    sb_buf_write.as_mut_ptr(),
                    core::mem::size_of::<Superblock>(),
                );
            }
            device
                .write_block(2, &sb_buf_write[0..512])
                .map_err(|_| "Error writing superblock low")?;
            device
                .write_block(3, &sb_buf_write[512..1024])
                .map_err(|_| "Error writing superblock high")?;

            // Write gd back
            let mut gdt_buf_write = ::alloc::vec![0u8; gdt_blocks * block_size as usize];
            for g in 0..num_groups {
                let gd_offset = g * gd_size;
                let src_ptr = &gds[g] as *const GroupDescriptor as *const u8;
                // SAFETY: source is &gds[g], destination is inside gdt_buf_write, copy size is gd_size.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src_ptr,
                        gdt_buf_write.as_mut_ptr().add(gd_offset),
                        gd_size,
                    );
                }
            }
            for b in 0..gdt_blocks {
                let offset = b * block_size as usize;
                write_blocks(
                    &*device,
                    (gdt_block + b) as u64,
                    &gdt_buf_write[offset..(offset + block_size as usize)],
                    block_size,
                )?;
            }
        } else {
            kprintln!("[ext] Filesystem consistency check succeeded. No corruption detected.");
        }

        let fs = Arc::new(Self {
            device: device.clone(),
            block_size,
            inodes_per_block,
            inodes_per_group,
            inode_size,
            superblock: TicketLock::new(sb),
            group_descriptors: TicketLock::new(gds),
            root_node: TicketLock::new(None),
        });

        // Parse JBD2 Journal if HAS_JOURNAL feature is set
        if (sb.s_feature_compat & 0x0004) != 0 {
            kprintln!("[ext4] Superblock has journal feature compat flag.");
            let journal_ino = 8;
            let journal_inode = fs.get_ext_inode(journal_ino)?;
            let phys_block = journal_inode.resolve_block(0)?;
            if phys_block == 0 {
                return Err("Journal inode block 0 is not mapped");
            }
            let mut jsb_buf = ::alloc::vec![0u8; block_size as usize];
            read_blocks(&*device, phys_block as u64, &mut jsb_buf, block_size)?;
            // SAFETY: jsb_buf is allocated with size block_size, which is at least 1024 bytes, matching JBD2 superblock layout.
            let jsb =
                unsafe { core::ptr::read_unaligned(jsb_buf.as_ptr() as *const JournalSuperblock) };
            let magic = u32::from_be(jsb.s_header.h_magic);
            if magic != 0xC03B3998 {
                return Err("Invalid JBD2 journal superblock magic");
            }
            let j_blocksize = u32::from_be(jsb.s_blocksize);
            let j_start = u32::from_be(jsb.s_start);
            kprintln!(
                "[ext4] JBD2 Journal Superblock magic verified. Block Size: {}, Start Block: {}",
                j_blocksize,
                j_start
            );
            if j_start != 0 {
                kprintln!("[ext4] WARNING: Journal has active transactions (start block {}). Mounting anyway (clean state default).", j_start);
            } else {
                kprintln!("[ext4] Journal is clean.");
            }
        }

        // Parse root directory (Inode 2)
        let root = fs.get_inode(2)?;
        *fs.root_node.lock() = Some(root);

        Ok(fs)
    }

    /// Retrieve raw ext inode wrapper.
    pub fn get_ext_inode(self: &Arc<Self>, ino: u32) -> Result<ExtInode, &'static str> {
        if ino == 0 {
            return Err("Invalid inode number 0");
        }

        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;

        let gd = {
            let gds = self.group_descriptors.lock();
            gds.get(group as usize)
                .copied()
                .ok_or("Group descriptor index out of bounds")?
        };

        let table_block = gd.bg_inode_table as u64;
        let inode_offset_in_table = (index * self.inode_size as u32) as u64;

        let logical_block = table_block + (inode_offset_in_table / self.block_size as u64);
        let offset_in_block = (inode_offset_in_table % self.block_size as u64) as usize;

        let mut block_buf = ::alloc::vec![0u8; self.block_size as usize];
        read_blocks(
            &*self.device,
            logical_block,
            &mut block_buf,
            self.block_size,
        )?;

        // SAFETY: block_buf is allocated with size block_size, offset_in_block is within bounds, and layout matches ExtRawInode.
        let raw_inode = unsafe {
            core::ptr::read_unaligned(block_buf[offset_in_block..].as_ptr() as *const ExtRawInode)
        };

        let i_mode = raw_inode.i_mode;
        let i_size = raw_inode.i_size;
        let i_links_count = raw_inode.i_links_count;
        let i_uid = raw_inode.i_uid;
        let i_gid = raw_inode.i_gid;
        let i_blocks = raw_inode.i_blocks;

        let file_type = match i_mode & 0xF000 {
            0x8000 => FileType::Regular,
            0x4000 => FileType::Directory,
            0xA000 => FileType::Symlink,
            _ => FileType::Regular,
        };

        let mut inode = Inode::new(ino as u64, file_type);
        inode.size = i_size as u64;
        inode.permissions = FilePermissions::new(i_mode);
        inode.nlink = i_links_count as u32;
        inode.uid = i_uid as u32;
        inode.gid = i_gid as u32;
        inode.blocks = i_blocks as u64;
        inode.atime = raw_inode.i_atime as u64;
        inode.mtime = raw_inode.i_mtime as u64;
        inode.ctime = raw_inode.i_ctime as u64;

        Ok(ExtInode {
            fs: self.clone(),
            ino,
            raw: TicketLock::new(raw_inode),
            vfs_inode: RwLock::new(inode),
        })
    }

    /// Retrieve an inode by its number.
    pub fn get_inode(self: &Arc<Self>, ino: u32) -> Result<Arc<dyn InodeOps>, &'static str> {
        let ext_inode = self.get_ext_inode(ino)?;
        Ok(Arc::new(ext_inode))
    }

    /// Write superblock back to the block device.
    pub fn write_superblock(&self, sb: &Superblock) -> Result<(), &'static str> {
        let sb_ptr = sb as *const Superblock as *const u8;
        let mut sb_buf = [0u8; 1024];
        // SAFETY: sb is stack-allocated, sb_buf is 1024 bytes, copy size is core::mem::size_of::<Superblock>() which is less than 1024.
        unsafe {
            core::ptr::copy_nonoverlapping(
                sb_ptr,
                sb_buf.as_mut_ptr(),
                core::mem::size_of::<Superblock>(),
            );
        }
        self.device
            .write_block(2, &sb_buf[0..512])
            .map_err(|_| "Error writing superblock low")?;
        self.device
            .write_block(3, &sb_buf[512..1024])
            .map_err(|_| "Error writing superblock high")?;
        Ok(())
    }

    /// Write group descriptors back to the block device.
    pub fn write_group_descriptors(&self) -> Result<(), &'static str> {
        let gds = self.group_descriptors.lock();
        let block_size = self.block_size;
        let gdt_block = if block_size == 1024 { 2 } else { 1 };

        let gd_size = core::mem::size_of::<GroupDescriptor>();
        let gdt_size = gds.len() * gd_size;
        let gdt_blocks = (gdt_size + block_size as usize - 1) / block_size as usize;

        let mut gdt_buf = ::alloc::vec![0u8; gdt_blocks * block_size as usize];
        for (i, gd) in gds.iter().enumerate() {
            let offset = i * gd_size;
            let src_ptr = gd as *const GroupDescriptor as *const u8;
            // SAFETY: gd is valid for reading, offset + gd_size is within gdt_buf bounds since gdt_buf size is gdt_blocks * block_size.
            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, gdt_buf.as_mut_ptr().add(offset), gd_size);
            }
        }

        for b in 0..gdt_blocks {
            let offset = b * block_size as usize;
            write_blocks(
                &*self.device,
                (gdt_block + b) as u64,
                &gdt_buf[offset..(offset + block_size as usize)],
                block_size,
            )?;
        }
        Ok(())
    }
}

/// ext Inode wrapper implementing InodeOps.
pub struct ExtInode {
    pub(crate) fs: Arc<ExtFileSystem>,
    pub(crate) ino: u32,
    pub(crate) raw: TicketLock<ExtRawInode>,
    pub(crate) vfs_inode: RwLock<Inode>,
}

impl InodeOps for ExtInode {
    fn inode(&self) -> &Inode {
        // SAFETY: The reference to Inode is protected by RwLock. Reading allows shared re-entrant access on the same thread.
        // We cast the reference to a raw pointer to satisfy the trait signature.
        unsafe { &*(&*self.vfs_inode.read() as *const Inode) }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if self.inode().file_type == FileType::Regular {
            self.read_page_cache(offset, buf)
        } else {
            self.read_file(offset, buf)
        }
    }

    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, i32> {
        if self.inode().file_type == FileType::Regular {
            self.write_page_cache(offset, buf)
        } else {
            self.write_file(offset, buf)
        }
    }

    fn read_direct(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        self.read_file(offset, buf)
    }

    fn write_direct(&self, offset: u64, data: &[u8]) -> Result<usize, i32> {
        self.write_file(offset, data)
    }

    fn set_permissions(&self, mode: u16) -> Result<(), i32> {
        let mut vfs = self.vfs_inode.write();
        let mut raw = self.raw.lock();
        let new_mode = (raw.i_mode & 0xF000) | (mode & 0x0FFF);
        raw.i_mode = new_mode;
        vfs.permissions.mode = new_mode;
        self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?; // -EIO
        Ok(())
    }

    fn set_owner(&self, uid: u32, gid: u32) -> Result<(), i32> {
        let mut vfs = self.vfs_inode.write();
        let mut raw = self.raw.lock();
        raw.i_uid = uid as u16;
        raw.i_gid = gid as u16;
        vfs.uid = uid;
        vfs.gid = gid;
        self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?; // -EIO
        Ok(())
    }

    fn create(&self, name: &str, file_type: FileType) -> Option<Arc<dyn InodeOps>> {
        self.create_dir_entry(name, file_type)
    }

    fn unlink(&self, name: &str) -> Result<(), i32> {
        self.unlink_dir_entry(name)
    }

    fn mkdir(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        self.mkdir_dir_entry(name)
    }

    fn rmdir(&self, name: &str) -> Result<(), i32> {
        self.rmdir_dir_entry(name)
    }

    fn readdir(&self) -> Vec<DirEntry> {
        self.readdir_dir_entry()
    }

    fn lookup(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        self.lookup_dir_entry(name)
    }

    fn truncate(&self, size: u64) -> Result<(), i32> {
        self.truncate_file(size)
    }
}

impl FileSystem for ExtFileSystem {
    fn root(&self) -> Option<Arc<dyn InodeOps>> {
        self.root_node.lock().clone()
    }

    fn name(&self) -> &str {
        "ext"
    }

    fn statfs(&self) -> FsStats {
        let sb = self.superblock.lock();
        FsStats {
            total_blocks: sb.s_blocks_count as u64,
            free_blocks: sb.s_free_blocks_count as u64,
            total_inodes: sb.s_inodes_count as u64,
            free_inodes: sb.s_free_inodes_count as u64,
            block_size: self.block_size as u64,
            max_name_len: 255,
        }
    }
}
