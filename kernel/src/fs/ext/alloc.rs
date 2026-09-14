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

//! Block and inode allocation/deallocation routines.

use super::{read_blocks, write_blocks};
use super::{ExtFileSystem, ExtRawInode};

/// Helper to count free bits (zeros) in a bitmap buffer.
pub fn count_free_bits(bitmap: &[u8], total_count: u32) -> u32 {
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

impl ExtFileSystem {
    /// Allocate up to `count` contiguous blocks from the filesystem block bitmap.
    /// Returns `(start_block_num, allocated_count)`.
    pub fn allocate_blocks_contiguous(
        &self,
        preferred_block: u32,
        count: u32,
    ) -> Result<(u32, u32), &'static str> {
        let mut sb = self.superblock.lock();
        let mut gds = self.group_descriptors.lock();

        if sb.s_free_blocks_count == 0 {
            return Err("No free blocks");
        }

        let blocks_per_group = sb.s_blocks_per_group;
        let num_groups = gds.len();
        let requested = count.min(128).min(sb.s_free_blocks_count as u32);
        if requested == 0 {
            return Err("No free blocks");
        }

        let start_group = if preferred_block > sb.s_first_data_block {
            (((preferred_block - sb.s_first_data_block) / blocks_per_group) as usize) % num_groups
        } else {
            0
        };

        for g_offset in 0..num_groups {
            let g = (start_group + g_offset) % num_groups;
            if gds[g].bg_free_blocks_count == 0 {
                continue;
            }
            let group_blocks = if g == num_groups - 1 {
                sb.s_blocks_count - sb.s_first_data_block - (g as u32) * blocks_per_group
            } else {
                blocks_per_group
            };

            let block_bitmap_num = gds[g].bg_block_bitmap as u64;
            let mut bitmap = alloc::vec![0u8; self.block_size as usize];
            if read_blocks(
                &*self.device,
                block_bitmap_num,
                &mut bitmap,
                self.block_size,
            )
            .is_err()
            {
                continue;
            }

            let mut best_start = None;
            let mut best_len = 0u32;
            let mut curr_start = None;
            let mut curr_len = 0u32;

            for i in 0..group_blocks {
                let byte = (i / 8) as usize;
                let bit = i % 8;
                if (bitmap[byte] & (1 << bit)) == 0 {
                    if curr_start.is_none() {
                        curr_start = Some(i);
                        curr_len = 0;
                    }
                    curr_len += 1;
                    if curr_len == requested {
                        best_start = curr_start;
                        best_len = curr_len;
                        break;
                    }
                } else {
                    if curr_len > best_len {
                        best_start = curr_start;
                        best_len = curr_len;
                    }
                    curr_start = None;
                    curr_len = 0;
                }
            }
            if curr_len > best_len {
                best_start = curr_start;
                best_len = curr_len;
            }

            if let Some(start_idx) = best_start {
                if best_len > 0 {
                    let alloc_len = best_len.min(requested);
                    for i in start_idx..(start_idx + alloc_len) {
                        let byte = (i / 8) as usize;
                        let bit = i % 8;
                        bitmap[byte] |= 1 << bit;
                    }
                    write_blocks(&*self.device, block_bitmap_num, &bitmap, self.block_size)?;

                    let start_block_num =
                        (g as u32) * blocks_per_group + sb.s_first_data_block + start_idx;

                    let free_b = gds[g].free_blocks_count(self.is_64bit);
                    gds[g].set_free_blocks_count(self.is_64bit, free_b.saturating_sub(alloc_len));
                    let free_total = sb.free_blocks();
                    sb.set_free_blocks(free_total.saturating_sub(alloc_len as u64));

                    self.write_superblock(&sb)?;
                    drop(gds);
                    self.write_group_descriptors()?;

                    return Ok((start_block_num, alloc_len));
                }
            }
            // Group bitmap was full despite descriptor; correct counter and check next group
            gds[g].bg_free_blocks_count = 0;
        }
        Err("No free blocks found in bitmap")
    }

    /// Allocate a single block from the filesystem block bitmap.
    pub fn allocate_block(&self) -> Result<u32, &'static str> {
        self.allocate_blocks_contiguous(0, 1)
            .map(|(block, _)| block)
    }

    /// Deallocate a block back to the block bitmap.
    pub fn deallocate_block(&self, block_num: u32) -> Result<(), &'static str> {
        if block_num == 0 {
            return Ok(());
        }
        let mut sb = self.superblock.lock();
        let mut gds = self.group_descriptors.lock();

        let blocks_per_group = sb.s_blocks_per_group;
        let g = ((block_num - sb.s_first_data_block) / blocks_per_group) as usize;
        let i = (block_num - sb.s_first_data_block) % blocks_per_group;

        if g >= gds.len() {
            return Err("Block number out of filesystem bounds");
        }
        let gd = &mut gds[g];
        let mut bitmap = alloc::vec![0u8; self.block_size as usize];
        read_blocks(
            &*self.device,
            gd.bg_block_bitmap as u64,
            &mut bitmap,
            self.block_size,
        )?;

        let byte = (i / 8) as usize;
        let bit = i % 8;
        if (bitmap[byte] & (1 << bit)) != 0 {
            bitmap[byte] &= !(1 << bit);
            write_blocks(
                &*self.device,
                gd.bg_block_bitmap as u64,
                &bitmap,
                self.block_size,
            )?;

            sb.s_free_blocks_count += 1;
            gd.bg_free_blocks_count += 1;

            self.write_superblock(&sb)?;
            drop(gds);
            self.write_group_descriptors()?;
        }
        Ok(())
    }

    /// Allocate an inode from the filesystem inode bitmap.
    pub fn allocate_inode(&self, is_dir: bool) -> Result<u32, &'static str> {
        let mut sb = self.superblock.lock();
        let mut gds = self.group_descriptors.lock();

        if sb.s_free_inodes_count == 0 {
            return Err("No free inodes");
        }

        let inodes_per_group = self.inodes_per_group;
        let num_groups = gds.len();

        for g in 0..num_groups {
            if gds[g].bg_free_inodes_count == 0 {
                continue;
            }
            let group_inodes = if g == num_groups - 1 {
                sb.s_inodes_count - (g as u32) * inodes_per_group
            } else {
                inodes_per_group
            };

            let inode_bitmap_num = gds[g].bg_inode_bitmap as u64;
            let mut bitmap = alloc::vec![0u8; self.block_size as usize];
            if read_blocks(
                &*self.device,
                inode_bitmap_num,
                &mut bitmap,
                self.block_size,
            )
            .is_err()
            {
                continue;
            }

            for i in 0..group_inodes {
                let byte = (i / 8) as usize;
                let bit = i % 8;
                if (bitmap[byte] & (1 << bit)) == 0 {
                    bitmap[byte] |= 1 << bit;
                    write_blocks(&*self.device, inode_bitmap_num, &bitmap, self.block_size)?;

                    sb.s_free_inodes_count = sb.s_free_inodes_count.saturating_sub(1);
                    gds[g].bg_free_inodes_count = gds[g].bg_free_inodes_count.saturating_sub(1);
                    if is_dir {
                        gds[g].bg_used_dirs_count += 1;
                    }

                    self.write_superblock(&sb)?;
                    drop(gds);
                    self.write_group_descriptors()?;

                    let ino = (g as u32) * inodes_per_group + i + 1;
                    return Ok(ino);
                }
            }
            // Group bitmap had no free inodes despite descriptor; correct counter and check next group
            gds[g].bg_free_inodes_count = 0;
        }
        Err("No free inodes found in bitmap")
    }

    /// Deallocate an inode back to the inode bitmap.
    pub fn deallocate_inode(&self, ino: u32, is_dir: bool) -> Result<(), &'static str> {
        if ino == 0 {
            return Ok(());
        }
        self.inode_cache.lock().remove(&ino);
        let mut sb = self.superblock.lock();
        let mut gds = self.group_descriptors.lock();

        let inodes_per_group = self.inodes_per_group;
        let g = ((ino - 1) / inodes_per_group) as usize;
        let i = (ino - 1) % inodes_per_group;

        if g >= gds.len() {
            return Err("Inode number out of filesystem bounds");
        }
        let gd = &mut gds[g];
        let mut bitmap = alloc::vec![0u8; self.block_size as usize];
        read_blocks(
            &*self.device,
            gd.bg_inode_bitmap as u64,
            &mut bitmap,
            self.block_size,
        )?;

        let byte = (i / 8) as usize;
        let bit = i % 8;
        if (bitmap[byte] & (1 << bit)) != 0 {
            bitmap[byte] &= !(1 << bit);
            write_blocks(
                &*self.device,
                gd.bg_inode_bitmap as u64,
                &bitmap,
                self.block_size,
            )?;

            sb.s_free_inodes_count += 1;
            gd.bg_free_inodes_count += 1;
            if is_dir && gd.bg_used_dirs_count > 0 {
                gd.bg_used_dirs_count -= 1;
            }

            self.write_superblock(&sb)?;
            drop(gds);
            self.write_group_descriptors()?;
        }
        Ok(())
    }

    /// Write the modified raw inode back to the disk.
    pub fn write_inode(&self, ino: u32, raw_inode: &ExtRawInode) -> Result<(), &'static str> {
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

        let mut block_buf = alloc::vec![0u8; self.block_size as usize];
        read_blocks(
            &*self.device,
            logical_block,
            &mut block_buf,
            self.block_size,
        )?;

        let dst_ptr = block_buf[offset_in_block..].as_mut_ptr();
        let src_ptr = raw_inode as *const ExtRawInode as *const u8;
        let copy_size = core::cmp::min(
            self.inode_size as usize,
            core::mem::size_of::<ExtRawInode>(),
        );
        // SAFETY: Both pointers are valid. src_ptr points to an ExtRawInode of size copy_size,
        // and dst_ptr points to block_buf at offset_in_block with at least copy_size bytes available.
        unsafe {
            core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, copy_size);
        }

        write_blocks(&*self.device, logical_block, &block_buf, self.block_size)?;
        Ok(())
    }

    /// Deallocate all data blocks (direct, indirect, double-indirect) and the inode itself.
    pub fn deallocate_inode_and_blocks(
        &self,
        ino: u32,
        raw_inode: &ExtRawInode,
        is_dir: bool,
    ) -> Result<(), &'static str> {
        let is_symlink = (raw_inode.i_mode & 0xF000) == 0xA000;
        let is_fast_symlink = is_symlink && (raw_inode.i_size < 60 || raw_inode.i_blocks == 0);

        if !is_fast_symlink {
            // Copy array to avoid creating unaligned reference to packed struct field
            let i_block = raw_inode.i_block;
            for block in &i_block[0..12] {
                if *block != 0 {
                    let _ = self.deallocate_block(*block);
                }
            }

            // Free indirect block if present
            let sib = i_block[12];
            if sib != 0 {
                let mut ind_buf = alloc::vec![0u8; self.block_size as usize];
                if read_blocks(&*self.device, sib as u64, &mut ind_buf, self.block_size).is_ok() {
                    let refs_per_block = self.block_size / 4;
                    for j in 0..refs_per_block {
                        let ptr_offset = (j * 4) as usize;
                        let phys_block = u32::from_le_bytes([
                            ind_buf[ptr_offset],
                            ind_buf[ptr_offset + 1],
                            ind_buf[ptr_offset + 2],
                            ind_buf[ptr_offset + 3],
                        ]);
                        if phys_block != 0 {
                            let _ = self.deallocate_block(phys_block);
                        }
                    }
                }
                let _ = self.deallocate_block(sib);
            }

            // Free double indirect block if present
            let dib = i_block[13];
            if dib != 0 {
                let mut dib_buf = alloc::vec![0u8; self.block_size as usize];
                if read_blocks(&*self.device, dib as u64, &mut dib_buf, self.block_size).is_ok() {
                    let refs_per_block = self.block_size / 4;
                    for i in 0..refs_per_block {
                        let sib_offset = (i * 4) as usize;
                        let sib = u32::from_le_bytes([
                            dib_buf[sib_offset],
                            dib_buf[sib_offset + 1],
                            dib_buf[sib_offset + 2],
                            dib_buf[sib_offset + 3],
                        ]);
                        if sib != 0 {
                            let mut sib_buf = alloc::vec![0u8; self.block_size as usize];
                            if read_blocks(&*self.device, sib as u64, &mut sib_buf, self.block_size)
                                .is_ok()
                            {
                                for j in 0..refs_per_block {
                                    let ptr_offset = (j * 4) as usize;
                                    let phys_block = u32::from_le_bytes([
                                        sib_buf[ptr_offset],
                                        sib_buf[ptr_offset + 1],
                                        sib_buf[ptr_offset + 2],
                                        sib_buf[ptr_offset + 3],
                                    ]);
                                    if phys_block != 0 {
                                        let _ = self.deallocate_block(phys_block);
                                    }
                                }
                            }
                            let _ = self.deallocate_block(sib);
                        }
                    }
                }
                let _ = self.deallocate_block(dib);
            }
        }

        // Release the inode
        self.deallocate_inode(ino, is_dir)?;
        Ok(())
    }

    /// Decrement the links count of an inode. Cleans up blocks and frees the inode if it reaches 0.
    pub fn decrement_links_count(&self, ino: u32, is_dir: bool) -> Result<(), &'static str> {
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

        let mut block_buf = alloc::vec![0u8; self.block_size as usize];
        read_blocks(
            &*self.device,
            logical_block,
            &mut block_buf,
            self.block_size,
        )?;

        let mut raw_inode = unsafe {
            core::ptr::read_unaligned(block_buf[offset_in_block..].as_ptr() as *const ExtRawInode)
        };

        if raw_inode.i_links_count > 0 {
            raw_inode.i_links_count -= 1;
        }

        if raw_inode.i_links_count == 0 {
            self.deallocate_inode_and_blocks(ino, &raw_inode, is_dir)?;
        } else {
            let dst_ptr = block_buf[offset_in_block..].as_mut_ptr();
            let src_ptr = &raw_inode as *const ExtRawInode as *const u8;
            let copy_size = core::cmp::min(
                self.inode_size as usize,
                core::mem::size_of::<ExtRawInode>(),
            );
            // SAFETY: Both pointers are valid. src_ptr points to an ExtRawInode of size copy_size,
            // and dst_ptr points to block_buf at offset_in_block with at least copy_size bytes available.
            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, copy_size);
            }
            write_blocks(&*self.device, logical_block, &block_buf, self.block_size)?;
        }
        Ok(())
    }
}
