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
    /// Allocate a physical block from the filesystem block bitmap.
    pub fn allocate_block(&self) -> Result<u64, &'static str> {
        let mut sb = self.superblock.lock();
        let mut gds = self.group_descriptors.lock();
        let is_64bit = self.is_64bit;

        if sb.free_blocks() == 0 {
            return Err("No free blocks");
        }

        let blocks_per_group = sb.s_blocks_per_group;
        let total_blocks = sb.total_blocks();
        let mut group_idx = None;
        for (idx, gd) in gds.iter().enumerate() {
            if gd.free_blocks_count(is_64bit) > 0 {
                group_idx = Some(idx);
                break;
            }
        }
        let g = group_idx.ok_or("No free blocks found in group descriptors")?;
        let group_blocks = if g == gds.len() - 1 {
            (total_blocks - sb.s_first_data_block as u64 - (g as u64) * blocks_per_group as u64) as u32
        } else {
            blocks_per_group
        };

        let gd = &mut gds[g];
        let mut bitmap = alloc::vec![0u8; self.block_size as usize];
        read_blocks(
            &*self.device,
            gd.block_bitmap(is_64bit),
            &mut bitmap,
            self.block_size,
        )?;

        for i in 0..group_blocks {
            let byte = (i / 8) as usize;
            let bit = i % 8;
            if (bitmap[byte] & (1 << bit)) == 0 {
                bitmap[byte] |= 1 << bit;
                write_blocks(
                    &*self.device,
                    gd.block_bitmap(is_64bit),
                    &bitmap,
                    self.block_size,
                )?;

                let block_num = (g as u64) * blocks_per_group as u64 + sb.s_first_data_block as u64 + i as u64;

                let current_free = sb.free_blocks();
                sb.set_free_blocks(current_free.saturating_sub(1));
                let current_gd_free = gd.free_blocks_count(is_64bit);
                gd.set_free_blocks_count(is_64bit, current_gd_free.saturating_sub(1));

                self.write_superblock(&sb)?;
                drop(gds);
                self.write_group_descriptors()?;

                // Zero out the newly allocated block
                let zero_buf = alloc::vec![0u8; self.block_size as usize];
                write_blocks(&*self.device, block_num, &zero_buf, self.block_size)?;

                return Ok(block_num);
            }
        }
        Err("No free blocks found in bitmap")
    }

    /// Deallocate a physical block back to the block bitmap.
    pub fn deallocate_block(&self, block_num: u64) -> Result<(), &'static str> {
        if block_num == 0 {
            return Ok(());
        }
        let mut sb = self.superblock.lock();
        let mut gds = self.group_descriptors.lock();
        let is_64bit = self.is_64bit;

        let blocks_per_group = sb.s_blocks_per_group as u64;
        let g = ((block_num - sb.s_first_data_block as u64) / blocks_per_group) as usize;
        let i = ((block_num - sb.s_first_data_block as u64) % blocks_per_group) as u32;

        if g >= gds.len() {
            return Err("Block number out of filesystem bounds");
        }
        let gd = &mut gds[g];
        let mut bitmap = alloc::vec![0u8; self.block_size as usize];
        read_blocks(
            &*self.device,
            gd.block_bitmap(is_64bit),
            &mut bitmap,
            self.block_size,
        )?;

        let byte = (i / 8) as usize;
        let bit = i % 8;
        if (bitmap[byte] & (1 << bit)) != 0 {
            bitmap[byte] &= !(1 << bit);
            write_blocks(
                &*self.device,
                gd.block_bitmap(is_64bit),
                &bitmap,
                self.block_size,
            )?;

            let current_free = sb.free_blocks();
            sb.set_free_blocks(current_free.saturating_add(1));
            let current_gd_free = gd.free_blocks_count(is_64bit);
            gd.set_free_blocks_count(is_64bit, current_gd_free.saturating_add(1));

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
        let is_64bit = self.is_64bit;

        if sb.s_free_inodes_count == 0 {
            return Err("No free inodes");
        }

        let inodes_per_group = self.inodes_per_group;
        let mut group_idx = None;
        for (idx, gd) in gds.iter().enumerate() {
            if gd.free_inodes_count(is_64bit) > 0 {
                group_idx = Some(idx);
                break;
            }
        }
        let g = group_idx.ok_or("No free inodes found in group descriptors")?;
        let group_inodes = if g == gds.len() - 1 {
            sb.s_inodes_count - (g as u32) * inodes_per_group
        } else {
            inodes_per_group
        };

        let gd = &mut gds[g];
        let mut bitmap = alloc::vec![0u8; self.block_size as usize];
        read_blocks(
            &*self.device,
            gd.inode_bitmap(is_64bit),
            &mut bitmap,
            self.block_size,
        )?;

        for i in 0..group_inodes {
            let byte = (i / 8) as usize;
            let bit = i % 8;
            if (bitmap[byte] & (1 << bit)) == 0 {
                bitmap[byte] |= 1 << bit;
                write_blocks(
                    &*self.device,
                    gd.inode_bitmap(is_64bit),
                    &bitmap,
                    self.block_size,
                )?;

                sb.s_free_inodes_count = sb.s_free_inodes_count.saturating_sub(1);
                let current_gd_free_i = gd.free_inodes_count(is_64bit);
                gd.set_free_inodes_count(is_64bit, current_gd_free_i.saturating_sub(1));
                if is_dir {
                    let current_used_dirs = gd.used_dirs_count(is_64bit);
                    gd.set_used_dirs_count(is_64bit, current_used_dirs.saturating_add(1));
                }

                self.write_superblock(&sb)?;
                drop(gds);
                self.write_group_descriptors()?;

                let ino = (g as u32) * inodes_per_group + i + 1;
                return Ok(ino);
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
        let is_64bit = self.is_64bit;

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
            gd.inode_bitmap(is_64bit),
            &mut bitmap,
            self.block_size,
        )?;

        let byte = (i / 8) as usize;
        let bit = i % 8;
        if (bitmap[byte] & (1 << bit)) != 0 {
            bitmap[byte] &= !(1 << bit);
            write_blocks(
                &*self.device,
                gd.inode_bitmap(is_64bit),
                &bitmap,
                self.block_size,
            )?;

            sb.s_free_inodes_count = sb.s_free_inodes_count.saturating_add(1);
            let current_gd_free_i = gd.free_inodes_count(is_64bit);
            gd.set_free_inodes_count(is_64bit, current_gd_free_i.saturating_add(1));
            if is_dir {
                let current_used_dirs = gd.used_dirs_count(is_64bit);
                gd.set_used_dirs_count(is_64bit, current_used_dirs.saturating_sub(1));
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

        let table_block = gd.inode_table(self.is_64bit);
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

    /// Deallocate all data blocks (direct, indirect, double-indirect, or extents) and the inode itself.
    pub fn deallocate_inode_and_blocks(
        &self,
        ino: u32,
        raw_inode: &ExtRawInode,
        is_dir: bool,
    ) -> Result<(), &'static str> {
        let is_symlink = (raw_inode.i_mode & 0xF000) == 0xA000;
        let is_fast_symlink = is_symlink && (raw_inode.file_size() < 60 || raw_inode.i_blocks == 0);

        if !is_fast_symlink {
            if (raw_inode.i_flags & super::types::EXT4_EXTENTS_FL) != 0 {
                // Free extent tree data blocks and index blocks
                let i_block = raw_inode.i_block;
                self.deallocate_extent_tree(&i_block)?;
            } else {
                // Copy array to avoid creating unaligned reference to packed struct field
                let i_block = raw_inode.i_block;
                for block in &i_block[0..12] {
                    if *block != 0 {
                        let _ = self.deallocate_block(*block as u64);
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
                                let _ = self.deallocate_block(phys_block as u64);
                            }
                        }
                    }
                    let _ = self.deallocate_block(sib as u64);
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
                                            let _ = self.deallocate_block(phys_block as u64);
                                        }
                                    }
                                }
                                let _ = self.deallocate_block(sib as u64);
                            }
                        }
                    }
                    let _ = self.deallocate_block(dib as u64);
                }
            }
        }

        // Release the inode
        self.deallocate_inode(ino, is_dir)?;
        Ok(())
    }

    /// Recursively free all blocks in an extent tree.
    pub fn deallocate_extent_tree(&self, i_block: &[u32; 15]) -> Result<(), &'static str> {
        let mut current_buf = [0u8; 4096];
        let block_size = self.block_size as usize;
        for i in 0..15 {
            current_buf[i * 4..i * 4 + 4].copy_from_slice(&i_block[i].to_le_bytes());
        }

        fn free_node(
            fs: &ExtFileSystem,
            buf: &[u8],
            is_root: bool,
            node_block: u64,
        ) -> Result<(), &'static str> {
            if buf.len() < 12 {
                return Ok(());
            }
            let header = unsafe {
                core::ptr::read_unaligned(buf.as_ptr() as *const super::types::Ext4ExtentHeader)
            };
            if header.eh_magic != 0xF30A {
                return Ok(());
            }

            let entries = header.eh_entries as usize;
            let depth = header.eh_depth;

            if depth == 0 {
                // Leaf node
                let entry_size = core::mem::size_of::<super::types::Ext4Extent>();
                for i in 0..entries {
                    let offset = 12 + i * entry_size;
                    if offset + entry_size <= buf.len() {
                        let ext = unsafe {
                            core::ptr::read_unaligned(
                                buf[offset..].as_ptr() as *const super::types::Ext4Extent,
                            )
                        };
                        let start = ext.start_block();
                        let len = ext.len() as u64;
                        for b in 0..len {
                            let _ = fs.deallocate_block(start + b);
                        }
                    }
                }
            } else {
                // Index node
                let entry_size = core::mem::size_of::<super::types::Ext4ExtentIdx>();
                for i in 0..entries {
                    let offset = 12 + i * entry_size;
                    if offset + entry_size <= buf.len() {
                        let idx = unsafe {
                            core::ptr::read_unaligned(
                                buf[offset..].as_ptr() as *const super::types::Ext4ExtentIdx,
                            )
                        };
                        let child_block = idx.leaf_block();
                        let mut child_buf = alloc::vec![0u8; fs.block_size as usize];
                        if read_blocks(&*fs.device, child_block, &mut child_buf, fs.block_size).is_ok() {
                            let _ = free_node(fs, &child_buf, false, child_block);
                        }
                    }
                }
            }

            if !is_root && node_block != 0 {
                let _ = fs.deallocate_block(node_block);
            }
            Ok(())
        }

        free_node(self, &current_buf[..60], true, 0)
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
        let table_block = gd.inode_table(self.is_64bit);
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

        let mut raw_inode = super::read_raw_inode(&block_buf, offset_in_block, self.inode_size as usize);

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
