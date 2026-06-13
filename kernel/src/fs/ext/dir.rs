//! Directory operations for ext filesystems.

use super::{read_blocks, write_blocks};
use super::{ExtInode, ExtRawInode};
use crate::fs::inode::{DirEntry, FileType, InodeOps};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

impl ExtInode {
    /// Add a directory entry to the parent.
    pub fn add_directory_entry(
        &self,
        child_ino: u32,
        child_name: &str,
        child_type: FileType,
    ) -> Result<(), &'static str> {
        let mut raw = self.raw.lock();
        let mut vfs = self.vfs_inode.lock();

        let file_size = vfs.size;
        let block_size = self.fs.block_size;

        let file_type_byte: u8 = match child_type {
            FileType::Regular => 1,
            FileType::Directory => 2,
            FileType::Symlink => 7,
            _ => 1,
        };

        let new_entry_min_len = ((8 + child_name.len() + 3) & !3) as usize;
        let mut offset = 0u64;

        while offset < file_size {
            let file_block = (offset / block_size as u64) as u32;
            let phys_block = self.resolve_block_with_raw(&raw, file_block)?;
            if phys_block == 0 {
                break;
            }

            let mut block_buf = alloc::vec![0u8; block_size as usize];
            read_blocks(
                &*self.fs.device,
                phys_block as u64,
                &mut block_buf,
                block_size,
            )?;

            let mut ptr = 0;
            while ptr < block_size as usize {
                let inode = u32::from_le_bytes([
                    block_buf[ptr],
                    block_buf[ptr + 1],
                    block_buf[ptr + 2],
                    block_buf[ptr + 3],
                ]);
                let rec_len = u16::from_le_bytes([block_buf[ptr + 4], block_buf[ptr + 5]]) as usize;
                let name_len = block_buf[ptr + 6] as usize;

                if rec_len == 0 {
                    break;
                }

                let actual_rec_len = ((8 + name_len + 3) & !3) as usize;

                if inode != 0 {
                    let free_space = rec_len - actual_rec_len;
                    if free_space >= new_entry_min_len {
                        let new_rec_len = actual_rec_len as u16;
                        block_buf[ptr + 4..ptr + 6].copy_from_slice(&new_rec_len.to_le_bytes());

                        let new_ptr = ptr + actual_rec_len;
                        let remaining_rec_len = free_space as u16;

                        block_buf[new_ptr..new_ptr + 4].copy_from_slice(&child_ino.to_le_bytes());
                        block_buf[new_ptr + 4..new_ptr + 6]
                            .copy_from_slice(&remaining_rec_len.to_le_bytes());
                        block_buf[new_ptr + 6] = child_name.len() as u8;
                        block_buf[new_ptr + 7] = file_type_byte;
                        block_buf[new_ptr + 8..new_ptr + 8 + child_name.len()]
                            .copy_from_slice(child_name.as_bytes());

                        write_blocks(&*self.fs.device, phys_block as u64, &block_buf, block_size)?;
                        return Ok(());
                    }
                } else {
                    // Reuse deleted slot
                    if rec_len >= new_entry_min_len {
                        block_buf[ptr..ptr + 4].copy_from_slice(&child_ino.to_le_bytes());
                        block_buf[ptr + 6] = child_name.len() as u8;
                        block_buf[ptr + 7] = file_type_byte;
                        block_buf[ptr + 8..ptr + 8 + child_name.len()]
                            .copy_from_slice(child_name.as_bytes());

                        write_blocks(&*self.fs.device, phys_block as u64, &block_buf, block_size)?;
                        return Ok(());
                    }
                }
                ptr += rec_len;
            }
            offset += block_size as u64;
        }

        // Allocate a new block for the directory if no existing slots found
        let file_block = (file_size / block_size as u64) as u32;
        let phys_block = self.get_or_alloc_block(&mut raw, file_block)?;

        let mut block_buf = alloc::vec![0u8; block_size as usize];
        block_buf[0..4].copy_from_slice(&child_ino.to_le_bytes());
        let rec_len = block_size as u16;
        block_buf[4..6].copy_from_slice(&rec_len.to_le_bytes());
        block_buf[6] = child_name.len() as u8;
        block_buf[7] = file_type_byte;
        block_buf[8..8 + child_name.len()].copy_from_slice(child_name.as_bytes());

        write_blocks(&*self.fs.device, phys_block as u64, &block_buf, block_size)?;

        vfs.size += block_size as u64;
        raw.i_size = vfs.size as u32;
        self.fs.write_inode(self.ino, &raw)?;

        Ok(())
    }

    /// Remove a directory entry from the parent.
    pub fn remove_directory_entry(&self, child_name: &str) -> Result<u32, &'static str> {
        let vfs = self.vfs_inode.lock();
        let block_size = self.fs.block_size;
        let file_size = vfs.size;

        let mut offset = 0u64;
        while offset < file_size {
            let file_block = (offset / block_size as u64) as u32;
            let phys_block = self.resolve_block(file_block)?;
            if phys_block == 0 {
                break;
            }

            let mut block_buf = alloc::vec![0u8; block_size as usize];
            read_blocks(
                &*self.fs.device,
                phys_block as u64,
                &mut block_buf,
                block_size,
            )?;

            let mut ptr = 0;
            let mut prev_ptr = None;
            while ptr < block_size as usize {
                let inode = u32::from_le_bytes([
                    block_buf[ptr],
                    block_buf[ptr + 1],
                    block_buf[ptr + 2],
                    block_buf[ptr + 3],
                ]);
                let rec_len = u16::from_le_bytes([block_buf[ptr + 4], block_buf[ptr + 5]]) as usize;
                let name_len = block_buf[ptr + 6] as usize;

                if rec_len == 0 {
                    break;
                }

                if inode != 0 && name_len == child_name.len() {
                    let name_bytes = &block_buf[ptr + 8..ptr + 8 + name_len];
                    if let Ok(name_str) = core::str::from_utf8(name_bytes) {
                        if name_str == child_name {
                            if let Some(prev) = prev_ptr {
                                let prev_rec_len =
                                    u16::from_le_bytes([block_buf[prev + 4], block_buf[prev + 5]])
                                        as usize;
                                let merged_rec_len = (prev_rec_len + rec_len) as u16;
                                block_buf[prev + 4..prev + 6]
                                    .copy_from_slice(&merged_rec_len.to_le_bytes());
                            } else {
                                let zero_ino = 0u32;
                                block_buf[ptr..ptr + 4].copy_from_slice(&zero_ino.to_le_bytes());
                            }
                            write_blocks(
                                &*self.fs.device,
                                phys_block as u64,
                                &block_buf,
                                block_size,
                            )?;
                            return Ok(inode);
                        }
                    }
                }

                prev_ptr = Some(ptr);
                ptr += rec_len;
            }
            offset += block_size as u64;
        }
        Err("Directory entry not found")
    }

    /// Implement VFS create.
    pub fn create_dir_entry(&self, name: &str, file_type: FileType) -> Option<Arc<dyn InodeOps>> {
        if file_type != FileType::Regular
            && file_type != FileType::Directory
            && file_type != FileType::Symlink
        {
            return None;
        }

        let is_dir = file_type == FileType::Directory;
        let child_ino = self.fs.allocate_inode(is_dir).ok()?;

        let mut raw_child = ExtRawInode {
            i_mode: match file_type {
                FileType::Directory => 0x4000 | 0o755,
                FileType::Symlink => 0xA000 | 0o777,
                _ => 0x8000 | 0o644,
            },
            i_uid: 0,
            i_size: 0,
            i_atime: 0,
            i_ctime: 0,
            i_mtime: 0,
            i_dtime: 0,
            i_gid: 0,
            i_links_count: if is_dir { 2 } else { 1 },
            i_blocks: 0,
            i_flags: 0,
            i_osd1: 0,
            i_block: [0; 15],
            i_generation: 0,
            i_file_acl: 0,
            i_dir_acl: 0,
            i_faddr: 0,
            i_osd2: [0; 12],
        };

        if is_dir {
            let block = self.fs.allocate_block().ok()?;
            raw_child.i_block[0] = block;
            raw_child.i_blocks = self.fs.block_size / 512;
            raw_child.i_size = self.fs.block_size;

            let mut block_buf = alloc::vec![0u8; self.fs.block_size as usize];

            let dot_ino = child_ino;
            block_buf[0..4].copy_from_slice(&dot_ino.to_le_bytes());
            let dot_rec_len = 12u16;
            block_buf[4..6].copy_from_slice(&dot_rec_len.to_le_bytes());
            block_buf[6] = 1;
            block_buf[7] = 2;
            block_buf[8] = b'.';

            let dotdot_ino = self.ino;
            let dotdot_ptr = 12usize;
            block_buf[dotdot_ptr..dotdot_ptr + 4].copy_from_slice(&dotdot_ino.to_le_bytes());
            let dotdot_rec_len = (self.fs.block_size - 12) as u16;
            block_buf[dotdot_ptr + 4..dotdot_ptr + 6]
                .copy_from_slice(&dotdot_rec_len.to_le_bytes());
            block_buf[dotdot_ptr + 6] = 2;
            block_buf[dotdot_ptr + 7] = 2;
            block_buf[dotdot_ptr + 8] = b'.';
            block_buf[dotdot_ptr + 9] = b'.';

            write_blocks(
                &*self.fs.device,
                block as u64,
                &block_buf,
                self.fs.block_size,
            )
            .ok()?;
        }

        self.fs.write_inode(child_ino, &raw_child).ok()?;

        self.add_directory_entry(child_ino, name, file_type).ok()?;

        if is_dir {
            let mut parent_raw = self.raw.lock();
            parent_raw.i_links_count += 1;
            self.fs.write_inode(self.ino, &parent_raw).ok()?;
            self.vfs_inode.lock().nlink = parent_raw.i_links_count as u32;
        }

        self.fs.get_inode(child_ino).ok()
    }

    /// Implement VFS unlink.
    pub fn unlink_dir_entry(&self, name: &str) -> Result<(), i32> {
        let child_ino = self.remove_directory_entry(name).map_err(|_| -2)?; // ENOENT
        self.fs
            .decrement_links_count(child_ino, false)
            .map_err(|_| -5)?; // EIO
        Ok(())
    }

    /// Implement VFS mkdir.
    pub fn mkdir_dir_entry(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        self.create_dir_entry(name, FileType::Directory)
    }

    /// Implement VFS rmdir.
    pub fn rmdir_dir_entry(&self, name: &str) -> Result<(), i32> {
        let child = self.lookup(name).ok_or(-2)?; // ENOENT
        if !child.inode().is_dir() {
            return Err(-20); // ENOTDIR
        }

        let entries = child.readdir();
        let non_trivial = entries.iter().any(|e| e.name != "." && e.name != "..");
        if non_trivial {
            return Err(-39); // ENOTEMPTY
        }

        let child_ino = self.remove_directory_entry(name).map_err(|_| -2)?; // ENOENT

        let mut parent_raw = self.raw.lock();
        if parent_raw.i_links_count > 2 {
            parent_raw.i_links_count -= 1;
        }
        self.fs.write_inode(self.ino, &parent_raw).map_err(|_| -5)?;
        self.vfs_inode.lock().nlink = parent_raw.i_links_count as u32;

        self.fs
            .decrement_links_count(child_ino, true)
            .map_err(|_| -5)?;
        self.fs
            .decrement_links_count(child_ino, true)
            .map_err(|_| -5)?;

        Ok(())
    }

    /// Implement VFS readdir.
    pub fn readdir_dir_entry(&self) -> Vec<DirEntry> {
        let is_dir = self.vfs_inode.lock().is_dir();
        if !is_dir {
            return Vec::new();
        }

        let mut entries = Vec::new();
        let mut offset = 0u64;
        let file_size = self.vfs_inode.lock().size;

        while offset < file_size {
            let file_block = (offset / self.fs.block_size as u64) as u32;
            let block_offset = (offset % self.fs.block_size as u64) as usize;

            let phys_block = match self.resolve_block(file_block) {
                Ok(b) => b,
                Err(_) => break,
            };

            if phys_block == 0 {
                break;
            }

            let mut block_buf = alloc::vec![0u8; self.fs.block_size as usize];
            if read_blocks(
                &*self.fs.device,
                phys_block as u64,
                &mut block_buf,
                self.fs.block_size,
            )
            .is_err()
            {
                break;
            }

            let mut ptr = block_offset;
            while ptr + 8 <= self.fs.block_size as usize {
                let inode = u32::from_le_bytes([
                    block_buf[ptr],
                    block_buf[ptr + 1],
                    block_buf[ptr + 2],
                    block_buf[ptr + 3],
                ]);
                let rec_len = u16::from_le_bytes([block_buf[ptr + 4], block_buf[ptr + 5]]) as usize;
                let name_len = block_buf[ptr + 6] as usize;
                let file_type_byte = block_buf[ptr + 7];

                if rec_len == 0 {
                    break;
                }

                if inode != 0 && ptr + 8 + name_len <= self.fs.block_size as usize {
                    let name_bytes = &block_buf[ptr + 8..ptr + 8 + name_len];
                    if let Ok(name_str) = core::str::from_utf8(name_bytes) {
                        let file_type = match file_type_byte {
                            1 => FileType::Regular,
                            2 => FileType::Directory,
                            7 => FileType::Symlink,
                            _ => FileType::Regular,
                        };
                        entries.push(DirEntry {
                            name: String::from(name_str),
                            ino: inode as u64,
                            file_type,
                        });
                    }
                }

                ptr += rec_len;
            }

            offset += (self.fs.block_size as usize - block_offset) as u64;
        }

        entries
    }

    /// Implement VFS lookup.
    pub fn lookup_dir_entry(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        for entry in self.readdir() {
            if entry.name == name {
                return self.fs.get_inode(entry.ino as u32).ok();
            }
        }
        None
    }
}
