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
    /// Allocate a block from the filesystem block bitmap.
    pub fn allocate_block(&self) -> Result<u32, &'static str> {
        let mut sb = self.superblock.lock();
        let mut gds = self.group_descriptors.lock();

        if sb.s_free_blocks_count == 0 {
            return Err("No free blocks");
        }

        let blocks_per_group = sb.s_blocks_per_group;
        let mut group_idx = None;
        for (idx, gd) in gds.iter().enumerate() {
            if gd.bg_free_blocks_count > 0 {
                group_idx = Some(idx);
                break;
            }
        }
        let g = group_idx.ok_or("No free blocks found in group descriptors")?;
        let group_blocks = if g == gds.len() - 1 {
            sb.s_blocks_count - sb.s_first_data_block - (g as u32) * blocks_per_group
        } else {
            blocks_per_group
        };

        let gd = &mut gds[g];
        let mut bitmap = alloc::vec![0u8; self.block_size as usize];
        read_blocks(
            &*self.device,
            gd.bg_block_bitmap as u64,
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
                    gd.bg_block_bitmap as u64,
                    &bitmap,
                    self.block_size,
                )?;

                let block_num = (g as u32) * blocks_per_group + sb.s_first_data_block + i;

                sb.s_free_blocks_count -= 1;
                gd.bg_free_blocks_count -= 1;

                self.write_superblock(&sb)?;
                drop(gds);
                self.write_group_descriptors()?;

                // Zero out the newly allocated block
                let zero_buf = alloc::vec![0u8; self.block_size as usize];
                write_blocks(&*self.device, block_num as u64, &zero_buf, self.block_size)?;

                return Ok(block_num);
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
        let mut group_idx = None;
        for (idx, gd) in gds.iter().enumerate() {
            if gd.bg_free_inodes_count > 0 {
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
            gd.bg_inode_bitmap as u64,
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
                    gd.bg_inode_bitmap as u64,
                    &bitmap,
                    self.block_size,
                )?;

                sb.s_free_inodes_count -= 1;
                gd.bg_free_inodes_count -= 1;
                if is_dir {
                    gd.bg_used_dirs_count += 1;
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
        unsafe {
            core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, self.inode_size as usize);
        }

        write_blocks(&*self.device, logical_block, &block_buf, self.block_size)?;
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
            // Copy array to avoid creating unaligned reference to packed struct field
            let i_block = raw_inode.i_block;
            for block in &i_block[0..12] {
                if *block != 0 {
                    self.deallocate_block(*block)?;
                }
            }

            // Free indirect block if present
            let sib = i_block[12];
            if sib != 0 {
                let mut ind_buf = alloc::vec![0u8; self.block_size as usize];
                read_blocks(&*self.device, sib as u64, &mut ind_buf, self.block_size)?;
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
                        self.deallocate_block(phys_block)?;
                    }
                }
                self.deallocate_block(sib)?;
            }

            // Free double indirect block if present
            let dib = i_block[13];
            if dib != 0 {
                let mut dib_buf = alloc::vec![0u8; self.block_size as usize];
                read_blocks(&*self.device, dib as u64, &mut dib_buf, self.block_size)?;
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
                        read_blocks(&*self.device, sib as u64, &mut sib_buf, self.block_size)?;
                        for j in 0..refs_per_block {
                            let ptr_offset = (j * 4) as usize;
                            let phys_block = u32::from_le_bytes([
                                sib_buf[ptr_offset],
                                sib_buf[ptr_offset + 1],
                                sib_buf[ptr_offset + 2],
                                sib_buf[ptr_offset + 3],
                            ]);
                            if phys_block != 0 {
                                self.deallocate_block(phys_block)?;
                            }
                        }
                        self.deallocate_block(sib)?;
                    }
                }
                self.deallocate_block(dib)?;
            }

            // Release the inode
            self.deallocate_inode(ino, is_dir)?;
        } else {
            let dst_ptr = block_buf[offset_in_block..].as_mut_ptr();
            let src_ptr = &raw_inode as *const ExtRawInode as *const u8;
            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, self.inode_size as usize);
            }
            write_blocks(&*self.device, logical_block, &block_buf, self.block_size)?;
        }
        Ok(())
    }
}
