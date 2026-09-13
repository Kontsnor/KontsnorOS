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

/// Find the index of the first 0 bit in bitmap starting from `start_bit`, up to `total_bits`.
/// Uses 64-bit word scanning (`u64::from_le_bytes`) for O(1) skipping of full regions.
fn find_first_zero_bit(bitmap: &[u8], start_bit: u32, total_bits: u32) -> Option<u32> {
    if start_bit >= total_bits {
        return None;
    }

    let mut current_bit = start_bit;

    // 1. Bit-by-bit check until word-aligned (64-bit / 8-byte aligned) or target bit reached
    while current_bit < total_bits && (current_bit % 64 != 0) {
        let byte_idx = (current_bit / 8) as usize;
        let bit_idx = current_bit % 8;
        if byte_idx < bitmap.len() && (bitmap[byte_idx] & (1 << bit_idx)) == 0 {
            return Some(current_bit);
        }
        current_bit += 1;
    }

    // 2. Scan word by word (64 bits at a time)
    while current_bit + 64 <= total_bits {
        let byte_idx = (current_bit / 8) as usize;
        if byte_idx + 8 <= bitmap.len() {
            let word = u64::from_le_bytes(bitmap[byte_idx..byte_idx + 8].try_into().unwrap());
            if word != !0u64 {
                let zero_bit = (!word).trailing_zeros();
                return Some(current_bit + zero_bit);
            }
        } else {
            break;
        }
        current_bit += 64;
    }

    // 3. Scan remaining bits byte/bit at a time
    while current_bit < total_bits {
        let byte_idx = (current_bit / 8) as usize;
        let bit_idx = current_bit % 8;
        if byte_idx < bitmap.len() && (bitmap[byte_idx] & (1 << bit_idx)) == 0 {
            return Some(current_bit);
        }
        current_bit += 1;
    }

    None
}

impl ExtFileSystem {
    /// Allocate a block from the filesystem block bitmap.
    pub fn allocate_block(&self) -> Result<u32, &'static str> {
        let mut sb = self.superblock.lock();
        let mut gds = self.group_descriptors.lock();

        if sb.s_free_blocks_count == 0 {
            return Err("No free blocks");
        }

        let blocks_per_group = sb.s_blocks_per_group;
        let num_groups = gds.len();
        let first_data_block = sb.s_first_data_block;
        let total_blocks = sb.s_blocks_count;

        let last_hint = *self.last_alloc_block.lock();
        let start_group = if last_hint >= first_data_block {
            let relative = last_hint - first_data_block;
            (((relative / blocks_per_group) as usize)).min(num_groups - 1)
        } else {
            0
        };

        for pass in 0..2 {
            let group_range = if pass == 0 {
                start_group..num_groups
            } else {
                0..start_group
            };

            for g in group_range {
                if gds[g].free_blocks_count(self.is_64bit) == 0 {
                    continue;
                }

                let group_start_block = (g as u32) * blocks_per_group + first_data_block;
                let group_blocks = if g == num_groups - 1 {
                    total_blocks - group_start_block
                } else {
                    blocks_per_group
                };

                let start_bit = if pass == 0 && g == start_group && last_hint >= group_start_block {
                    (last_hint - group_start_block).min(group_blocks)
                } else {
                    0
                };

                let gd = &mut gds[g];
                let block_bitmap_loc = gd.block_bitmap(self.is_64bit);
                let mut bitmap = alloc::vec![0u8; self.block_size as usize];
                read_blocks(&*self.device, block_bitmap_loc, &mut bitmap, self.block_size)?;

                let found_bit = find_first_zero_bit(&bitmap, start_bit, group_blocks)
                    .or_else(|| {
                        if start_bit > 0 {
                            find_first_zero_bit(&bitmap, 0, start_bit)
                        } else {
                            None
                        }
                    });

                if let Some(i) = found_bit {
                    let byte = (i / 8) as usize;
                    let bit = i % 8;
                    bitmap[byte] |= 1 << bit;

                    write_blocks(&*self.device, block_bitmap_loc, &bitmap, self.block_size)?;

                    let block_num = group_start_block + i;
                    *self.last_alloc_block.lock() = block_num + 1;

                    let new_free = sb.free_blocks() - 1;
                    sb.set_free_blocks(new_free);
                    gd.set_free_blocks_count(self.is_64bit, gd.free_blocks_count(self.is_64bit) - 1);

                    self.write_superblock(&sb)?;
                    drop(gds);
                    self.write_group_descriptors()?;

                    // Zero out the newly allocated block
                    let zero_buf = alloc::vec![0u8; self.block_size as usize];
                    write_blocks(&*self.device, block_num as u64, &zero_buf, self.block_size)?;

                    return Ok(block_num);
                }
            }
        }

        Err("No free blocks found in bitmap")
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
        let total_inodes = sb.s_inodes_count;

        let last_hint = *self.last_alloc_inode.lock();
        let start_group = if last_hint > 0 {
            (((last_hint - 1) / inodes_per_group) as usize).min(num_groups - 1)
        } else {
            0
        };

        for pass in 0..2 {
            let group_range = if pass == 0 {
                start_group..num_groups
            } else {
                0..start_group
            };

            for g in group_range {
                if gds[g].free_inodes_count(self.is_64bit) == 0 {
                    continue;
                }

                let group_start_ino = (g as u32) * inodes_per_group + 1;
                let group_inodes = if g == num_groups - 1 {
                    total_inodes - (group_start_ino - 1)
                } else {
                    inodes_per_group
                };

                let start_bit = if pass == 0 && g == start_group && last_hint >= group_start_ino {
                    (last_hint - group_start_ino).min(group_inodes)
                } else {
                    0
                };

                let gd = &mut gds[g];
                let inode_bitmap_loc = gd.inode_bitmap(self.is_64bit);
                let mut bitmap = alloc::vec![0u8; self.block_size as usize];
                read_blocks(&*self.device, inode_bitmap_loc, &mut bitmap, self.block_size)?;

                let found_bit = find_first_zero_bit(&bitmap, start_bit, group_inodes)
                    .or_else(|| {
                        if start_bit > 0 {
                            find_first_zero_bit(&bitmap, 0, start_bit)
                        } else {
                            None
                        }
                    });

                if let Some(i) = found_bit {
                    let byte = (i / 8) as usize;
                    let bit = i % 8;
                    bitmap[byte] |= 1 << bit;

                    write_blocks(&*self.device, inode_bitmap_loc, &bitmap, self.block_size)?;

                    sb.s_free_inodes_count -= 1;
                    gd.set_free_inodes_count(self.is_64bit, gd.free_inodes_count(self.is_64bit) - 1);
                    if is_dir {
                        gd.set_used_dirs_count(self.is_64bit, gd.used_dirs_count(self.is_64bit) + 1);
                    }

                    self.write_superblock(&sb)?;
                    drop(gds);
                    self.write_group_descriptors()?;

                    let ino = group_start_ino + i;
                    *self.last_alloc_inode.lock() = ino + 1;
                    return Ok(ino);
                }
            }
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
