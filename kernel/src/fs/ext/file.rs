//! Regular file read, write, and size manipulation (truncate) operations.

use super::{read_blocks, write_blocks};
use super::{Ext4Extent, Ext4ExtentHeader, Ext4ExtentIdx};
use super::{ExtInode, ExtRawInode};
use crate::fs::inode::{FileType, InodeOps};

impl ExtInode {
    /// Resolve an Ext4 extent-mapped block.
    pub fn resolve_extent_block(
        &self,
        i_block: &[u32; 15],
        file_block: u32,
    ) -> Result<u32, &'static str> {
        crate::kprintln!(
            "[resolve_extent_block] ino={}, file_block={}",
            self.ino,
            file_block
        );
        let mut current_buf = [0u8; 4096];
        let mut current_len = 60;
        for i in 0..15 {
            current_buf[i * 4..i * 4 + 4].copy_from_slice(&i_block[i].to_le_bytes());
        }

        loop {
            crate::kprintln!(
                "[resolve_extent_block] loop offset, buf len={}",
                current_len
            );
            if current_len < 12 {
                return Err("Extent buffer too small for header");
            }
            // SAFETY: Safe to read Ext4ExtentHeader from a valid aligned/unaligned buffer of sufficient size.
            let header = unsafe {
                core::ptr::read_unaligned(current_buf.as_ptr() as *const Ext4ExtentHeader)
            };
            let eh_magic = header.eh_magic;
            let eh_depth = header.eh_depth;
            let eh_entries = header.eh_entries;
            crate::kprintln!(
                "[resolve_extent_block] magic={:#x}, depth={}, entries={}",
                eh_magic,
                eh_depth,
                eh_entries
            );
            if eh_magic != 0xF30A {
                return Err("Invalid extent header magic");
            }

            let eh_entries = eh_entries as usize;

            if eh_depth == 0 {
                // Leaf node. Followed by leaf entries.
                let entry_size = core::mem::size_of::<Ext4Extent>(); // 12 bytes
                for i in 0..eh_entries {
                    let offset = 12 + i * entry_size;
                    if offset + entry_size > current_len {
                        return Err("Extent entry out of bounds");
                    }
                    // SAFETY: Safe to read Ext4Extent from a valid aligned/unaligned buffer of sufficient size.
                    let ext = unsafe {
                        core::ptr::read_unaligned(
                            current_buf[offset..].as_ptr() as *const Ext4Extent
                        )
                    };
                    if file_block >= ext.ee_block && file_block < ext.ee_block + ext.ee_len as u32 {
                        let phys_start =
                            ((ext.ee_start_hi as u64) << 32) | (ext.ee_start_lo as u64);
                        let phys_block = phys_start + (file_block - ext.ee_block) as u64;
                        return Ok(phys_block as u32);
                    }
                }
                return Ok(0); // Sparse block / hole
            } else {
                // Index node. Followed by index entries.
                let entry_size = core::mem::size_of::<Ext4ExtentIdx>(); // 12 bytes
                let mut best_idx: Option<Ext4ExtentIdx> = None;
                for i in 0..eh_entries {
                    let offset = 12 + i * entry_size;
                    if offset + entry_size > current_len {
                        return Err("Extent index entry out of bounds");
                    }
                    // SAFETY: Safe to read Ext4ExtentIdx from a valid aligned/unaligned buffer of sufficient size.
                    let idx = unsafe {
                        core::ptr::read_unaligned(
                            current_buf[offset..].as_ptr() as *const Ext4ExtentIdx
                        )
                    };
                    if idx.ei_block <= file_block {
                        match best_idx {
                            None => best_idx = Some(idx),
                            Some(ref best) => {
                                if idx.ei_block > best.ei_block {
                                    best_idx = Some(idx);
                                }
                            }
                        }
                    }
                }

                if let Some(best) = best_idx {
                    let child_block = ((best.ei_leaf_hi as u64) << 32) | (best.ei_leaf_lo as u64);
                    let best_ei_block = best.ei_block;
                    crate::kprintln!(
                        "[resolve_extent_block] depth={}, ei_block={}, child_block={}",
                        eh_depth,
                        best_ei_block,
                        child_block
                    );
                    let block_size = self.fs.block_size as usize;
                    assert!(block_size <= 4096);
                    read_blocks(
                        &*self.fs.device,
                        child_block,
                        &mut current_buf[..block_size],
                        self.fs.block_size,
                    )?;
                    current_len = block_size;
                } else {
                    return Ok(0); // Not found
                }
            }
        }
    }

    /// Resolve logical block number to physical disk block using a provided raw inode reference.
    pub fn resolve_block_with_raw(
        &self,
        raw: &ExtRawInode,
        file_block: u32,
    ) -> Result<u32, &'static str> {
        if (raw.i_flags & 0x80000) != 0 {
            let i_block = raw.i_block;
            return self.resolve_extent_block(&i_block, file_block);
        }
        let i_block = raw.i_block;

        if file_block < 12 {
            return Ok(i_block[file_block as usize]);
        }

        let indirect_index = file_block - 12;
        let refs_per_block = self.fs.block_size / 4;

        if indirect_index < refs_per_block {
            let sib = i_block[12];
            if sib == 0 {
                return Ok(0);
            }

            let mut ind_buf = [0u8; 4096];
            let block_size = self.fs.block_size as usize;
            assert!(block_size <= 4096);
            read_blocks(
                &*self.fs.device,
                sib as u64,
                &mut ind_buf[..block_size],
                self.fs.block_size,
            )?;

            let ptr_offset = (indirect_index * 4) as usize;
            let phys_block = u32::from_le_bytes([
                ind_buf[ptr_offset],
                ind_buf[ptr_offset + 1],
                ind_buf[ptr_offset + 2],
                ind_buf[ptr_offset + 3],
            ]);
            return Ok(phys_block);
        }

        let double_index = indirect_index - refs_per_block;
        let max_double_blocks = refs_per_block * refs_per_block;
        if double_index < max_double_blocks {
            let dib = i_block[13];
            if dib == 0 {
                return Ok(0);
            }

            let mut dib_buf = [0u8; 4096];
            let block_size = self.fs.block_size as usize;
            assert!(block_size <= 4096);
            read_blocks(
                &*self.fs.device,
                dib as u64,
                &mut dib_buf[..block_size],
                self.fs.block_size,
            )?;

            let sib_index = double_index / refs_per_block;
            let sib_ptr_offset = (sib_index * 4) as usize;
            let sib = u32::from_le_bytes([
                dib_buf[sib_ptr_offset],
                dib_buf[sib_ptr_offset + 1],
                dib_buf[sib_ptr_offset + 2],
                dib_buf[sib_ptr_offset + 3],
            ]);
            if sib == 0 {
                return Ok(0);
            }

            let mut sib_buf = [0u8; 4096];
            read_blocks(
                &*self.fs.device,
                sib as u64,
                &mut sib_buf[..block_size],
                self.fs.block_size,
            )?;

            let data_index = double_index % refs_per_block;
            let data_ptr_offset = (data_index * 4) as usize;
            let phys_block = u32::from_le_bytes([
                sib_buf[data_ptr_offset],
                sib_buf[data_ptr_offset + 1],
                sib_buf[data_ptr_offset + 2],
                sib_buf[data_ptr_offset + 3],
            ]);
            return Ok(phys_block);
        }

        Err("Triple indirect blocks are unsupported in this phase.")
    }

    /// Resolve logical block number to physical disk block.
    pub fn resolve_block(&self, file_block: u32) -> Result<u32, &'static str> {
        let raw = self.raw.lock();
        self.resolve_block_with_raw(&raw, file_block)
    }

    /// Retrieve or dynamically allocate a physical disk block for a file block index.
    pub fn get_or_alloc_block(
        &self,
        raw: &mut ExtRawInode,
        file_block: u32,
    ) -> Result<u32, &'static str> {
        if (raw.i_flags & 0x80000) != 0 {
            let i_block = raw.i_block;
            if let Ok(phys_block) = self.resolve_extent_block(&i_block, file_block) {
                if phys_block != 0 {
                    return Ok(phys_block);
                }
            }
            return Err(
                "Dynamic allocation of physical blocks for Ext4 extent files is unsupported",
            );
        }

        if file_block < 12 {
            let phys_block = raw.i_block[file_block as usize];

            if phys_block != 0 {
                return Ok(phys_block);
            }
            let new_block = self.fs.allocate_block()?;
            raw.i_block[file_block as usize] = new_block;
            raw.i_blocks += self.fs.block_size / 512;
            self.fs.write_inode(self.ino, raw)?;
            return Ok(new_block);
        }

        let indirect_index = file_block - 12;
        let refs_per_block = self.fs.block_size / 4;
        if indirect_index < refs_per_block {
            let mut sib = raw.i_block[12];
            if sib == 0 {
                sib = self.fs.allocate_block()?;
                raw.i_block[12] = sib;
                raw.i_blocks += self.fs.block_size / 512;
                self.fs.write_inode(self.ino, raw)?;
            }

            let mut ind_buf = [0u8; 4096];
            let block_size = self.fs.block_size as usize;
            assert!(block_size <= 4096);
            read_blocks(
                &*self.fs.device,
                sib as u64,
                &mut ind_buf[..block_size],
                self.fs.block_size,
            )?;

            let ptr_offset = (indirect_index * 4) as usize;
            let mut phys_block = u32::from_le_bytes([
                ind_buf[ptr_offset],
                ind_buf[ptr_offset + 1],
                ind_buf[ptr_offset + 2],
                ind_buf[ptr_offset + 3],
            ]);

            if phys_block == 0 {
                phys_block = self.fs.allocate_block()?;
                let bytes = phys_block.to_le_bytes();
                ind_buf[ptr_offset..ptr_offset + 4].copy_from_slice(&bytes);
                write_blocks(
                    &*self.fs.device,
                    sib as u64,
                    &ind_buf[..block_size],
                    self.fs.block_size,
                )?;

                raw.i_blocks += self.fs.block_size / 512;
                self.fs.write_inode(self.ino, raw)?;
            }

            return Ok(phys_block);
        }

        let double_index = indirect_index - refs_per_block;
        let max_double_blocks = refs_per_block * refs_per_block;
        if double_index < max_double_blocks {
            let mut dib = raw.i_block[13];
            if dib == 0 {
                dib = self.fs.allocate_block()?;
                raw.i_block[13] = dib;
                raw.i_blocks += self.fs.block_size / 512;
                self.fs.write_inode(self.ino, raw)?;
            }

            let mut dib_buf = [0u8; 4096];
            let block_size = self.fs.block_size as usize;
            assert!(block_size <= 4096);
            read_blocks(
                &*self.fs.device,
                dib as u64,
                &mut dib_buf[..block_size],
                self.fs.block_size,
            )?;

            let sib_index = double_index / refs_per_block;
            let sib_ptr_offset = (sib_index * 4) as usize;
            let mut sib = u32::from_le_bytes([
                dib_buf[sib_ptr_offset],
                dib_buf[sib_ptr_offset + 1],
                dib_buf[sib_ptr_offset + 2],
                dib_buf[sib_ptr_offset + 3],
            ]);

            if sib == 0 {
                sib = self.fs.allocate_block()?;
                let bytes = sib.to_le_bytes();
                dib_buf[sib_ptr_offset..sib_ptr_offset + 4].copy_from_slice(&bytes);
                write_blocks(
                    &*self.fs.device,
                    dib as u64,
                    &dib_buf[..block_size],
                    self.fs.block_size,
                )?;

                raw.i_blocks += self.fs.block_size / 512;
                self.fs.write_inode(self.ino, raw)?;
            }

            let mut sib_buf = [0u8; 4096];
            read_blocks(
                &*self.fs.device,
                sib as u64,
                &mut sib_buf[..block_size],
                self.fs.block_size,
            )?;

            let data_index = double_index % refs_per_block;
            let data_ptr_offset = (data_index * 4) as usize;
            let mut phys_block = u32::from_le_bytes([
                sib_buf[data_ptr_offset],
                sib_buf[data_ptr_offset + 1],
                sib_buf[data_ptr_offset + 2],
                sib_buf[data_ptr_offset + 3],
            ]);

            if phys_block == 0 {
                phys_block = self.fs.allocate_block()?;
                let bytes = phys_block.to_le_bytes();
                sib_buf[data_ptr_offset..data_ptr_offset + 4].copy_from_slice(&bytes);
                write_blocks(
                    &*self.fs.device,
                    sib as u64,
                    &sib_buf[..block_size],
                    self.fs.block_size,
                )?;

                raw.i_blocks += self.fs.block_size / 512;
                self.fs.write_inode(self.ino, raw)?;
            }

            return Ok(phys_block);
        }

        Err("Triple indirect blocks are unsupported in this phase.")
    }

    /// Read data from regular file or symlink.
    pub fn read_file(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let (file_size, is_symlink) = {
            let vfs = self.vfs_inode.read();
            (vfs.size, vfs.file_type == FileType::Symlink)
        };
        if offset >= file_size {
            return Ok(0);
        }

        if is_symlink && file_size < 60 {
            let raw = self.raw.lock();
            let total_len = file_size as usize;
            if offset as usize >= total_len {
                return Ok(0);
            }
            let bytes_to_copy = core::cmp::min(buf.len(), total_len - offset as usize);
            let i_block = raw.i_block;
            let raw_block_ptr = i_block.as_ptr() as *const u8;
            unsafe {
                let src = raw_block_ptr.add(offset as usize);
                core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), bytes_to_copy);
            }
            return Ok(bytes_to_copy);
        }

        let mut read_bytes = 0;
        let mut current_offset = offset;

        while read_bytes < buf.len() && current_offset < file_size {
            let file_block = (current_offset / self.fs.block_size as u64) as u32;
            let block_offset = (current_offset % self.fs.block_size as u64) as usize;

            let phys_block = match self.resolve_block(file_block) {
                Ok(b) => b,
                Err(_) => return Err(-5), // EIO
            };

            let bytes_to_read = core::cmp::min(
                buf.len() - read_bytes,
                core::cmp::min(
                    self.fs.block_size as usize - block_offset,
                    (file_size - current_offset) as usize,
                ),
            );

            if phys_block == 0 {
                for b in &mut buf[read_bytes..read_bytes + bytes_to_read] {
                    *b = 0;
                }
            } else {
                let mut block_buf = [0u8; 4096];
                let block_size = self.fs.block_size as usize;
                assert!(block_size <= 4096);
                if read_blocks(
                    &*self.fs.device,
                    phys_block as u64,
                    &mut block_buf[..block_size],
                    self.fs.block_size,
                )
                .is_err()
                {
                    return Err(-5); // EIO
                }
                buf[read_bytes..read_bytes + bytes_to_read]
                    .copy_from_slice(&block_buf[block_offset..block_offset + bytes_to_read]);
            }

            read_bytes += bytes_to_read;
            current_offset += bytes_to_read as u64;
        }

        Ok(read_bytes)
    }

    /// Write data to regular file or symlink.
    pub fn write_file(&self, offset: u64, buf: &[u8]) -> Result<usize, i32> {
        let mut raw = self.raw.lock();
        let mut vfs = self.vfs_inode.write();

        if vfs.file_type == FileType::Symlink && (offset + buf.len() as u64) < 60 {
            let mut i_block = raw.i_block;
            let i_block_ptr = i_block.as_mut_ptr() as *mut u8;
            unsafe {
                let dest = i_block_ptr.add(offset as usize);
                core::ptr::copy_nonoverlapping(buf.as_ptr(), dest, buf.len());
            }
            raw.i_block = i_block;
            let new_size = offset + buf.len() as u64;
            if new_size > vfs.size {
                vfs.size = new_size;
                raw.i_size = new_size as u32;
            }
            self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?;
            return Ok(buf.len());
        }

        let mut written_bytes = 0;
        let mut current_offset = offset;

        while written_bytes < buf.len() {
            let file_block = (current_offset / self.fs.block_size as u64) as u32;
            let block_offset = (current_offset % self.fs.block_size as u64) as usize;

            let phys_block = self
                .get_or_alloc_block(&mut raw, file_block)
                .map_err(|_| -5)?; // EIO

            let bytes_to_write = core::cmp::min(
                buf.len() - written_bytes,
                self.fs.block_size as usize - block_offset,
            );

            let mut block_buf = [0u8; 4096];
            let block_size = self.fs.block_size as usize;
            assert!(block_size <= 4096);
            if read_blocks(
                &*self.fs.device,
                phys_block as u64,
                &mut block_buf[..block_size],
                self.fs.block_size,
            )
            .is_err()
            {
                return Err(-5); // EIO
            }

            block_buf[block_offset..block_offset + bytes_to_write]
                .copy_from_slice(&buf[written_bytes..written_bytes + bytes_to_write]);

            if write_blocks(
                &*self.fs.device,
                phys_block as u64,
                &block_buf[..block_size],
                self.fs.block_size,
            )
            .is_err()
            {
                return Err(-5); // EIO
            }

            written_bytes += bytes_to_write;
            current_offset += bytes_to_write as u64;
        }

        if current_offset > vfs.size {
            vfs.size = current_offset;
            raw.i_size = current_offset as u32;
        }
        vfs.blocks = raw.i_blocks as u64;

        self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?;

        Ok(written_bytes)
    }

    /// Truncate file size to 0.
    pub fn truncate_file(&self, size: u64) -> Result<(), i32> {
        if size == 0 {
            let mut raw = self.raw.lock();
            let mut vfs = self.vfs_inode.write();

            let mut i_block = raw.i_block;
            for block in &mut i_block[0..12] {
                if *block != 0 {
                    self.fs.deallocate_block(*block).map_err(|_| -5)?;
                    *block = 0;
                }
            }
            raw.i_block = i_block;

            let sib = raw.i_block[12];
            if sib != 0 {
                let mut ind_buf = [0u8; 4096];
                let block_size = self.fs.block_size as usize;
                assert!(block_size <= 4096);
                read_blocks(
                    &*self.fs.device,
                    sib as u64,
                    &mut ind_buf[..block_size],
                    self.fs.block_size,
                )
                .map_err(|_| -5)?;
                let refs_per_block = self.fs.block_size / 4;
                for j in 0..refs_per_block {
                    let ptr_offset = (j * 4) as usize;
                    let phys_block = u32::from_le_bytes([
                        ind_buf[ptr_offset],
                        ind_buf[ptr_offset + 1],
                        ind_buf[ptr_offset + 2],
                        ind_buf[ptr_offset + 3],
                    ]);
                    if phys_block != 0 {
                        self.fs.deallocate_block(phys_block).map_err(|_| -5)?;
                    }
                }
                self.fs.deallocate_block(sib).map_err(|_| -5)?;
                raw.i_block[12] = 0;
            }

            let dib = raw.i_block[13];
            if dib != 0 {
                let mut dib_buf = [0u8; 4096];
                let block_size = self.fs.block_size as usize;
                assert!(block_size <= 4096);
                read_blocks(
                    &*self.fs.device,
                    dib as u64,
                    &mut dib_buf[..block_size],
                    self.fs.block_size,
                )
                .map_err(|_| -5)?;
                let refs_per_block = self.fs.block_size / 4;
                for i in 0..refs_per_block {
                    let sib_offset = (i * 4) as usize;
                    let sib = u32::from_le_bytes([
                        dib_buf[sib_offset],
                        dib_buf[sib_offset + 1],
                        dib_buf[sib_offset + 2],
                        dib_buf[sib_offset + 3],
                    ]);
                    if sib != 0 {
                        let mut sib_buf = [0u8; 4096];
                        read_blocks(
                            &*self.fs.device,
                            sib as u64,
                            &mut sib_buf[..block_size],
                            self.fs.block_size,
                        )
                        .map_err(|_| -5)?;
                        for j in 0..refs_per_block {
                            let ptr_offset = (j * 4) as usize;
                            let phys_block = u32::from_le_bytes([
                                sib_buf[ptr_offset],
                                sib_buf[ptr_offset + 1],
                                sib_buf[ptr_offset + 2],
                                sib_buf[ptr_offset + 3],
                            ]);
                            if phys_block != 0 {
                                self.fs.deallocate_block(phys_block).map_err(|_| -5)?;
                            }
                        }
                        self.fs.deallocate_block(sib).map_err(|_| -5)?;
                    }
                }
                self.fs.deallocate_block(dib).map_err(|_| -5)?;
                raw.i_block[13] = 0;
            }

            raw.i_size = 0;
            raw.i_blocks = 0;
            vfs.size = 0;
            vfs.blocks = 0;

            self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?;
            Ok(())
        } else {
            let mut raw = self.raw.lock();
            let mut vfs = self.vfs_inode.write();
            raw.i_size = size as u32;
            vfs.size = size;
            self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?;
            Ok(())
        }
    }

    /// Read data from regular file or symlink using the Page Cache.
    pub fn read_page_cache(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let file_size = self.inode().size;
        if offset >= file_size {
            return Ok(0);
        }

        let mut read_bytes = 0;
        let mut current_offset = offset;

        while read_bytes < buf.len() && current_offset < file_size {
            let file_block_offset = current_offset & !4095;
            let page_offset = (current_offset % 4096) as usize;

            let phys_page = match crate::memory::page_cache::get_or_create_page_inner(
                self,
                file_block_offset,
            ) {
                Ok(p) => p,
                Err(_) => return Err(-5), // EIO
            };

            let bytes_to_read = core::cmp::min(
                buf.len() - read_bytes,
                core::cmp::min(4096 - page_offset, (file_size - current_offset) as usize),
            );

            let phys_offset = phys_page + crate::memory::r#virtual::phys_mem_offset();
            let src_slice = unsafe { core::slice::from_raw_parts(phys_offset as *const u8, 4096) };

            buf[read_bytes..read_bytes + bytes_to_read]
                .copy_from_slice(&src_slice[page_offset..page_offset + bytes_to_read]);

            read_bytes += bytes_to_read;
            current_offset += bytes_to_read as u64;
        }

        Ok(read_bytes)
    }

    /// Write data to regular file or symlink using the Page Cache.
    pub fn write_page_cache(&self, offset: u64, buf: &[u8]) -> Result<usize, i32> {
        let mut raw = self.raw.lock();
        let mut vfs = self.vfs_inode.write();

        let mut written_bytes = 0;
        let mut current_offset = offset;

        while written_bytes < buf.len() {
            let file_block = (current_offset / self.fs.block_size as u64) as u32;
            let block_offset = (current_offset % self.fs.block_size as u64) as usize;

            // Resolve or allocate physical disk block to reserve disk space
            let _phys_block = self
                .get_or_alloc_block(&mut raw, file_block)
                .map_err(|_| -5)?; // EIO

            let bytes_to_write = core::cmp::min(
                buf.len() - written_bytes,
                self.fs.block_size as usize - block_offset,
            );

            let file_block_offset = current_offset & !4095;
            let page_offset = (current_offset % 4096) as usize;

            // Drop locks before accessing page cache to prevent double-locking deadlocks on self.vfs_inode
            drop(raw);
            drop(vfs);

            let page_phys = match crate::memory::page_cache::get_or_create_page_inner(
                self,
                file_block_offset,
            ) {
                Ok(p) => p,
                Err(_) => return Err(-5), // EIO
            };

            let phys_offset = page_phys + crate::memory::r#virtual::phys_mem_offset();
            let dest_slice =
                unsafe { core::slice::from_raw_parts_mut(phys_offset as *mut u8, 4096) };

            dest_slice[page_offset..page_offset + bytes_to_write]
                .copy_from_slice(&buf[written_bytes..written_bytes + bytes_to_write]);

            crate::memory::page_cache::mark_dirty(self.ino as u64, file_block_offset);

            written_bytes += bytes_to_write;
            current_offset += bytes_to_write as u64;

            // Re-acquire locks for the next block allocation check or loop finalization
            raw = self.raw.lock();
            vfs = self.vfs_inode.write();
        }

        if current_offset > vfs.size {
            vfs.size = current_offset;
            raw.i_size = current_offset as u32;
        }
        vfs.blocks = raw.i_blocks as u64;

        self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?;

        Ok(written_bytes)
    }
}
