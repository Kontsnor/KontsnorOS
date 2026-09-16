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
use ::alloc::collections::BTreeMap;
use ::alloc::sync::{Arc, Weak};
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

/// Filesystem device ID for primary ext block filesystem.
pub const EXT_DEV_ID: u64 = 1;

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

/// Safely deserialize `ExtRawInode` respecting `inode_size` and buffer bounds.
pub(crate) fn read_raw_inode(buf: &[u8], offset: usize, inode_size: usize) -> ExtRawInode {
    let mut raw = ExtRawInode::zeroed();
    let copy_size = core::cmp::min(inode_size, core::mem::size_of::<ExtRawInode>());
    let available = buf.len().saturating_sub(offset);
    let actual_copy = core::cmp::min(copy_size, available);
    let dst = &mut raw as *mut ExtRawInode as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(buf[offset..].as_ptr(), dst, actual_copy);
    }
    raw
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

/// Mark a physical disk block as allocated in the calculated group block bitmaps.
fn mark_phys_block(
    calc_block_bitmaps: &mut [Vec<u8>],
    phys_block: u64,
    first_data_block: u32,
    blocks_per_group: u32,
    total_blocks: u64,
) {
    if phys_block >= first_data_block as u64 && phys_block < total_blocks {
        let b_group = ((phys_block - first_data_block as u64) / blocks_per_group as u64) as usize;
        let local_b = ((phys_block - first_data_block as u64) % blocks_per_group as u64) as usize;
        let byte = local_b / 8;
        let bit = local_b % 8;
        if b_group < calc_block_bitmaps.len() && byte < calc_block_bitmaps[b_group].len() {
            calc_block_bitmaps[b_group][byte] |= 1 << bit;
        }
    }
}

/// Trace physical blocks allocated in an extent tree for FSCK.
fn trace_extent_tree_blocks(
    device: &dyn BlockDevice,
    block_size: u32,
    num_groups: usize,
    blocks_per_group: u32,
    first_data_block: u32,
    total_blocks: u64,
    i_block: &[u32; 15],
    calc_block_bitmaps: &mut [Vec<u8>],
) {
    let mut root_buf = [0u8; 60];
    for i in 0..15 {
        root_buf[i * 4..i * 4 + 4].copy_from_slice(&i_block[i].to_le_bytes());
    }

    fn trace_node(
        device: &dyn BlockDevice,
        block_size: u32,
        num_groups: usize,
        blocks_per_group: u32,
        first_data_block: u32,
        total_blocks: u64,
        buf: &[u8],
        calc_block_bitmaps: &mut [Vec<u8>],
    ) {
        if buf.len() < 12 {
            return;
        }
        let header =
            unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const types::Ext4ExtentHeader) };
        if header.eh_magic != 0xF30A {
            return;
        }

        let entries = header.eh_entries as usize;
        let depth = header.eh_depth;

        if depth == 0 {
            let entry_size = core::mem::size_of::<types::Ext4Extent>();
            for i in 0..entries {
                let offset = 12 + i * entry_size;
                if offset + entry_size <= buf.len() {
                    let ext = unsafe {
                        core::ptr::read_unaligned(buf[offset..].as_ptr() as *const types::Ext4Extent)
                    };
                    let start = ext.start_block();
                    let len = ext.len() as u64;
                    for b in 0..len {
                        mark_phys_block(
                            calc_block_bitmaps,
                            start + b,
                            first_data_block,
                            blocks_per_group,
                            total_blocks,
                        );
                    }
                }
            }
        } else {
            let entry_size = core::mem::size_of::<types::Ext4ExtentIdx>();
            for i in 0..entries {
                let offset = 12 + i * entry_size;
                if offset + entry_size <= buf.len() {
                    let idx = unsafe {
                        core::ptr::read_unaligned(
                            buf[offset..].as_ptr() as *const types::Ext4ExtentIdx
                        )
                    };
                    let child_block = idx.leaf_block();
                    mark_phys_block(
                        calc_block_bitmaps,
                        child_block,
                        first_data_block,
                        blocks_per_group,
                        total_blocks,
                    );

                    let mut child_buf = ::alloc::vec![0u8; block_size as usize];
                    if read_blocks(device, child_block, &mut child_buf, block_size).is_ok() {
                        trace_node(
                            device,
                            block_size,
                            num_groups,
                            blocks_per_group,
                            first_data_block,
                            total_blocks,
                            &child_buf,
                            calc_block_bitmaps,
                        );
                    }
                }
            }
        }
    }

    trace_node(
        device,
        block_size,
        num_groups,
        blocks_per_group,
        first_data_block,
        total_blocks,
        &root_buf[..60],
        calc_block_bitmaps,
    );
}

/// ext FileSystem implementation.
pub struct ExtFileSystem {
    pub(crate) device: Arc<dyn BlockDevice>,
    pub(crate) block_size: u32,
    pub(crate) inodes_per_block: u32,
    pub(crate) inodes_per_group: u32,
    pub(crate) inode_size: u16,
    pub(crate) is_64bit: bool,
    pub(crate) gd_size: usize,
    pub(crate) superblock: TicketLock<Superblock>,
    pub(crate) group_descriptors: TicketLock<Vec<GroupDescriptor>>,
    pub(crate) root_node: TicketLock<Option<Arc<dyn InodeOps>>>,
    pub(crate) self_weak: spin::Mutex<Option<::alloc::sync::Weak<ExtFileSystem>>>,
    pub(crate) inode_cache: TicketLock<BTreeMap<u32, Weak<ExtInode>>>,
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

        let supported_incompat = types::INCOMPAT_FILETYPE
            | types::INCOMPAT_RECOVER
            | types::INCOMPAT_JOURNAL_DEV
            | types::INCOMPAT_EXTENTS
            | types::INCOMPAT_64BIT
            | types::INCOMPAT_FLEX_BG
            | types::INCOMPAT_CSUM_SEED
            | types::INCOMPAT_LARGEDIR
            | types::INCOMPAT_INLINE_DATA;

        if (sb.s_feature_incompat & !supported_incompat) != 0 {
            return Err("Unsupported ext incompat feature flags");
        }

        let supported_ro_compat = types::RO_COMPAT_SPARSE_SUPER
            | types::RO_COMPAT_LARGE_FILE
            | types::RO_COMPAT_BTREE_DIR
            | types::RO_COMPAT_HUGE_FILE
            | types::RO_COMPAT_GDT_CSUM
            | types::RO_COMPAT_DIR_NLINK
            | types::RO_COMPAT_EXTRA_ISIZE
            | types::RO_COMPAT_QUOTA
            | types::RO_COMPAT_BIGALLOC
            | types::RO_COMPAT_METADATA_CSUM
            | types::RO_COMPAT_READONLY
            | types::RO_COMPAT_PROJECT;

        if (sb.s_feature_ro_compat & !supported_ro_compat) != 0 {
            return Err("Unsupported ext ro_compat feature flags");
        }

        let is_64bit = (sb.s_feature_incompat & types::INCOMPAT_64BIT) != 0;
        let gd_size = sb.desc_size();

        let blocks_per_group = sb.s_blocks_per_group;
        let total_blocks = sb.total_blocks();
        let num_groups = core::cmp::max(
            1,
            ((total_blocks - sb.s_first_data_block as u64 + blocks_per_group as u64 - 1)
                / blocks_per_group as u64) as usize,
        );

        let block_size = 1024 << s_log_block_size;
        let inode_size = s_inode_size;
        let inodes_per_group = s_inodes_per_group;
        let inodes_per_block = block_size / inode_size as u32;

        kprintln!(
            "[ext] Volume detected. s_magic: {:#x}, Block Size: {}, Inode Size: {}, 64bit: {}",
            s_magic,
            block_size,
            inode_size,
            is_64bit
        );

        // Read Group Descriptor Table
        let gdt_block = if block_size == 1024 { 2 } else { 1 };
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
            let mut gd = GroupDescriptor {
                bg_block_bitmap: 0,
                bg_inode_bitmap: 0,
                bg_inode_table: 0,
                bg_free_blocks_count: 0,
                bg_free_inodes_count: 0,
                bg_used_dirs_count: 0,
                bg_pad: 0,
                bg_reserved: [0; 12],
                bg_block_bitmap_hi: 0,
                bg_inode_bitmap_hi: 0,
                bg_inode_table_hi: 0,
                bg_free_blocks_count_hi: 0,
                bg_free_inodes_count_hi: 0,
                bg_used_dirs_count_hi: 0,
                bg_itable_unused_hi: 0,
                bg_exclude_bitmap_hi: 0,
                bg_block_bitmap_csum_hi: 0,
                bg_inode_bitmap_csum_hi: 0,
                bg_reserved_64: 0,
            };

            let src_slice = &gdt_buf[gd_offset..gd_offset + gd_size];
            let dst_ptr = &mut gd as *mut GroupDescriptor as *mut u8;
            // SAFETY: Copy gd_size bytes from gdt_buf into local struct gd.
            unsafe {
                core::ptr::copy_nonoverlapping(src_slice.as_ptr(), dst_ptr, gd_size);
            }

            // Validate GDT offsets are within filesystem bounds
            if gd.block_bitmap(is_64bit) >= total_blocks
                || gd.inode_bitmap(is_64bit) >= total_blocks
                || gd.inode_table(is_64bit) >= total_blocks
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
                gd.block_bitmap(is_64bit),
                &mut block_bitmap,
                block_size,
            )?;
            let mut inode_bitmap = ::alloc::vec![0u8; block_size as usize];
            read_blocks(
                &*device,
                gd.inode_bitmap(is_64bit),
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
                let inode_table_loc = gd.inode_table(is_64bit);
                let end_block = core::cmp::min(inode_table_loc + it_blocks as u64, total_blocks);
                for b in start_block as u64..end_block {
                    if b >= start_block as u64 && b < (start_block + blocks_per_group) as u64 {
                        let local_b = (b - start_block as u64) as usize;
                        let byte = local_b / 8;
                        let bit = local_b % 8;
                        if byte < block_bitmap.len() {
                            block_bitmap[byte] |= 1 << bit;
                        }
                    }
                }

                // Initialize padding bits for block bitmap
                let group_blocks = if g == num_groups - 1 {
                    (total_blocks
                        - sb.s_first_data_block as u64
                        - (g as u64) * blocks_per_group as u64) as u32
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
                    gd.block_bitmap(is_64bit),
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
                    gd.inode_bitmap(is_64bit),
                    &inode_bitmap,
                    block_size,
                )?;
                inode_bitmap_changed = true;
            }

            if block_bitmap_changed || inode_bitmap_changed {
                let group_blocks = if g == num_groups - 1 {
                    (total_blocks
                        - sb.s_first_data_block as u64
                        - (g as u64) * blocks_per_group as u64) as u32
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
                gd.set_free_blocks_count(is_64bit, free_b);
                gd.set_free_inodes_count(is_64bit, free_i);
                bitmap_healed = true;
            }
        }

        if bitmap_healed {
            let mut total_free_blocks = 0u64;
            let mut total_free_inodes = 0u32;
            for g in 0..num_groups {
                total_free_blocks += gds[g].free_blocks_count(is_64bit) as u64;
                total_free_inodes += gds[g].free_inodes_count(is_64bit);
            }
            sb.set_free_blocks(total_free_blocks);
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

        // 1. Mark reserved metadata blocks as allocated across all groups (accounting for flex_bg placement)
        for g in 0..num_groups {
            let gd = &gds[g];

            // Block bitmap for group g
            mark_phys_block(
                &mut calc_block_bitmaps,
                gd.block_bitmap(is_64bit),
                sb.s_first_data_block,
                blocks_per_group,
                total_blocks,
            );

            // Inode bitmap for group g
            mark_phys_block(
                &mut calc_block_bitmaps,
                gd.inode_bitmap(is_64bit),
                sb.s_first_data_block,
                blocks_per_group,
                total_blocks,
            );

            // Inode table blocks for group g
            let it_start = gd.inode_table(is_64bit);
            for b in 0..it_blocks as u64 {
                mark_phys_block(
                    &mut calc_block_bitmaps,
                    it_start + b,
                    sb.s_first_data_block,
                    blocks_per_group,
                    total_blocks,
                );
            }

            // Check if group g has superblock + GDT backup blocks
            let group_start_block =
                (g as u64) * blocks_per_group as u64 + sb.s_first_data_block as u64;
            let is_sparse_bg = (sb.s_feature_ro_compat & types::RO_COMPAT_SPARSE_SUPER) != 0;
            let has_super = if !is_sparse_bg {
                true
            } else if g == 0 || g == 1 {
                true
            } else {
                fn is_power_of(mut n: usize, base: usize) -> bool {
                    if n == 0 {
                        return false;
                    }
                    while n % base == 0 {
                        n /= base;
                    }
                    n == 1
                }
                is_power_of(g, 3) || is_power_of(g, 5) || is_power_of(g, 7)
            };

            if has_super {
                mark_phys_block(
                    &mut calc_block_bitmaps,
                    group_start_block,
                    sb.s_first_data_block,
                    blocks_per_group,
                    total_blocks,
                );
                for gb in 1..=gdt_blocks as u64 {
                    mark_phys_block(
                        &mut calc_block_bitmaps,
                        group_start_block + gb,
                        sb.s_first_data_block,
                        blocks_per_group,
                        total_blocks,
                    );
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
            let table_block = gds[group].inode_table(is_64bit);
            let inode_offset_in_table = (index * s_inode_size as u32) as u64;
            let logical_block = table_block + (inode_offset_in_table / block_size as u64);
            let offset_in_block = (inode_offset_in_table % block_size as u64) as usize;

            if block_cache_idx != logical_block {
                read_blocks(&*device, logical_block, &mut block_cache_buf, block_size)?;
                block_cache_idx = logical_block;
            }

            let raw_inode =
                read_raw_inode(&block_cache_buf, offset_in_block, s_inode_size as usize);

            if raw_inode.i_links_count > 0 && raw_inode.i_mode != 0 {
                // Mark inode as allocated
                let i_idx = (ino - 1) % s_inodes_per_group;
                let byte = (i_idx / 8) as usize;
                let bit = i_idx % 8;
                if byte < calc_inode_bitmaps[group].len() {
                    calc_inode_bitmaps[group][byte] |= 1 << bit;
                }

                if (raw_inode.i_flags & types::EXT4_EXTENTS_FL) != 0 {
                    // Traverse extent tree for FSCK
                    let i_block = raw_inode.i_block;
                    trace_extent_tree_blocks(
                        &*device,
                        block_size,
                        num_groups,
                        blocks_per_group,
                        sb.s_first_data_block,
                        total_blocks,
                        &i_block,
                        &mut calc_block_bitmaps,
                    );
                } else {
                    // Trace block pointers
                    for file_block in 0..12 {
                        let block_num = raw_inode.i_block[file_block] as u64;
                        mark_phys_block(
                            &mut calc_block_bitmaps,
                            block_num,
                            sb.s_first_data_block,
                            blocks_per_group,
                            total_blocks,
                        );
                    }

                    let sib = raw_inode.i_block[12] as u64;
                    if sib != 0 && sib < total_blocks {
                        mark_phys_block(
                            &mut calc_block_bitmaps,
                            sib,
                            sb.s_first_data_block,
                            blocks_per_group,
                            total_blocks,
                        );

                        // Read indirect block and trace its pointers
                        let mut ind_buf = ::alloc::vec![0u8; block_size as usize];
                        if read_blocks(&*device, sib, &mut ind_buf, block_size).is_ok() {
                            let refs_per_block = block_size / 4;
                            for r in 0..refs_per_block {
                                let ptr_offset = (r * 4) as usize;
                                let phys_block = u32::from_le_bytes([
                                    ind_buf[ptr_offset],
                                    ind_buf[ptr_offset + 1],
                                    ind_buf[ptr_offset + 2],
                                    ind_buf[ptr_offset + 3],
                                ]) as u64;
                                mark_phys_block(
                                    &mut calc_block_bitmaps,
                                    phys_block,
                                    sb.s_first_data_block,
                                    blocks_per_group,
                                    total_blocks,
                                );
                            }
                        }
                    }

                    let dib = raw_inode.i_block[13] as u64;
                    if dib != 0 && dib < total_blocks {
                        mark_phys_block(
                            &mut calc_block_bitmaps,
                            dib,
                            sb.s_first_data_block,
                            blocks_per_group,
                            total_blocks,
                        );

                        // Read double indirect block and trace its single indirect blocks
                        let mut dib_buf = ::alloc::vec![0u8; block_size as usize];
                        if read_blocks(&*device, dib, &mut dib_buf, block_size).is_ok() {
                            let refs_per_block = block_size / 4;
                            for i in 0..refs_per_block {
                                let ptr_offset = (i * 4) as usize;
                                let sib = u32::from_le_bytes([
                                    dib_buf[ptr_offset],
                                    dib_buf[ptr_offset + 1],
                                    dib_buf[ptr_offset + 2],
                                    dib_buf[ptr_offset + 3],
                                ]) as u64;
                                if sib != 0 && sib < total_blocks {
                                    mark_phys_block(
                                        &mut calc_block_bitmaps,
                                        sib,
                                        sb.s_first_data_block,
                                        blocks_per_group,
                                        total_blocks,
                                    );

                                    // Read indirect block and trace its pointers
                                    let mut ind_buf = ::alloc::vec![0u8; block_size as usize];
                                    if read_blocks(&*device, sib, &mut ind_buf, block_size).is_ok()
                                    {
                                        for r in 0..refs_per_block {
                                            let ptr_offset2 = (r * 4) as usize;
                                            let phys_block = u32::from_le_bytes([
                                                ind_buf[ptr_offset2],
                                                ind_buf[ptr_offset2 + 1],
                                                ind_buf[ptr_offset2 + 2],
                                                ind_buf[ptr_offset2 + 3],
                                            ])
                                                as u64;
                                            mark_phys_block(
                                                &mut calc_block_bitmaps,
                                                phys_block,
                                                sb.s_first_data_block,
                                                blocks_per_group,
                                                total_blocks,
                                            );
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
                (total_blocks - sb.s_first_data_block as u64 - (g as u64) * blocks_per_group as u64)
                    as u32
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
                gd.block_bitmap(is_64bit),
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
                gd.inode_bitmap(is_64bit),
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

            let mut total_free_blocks = 0u64;
            let mut total_free_inodes = 0u32;

            for g in 0..num_groups {
                let gd = &mut gds[g];
                write_blocks(
                    &*device,
                    gd.block_bitmap(is_64bit),
                    &calc_block_bitmaps[g],
                    block_size,
                )?;
                write_blocks(
                    &*device,
                    gd.inode_bitmap(is_64bit),
                    &calc_inode_bitmaps[g],
                    block_size,
                )?;

                let group_blocks = if g == num_groups - 1 {
                    (total_blocks
                        - sb.s_first_data_block as u64
                        - (g as u64) * blocks_per_group as u64) as u32
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
                gd.set_free_blocks_count(is_64bit, free_b);
                gd.set_free_inodes_count(is_64bit, free_i);

                total_free_blocks += free_b as u64;
                total_free_inodes += free_i;
            }

            sb.set_free_blocks(total_free_blocks);
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
            is_64bit,
            gd_size,
            superblock: TicketLock::new(sb),
            group_descriptors: TicketLock::new(gds),
            root_node: TicketLock::new(None),
            self_weak: spin::Mutex::new(None),
            inode_cache: TicketLock::new(BTreeMap::new()),
        });

        *fs.self_weak.lock() = Some(Arc::downgrade(&fs));

        // Parse JBD2 Journal if HAS_JOURNAL feature is set
        if (sb.s_feature_compat & 0x0004) != 0 {
            kprintln!("[ext4] Superblock has journal feature compat flag.");
            let journal_ino = 8;
            let journal_inode = fs.get_ext_inode(journal_ino)?;
            let phys_block = journal_inode.resolve_block(0)?;
            if phys_block != 0 {
                let mut jsb_buf = ::alloc::vec![0u8; block_size as usize];
                read_blocks(&*device, phys_block as u64, &mut jsb_buf, block_size)?;
                // SAFETY: jsb_buf is allocated with size block_size, which is at least 1024 bytes, matching JBD2 superblock layout.
                let jsb = unsafe {
                    core::ptr::read_unaligned(jsb_buf.as_ptr() as *const JournalSuperblock)
                };
                let magic = u32::from_be(jsb.s_header.h_magic);
                if magic == 0xC03B3998 {
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

        let table_block = gd.inode_table(self.is_64bit);
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

        let raw_inode = read_raw_inode(&block_buf, offset_in_block, self.inode_size as usize);

        let i_mode = raw_inode.i_mode;
        let i_size = raw_inode.file_size();
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
        inode.dev = EXT_DEV_ID;
        inode.size = i_size;
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
        {
            let cache = self.inode_cache.lock();
            if let Some(weak) = cache.get(&ino) {
                if let Some(arc) = weak.upgrade() {
                    return Ok(arc);
                }
            }
        }

        let ext_inode = self.get_ext_inode(ino)?;
        let arc = Arc::new(ext_inode);
        let weak = Arc::downgrade(&arc);

        let mut cache = self.inode_cache.lock();
        if let Some(existing_weak) = cache.get(&ino) {
            if let Some(existing) = existing_weak.upgrade() {
                return Ok(existing);
            }
        }
        cache.insert(ino, weak);
        Ok(arc)
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

        let gd_size = self.gd_size;
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

    fn set_times(&self, atime: u64, mtime: u64) -> Result<(), i32> {
        let mut vfs = self.vfs_inode.write();
        let mut raw = self.raw.lock();
        raw.i_atime = atime as u32;
        raw.i_mtime = mtime as u32;
        let now = crate::fs::vfs::current_time_sec();
        raw.i_ctime = now;
        vfs.atime = atime;
        vfs.mtime = mtime;
        vfs.ctime = now as u64;
        self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?; // -EIO
        Ok(())
    }

    fn inc_nlink(&self) -> Result<(), i32> {
        let mut raw = self.raw.lock();
        raw.i_links_count = raw.i_links_count.saturating_add(1);
        self.vfs_inode.write().nlink = raw.i_links_count as u32;
        self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?;
        Ok(())
    }

    fn dec_nlink(&self) -> Result<(), i32> {
        let mut raw = self.raw.lock();
        if raw.i_links_count > 0 {
            raw.i_links_count -= 1;
        }
        self.vfs_inode.write().nlink = raw.i_links_count as u32;
        self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?;
        Ok(())
    }

    fn create(&self, name: &str, file_type: FileType) -> Option<Arc<dyn InodeOps>> {
        self.create_dir_entry(name, file_type)
    }

    fn unlink(&self, name: &str) -> Result<(), i32> {
        self.unlink_dir_entry(name)
    }

    fn unlink_entry(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        if !self.inode().is_dir() {
            return None;
        }
        let node = self.lookup(name)?;
        if node.inode().is_dir() {
            return None;
        }
        self.remove_directory_entry(name).ok()?;
        Some(node)
    }

    fn link_entry(&self, name: &str, node: Arc<dyn InodeOps>) -> Result<(), i32> {
        if !self.inode().is_dir() {
            return Err(-20); // ENOTDIR
        }
        if self.lookup(name).is_some() {
            return Err(-17); // EEXIST
        }
        if node.inode().dev != self.inode().dev {
            return Err(-18); // EXDEV
        }
        let child_ino = node.inode().ino as u32;
        self.add_directory_entry(child_ino, name, node.inode().file_type)
            .map_err(|_| -5)?; // EIO
        let now = crate::fs::vfs::current_time_sec();
        let mut raw = self.raw.lock();
        raw.i_mtime = now;
        raw.i_ctime = now;
        let _ = self.fs.write_inode(self.ino, &raw);
        Ok(())
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

impl Drop for ExtInode {
    fn drop(&mut self) {
        let (links_count, is_dir, raw_copy) = {
            let raw = self.raw.lock();
            let is_dir = (raw.i_mode & 0xF000) == 0x4000;
            (raw.i_links_count, is_dir, *raw)
        };

        if links_count == 0 {
            // All directory references are gone and all in-memory references have dropped.
            // Clean up the page cache, free data blocks, and deallocate the inode.
            crate::memory::page_cache::page_cache_invalidate_inode(EXT_DEV_ID, self.ino as u64);
            let _ = self
                .fs
                .deallocate_inode_and_blocks(self.ino, &raw_copy, is_dir);
        } else {
            let mut cache = self.fs.inode_cache.lock();
            if let Some(weak) = cache.get(&self.ino) {
                if weak.upgrade().is_none() {
                    cache.remove(&self.ino);
                }
            }
        }
    }
}

impl FileSystem for ExtFileSystem {
    fn root(&self) -> Option<Arc<dyn InodeOps>> {
        self.root_node.lock().clone()
    }

    fn name(&self) -> &str {
        "ext"
    }

    fn sync(&self) {
        let self_arc = match self.self_weak.lock().as_ref().and_then(|w| w.upgrade()) {
            Some(arc) => arc,
            None => return,
        };

        let dirty_inodes = crate::memory::page_cache::dirty_inodes_for_dev(EXT_DEV_ID);

        for ino in dirty_inodes {
            if let Ok(inode) = self_arc.get_inode(ino as u32) {
                let _ = crate::memory::page_cache::flush_all_for_inode(&inode);
            }
        }

        // Flush metadata: updated superblock and all group descriptors
        let sb = *self.superblock.lock();
        let _ = self.write_superblock(&sb);
        let _ = self.write_group_descriptors();

        // Issue cache flush barrier to the underlying block device
        let _ = self.device.flush();
    }

    fn statfs(&self) -> FsStats {
        let sb = self.superblock.lock();
        FsStats {
            total_blocks: sb.total_blocks(),
            free_blocks: sb.free_blocks(),
            total_inodes: sb.s_inodes_count as u64,
            free_inodes: sb.s_free_inodes_count as u64,
            block_size: self.block_size as u64,
            max_name_len: 255,
        }
    }
}
