//! Regular file read, write, and size manipulation (truncate) operations.

use super::{read_blocks, write_blocks};
use super::{Ext2Inode, Ext2RawInode};
use crate::fs::inode::FileType;

impl Ext2Inode {
    /// Resolve logical block number to physical disk block using a provided raw inode reference.
    pub fn resolve_block_with_raw(
        &self,
        raw: &Ext2RawInode,
        file_block: u32,
    ) -> Result<u32, &'static str> {
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

            let mut ind_buf = alloc::vec![0u8; self.fs.block_size as usize];
            read_blocks(
                &*self.fs.device,
                sib as u64,
                &mut ind_buf,
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

            let mut dib_buf = alloc::vec![0u8; self.fs.block_size as usize];
            read_blocks(
                &*self.fs.device,
                dib as u64,
                &mut dib_buf,
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

            let mut sib_buf = alloc::vec![0u8; self.fs.block_size as usize];
            read_blocks(
                &*self.fs.device,
                sib as u64,
                &mut sib_buf,
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
        raw: &mut Ext2RawInode,
        file_block: u32,
    ) -> Result<u32, &'static str> {
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

            let mut ind_buf = alloc::vec![0u8; self.fs.block_size as usize];
            read_blocks(
                &*self.fs.device,
                sib as u64,
                &mut ind_buf,
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
                write_blocks(&*self.fs.device, sib as u64, &ind_buf, self.fs.block_size)?;

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

            let mut dib_buf = alloc::vec![0u8; self.fs.block_size as usize];
            read_blocks(
                &*self.fs.device,
                dib as u64,
                &mut dib_buf,
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
                write_blocks(&*self.fs.device, dib as u64, &dib_buf, self.fs.block_size)?;

                raw.i_blocks += self.fs.block_size / 512;
                self.fs.write_inode(self.ino, raw)?;
            }

            let mut sib_buf = alloc::vec![0u8; self.fs.block_size as usize];
            read_blocks(
                &*self.fs.device,
                sib as u64,
                &mut sib_buf,
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
                write_blocks(&*self.fs.device, sib as u64, &sib_buf, self.fs.block_size)?;

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
            let vfs = self.vfs_inode.lock();
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
                let mut block_buf = alloc::vec![0u8; self.fs.block_size as usize];
                if read_blocks(
                    &*self.fs.device,
                    phys_block as u64,
                    &mut block_buf,
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
        let mut vfs = self.vfs_inode.lock();

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

            let mut block_buf = alloc::vec![0u8; self.fs.block_size as usize];
            if read_blocks(
                &*self.fs.device,
                phys_block as u64,
                &mut block_buf,
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
                &block_buf,
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
        if size != 0 {
            return Err(-22); // EINVAL
        }

        let mut raw = self.raw.lock();
        let mut vfs = self.vfs_inode.lock();

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
            let mut ind_buf = alloc::vec![0u8; self.fs.block_size as usize];
            read_blocks(
                &*self.fs.device,
                sib as u64,
                &mut ind_buf,
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
            let mut dib_buf = alloc::vec![0u8; self.fs.block_size as usize];
            read_blocks(
                &*self.fs.device,
                dib as u64,
                &mut dib_buf,
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
                    let mut sib_buf = alloc::vec![0u8; self.fs.block_size as usize];
                    read_blocks(
                        &*self.fs.device,
                        sib as u64,
                        &mut sib_buf,
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
    }
}
