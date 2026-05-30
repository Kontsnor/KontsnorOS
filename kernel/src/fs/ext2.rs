//! ext2 read-only filesystem driver for KontsnorOS.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use crate::drivers::traits::BlockDevice;
use crate::fs::inode::{DirEntry, FilePermissions, FileType, Inode, InodeOps};
use crate::fs::vfs::{FileSystem, FsStats};
use crate::kprintln;

/// ext2 superblock structure (located at offset 1024).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Superblock {
    pub s_inodes_count: u32,
    pub s_blocks_count: u32,
    pub s_r_blocks_count: u32,
    pub s_free_blocks_count: u32,
    pub s_free_inodes_count: u32,
    pub s_first_data_block: u32,
    pub s_log_block_size: u32,
    pub s_log_frag_size: u32,
    pub s_blocks_per_group: u32,
    pub s_frags_per_group: u32,
    pub s_inodes_per_group: u32,
    pub s_mtime: u32,
    pub s_wtime: u32,
    pub s_mnt_count: u16,
    pub s_max_mnt_count: u16,
    pub s_magic: u16,
    pub s_state: u16,
    pub s_errors: u16,
    pub s_minor_rev_level: u16,
    pub s_lastcheck: u32,
    pub s_checkinterval: u32,
    pub s_creator_os: u32,
    pub s_rev_level: u32,
    pub s_def_resuid: u16,
    pub s_def_resgid: u16,
    pub s_first_ino: u32,
    pub s_inode_size: u16,
}

/// ext2 block group descriptor (32 bytes).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct GroupDescriptor {
    pub bg_block_bitmap: u32,
    pub bg_inode_bitmap: u32,
    pub bg_inode_table: u32,
    pub bg_free_blocks_count: u16,
    pub bg_free_inodes_count: u16,
    pub bg_used_dirs_count: u16,
    pub bg_pad: u16,
    pub bg_reserved: [u8; 12],
}

/// ext2 raw inode structure on disk (128 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Ext2RawInode {
    pub i_mode: u16,
    pub i_uid: u16,
    pub i_size: u32,
    pub i_atime: u32,
    pub i_ctime: u32,
    pub i_mtime: u32,
    pub i_dtime: u32,
    pub i_gid: u16,
    pub i_links_count: u16,
    pub i_blocks: u32,
    pub i_flags: u32,
    pub i_osd1: u32,
    pub i_block: [u32; 15],
    pub i_generation: u32,
    pub i_file_acl: u32,
    pub i_dir_acl: u32,
    pub i_faddr: u32,
    pub i_osd2: [u8; 12],
}

/// Logical-to-physical block reading helper.
fn read_blocks(device: &dyn BlockDevice, block: u64, buf: &mut [u8], block_size: u32) -> Result<(), &'static str> {
    let dev_block_size = device.block_size();
    let dev_blocks_per_fs_block = (block_size as u64) / dev_block_size;
    let start_dev_block = block * dev_blocks_per_fs_block;
    
    for i in 0..dev_blocks_per_fs_block {
        let offset = (i * dev_block_size) as usize;
        device.read_block(start_dev_block + i, &mut buf[offset..offset + dev_block_size as usize])
            .map_err(|_| "Block device read error")?;
    }
    Ok(())
}

/// ext2 FileSystem implementation.
pub struct Ext2FileSystem {
    device: Arc<dyn BlockDevice>,
    block_size: u32,
    inodes_per_block: u32,
    inodes_per_group: u32,
    inode_size: u16,
    group_descriptors: Vec<GroupDescriptor>,
    root_node: Mutex<Option<Arc<dyn InodeOps>>>,
}

impl Ext2FileSystem {
    /// Mount an ext2 volume on a block device.
    pub fn mount(device: Arc<dyn BlockDevice>) -> Result<Arc<Self>, &'static str> {
        let mut sb_buf = [0u8; 1024];
        
        // Superblock starts at offset 1024 (sectors 2 and 3 of 512-byte physical sectors)
        device.read_block(2, &mut sb_buf[0..512]).map_err(|_| "Error reading superblock low")?;
        device.read_block(3, &mut sb_buf[512..1024]).map_err(|_| "Error reading superblock high")?;
        
        let sb = unsafe { core::ptr::read_unaligned(sb_buf.as_ptr() as *const Superblock) };
        let s_magic = sb.s_magic;
        let s_log_block_size = sb.s_log_block_size;
        let s_inode_size = sb.s_inode_size;
        let s_inodes_per_group = sb.s_inodes_per_group;

        if s_magic != 0xEF53 {
            return Err("Invalid ext2 superblock magic");
        }
        
        let block_size = 1024 << s_log_block_size;
        let inode_size = s_inode_size;
        let inodes_per_group = s_inodes_per_group;
        let inodes_per_block = block_size / inode_size as u32;

        kprintln!("[ext2] Volume detected. s_magic: {:#x}, Block Size: {}, Inode Size: {}", 
            s_magic, block_size, inode_size);

        // Read Group Descriptor Table
        let gdt_block = if block_size == 1024 { 2 } else { 1 };
        let mut gdt_buf = alloc::vec![0u8; block_size as usize];
        read_blocks(&*device, gdt_block, &mut gdt_buf, block_size)?;

        let gd = unsafe { core::ptr::read_unaligned(gdt_buf.as_ptr() as *const GroupDescriptor) };
        let mut group_descriptors = Vec::new();
        group_descriptors.push(gd);

        let fs = Arc::new(Self {
            device: device.clone(),
            block_size,
            inodes_per_block,
            inodes_per_group,
            inode_size,
            group_descriptors,
            root_node: Mutex::new(None),
        });

        // Parse root directory (Inode 2)
        let root = fs.get_inode(2)?;
        *fs.root_node.lock() = Some(root);

        Ok(fs)
    }

    /// Retrieve an inode by its number.
    pub fn get_inode(self: &Arc<Self>, ino: u32) -> Result<Arc<dyn InodeOps>, &'static str> {
        if ino == 0 {
            return Err("Invalid inode number 0");
        }

        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;
        
        let gd = self.group_descriptors.get(group as usize)
            .ok_or("Group descriptor index out of bounds")?;

        let table_block = gd.bg_inode_table as u64;
        let inode_offset_in_table = (index * self.inode_size as u32) as u64;
        
        let logical_block = table_block + (inode_offset_in_table / self.block_size as u64);
        let offset_in_block = (inode_offset_in_table % self.block_size as u64) as usize;

        let mut block_buf = alloc::vec![0u8; self.block_size as usize];
        read_blocks(&*self.device, logical_block, &mut block_buf, self.block_size)?;

        let raw_inode = unsafe {
            core::ptr::read_unaligned(block_buf[offset_in_block..].as_ptr() as *const Ext2RawInode)
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
            _ => FileType::Regular,
        };

        let mut inode = Inode::new(ino as u64, file_type);
        inode.size = i_size as u64;
        inode.permissions = FilePermissions::new(i_mode);
        inode.nlink = i_links_count as u32;
        inode.uid = i_uid as u32;
        inode.gid = i_gid as u32;
        inode.blocks = i_blocks as u64;

        Ok(Arc::new(Ext2Inode {
            fs: self.clone(),
            ino,
            raw: raw_inode,
            vfs_inode: inode,
        }))
    }
}

/// ext2 Inode wrapper implementing InodeOps.
pub struct Ext2Inode {
    fs: Arc<Ext2FileSystem>,
    ino: u32,
    raw: Ext2RawInode,
    vfs_inode: Inode,
}

impl Ext2Inode {
    /// Resolve logical block number to physical disk block.
    fn resolve_block(&self, file_block: u32) -> Result<u32, &'static str> {
        let i_block = self.raw.i_block;
        
        if file_block < 12 {
            // Direct blocks
            return Ok(i_block[file_block as usize]);
        }
        
        let indirect_index = file_block - 12;
        let refs_per_block = self.fs.block_size / 4; // 32-bit addresses
        
        if indirect_index < refs_per_block {
            let sib = i_block[12];
            if sib == 0 {
                return Ok(0);
            }
            
            let mut ind_buf = alloc::vec![0u8; self.fs.block_size as usize];
            read_blocks(&*self.fs.device, sib as u64, &mut ind_buf, self.fs.block_size)?;
            
            let ptr_offset = (indirect_index * 4) as usize;
            let phys_block = u32::from_le_bytes([
                ind_buf[ptr_offset], ind_buf[ptr_offset + 1], ind_buf[ptr_offset + 2], ind_buf[ptr_offset + 3]
            ]);
            return Ok(phys_block);
        }

        Err("Double/triple indirect blocks are unsupported in this phase.")
    }
}

impl InodeOps for Ext2Inode {
    fn inode(&self) -> &Inode {
        &self.vfs_inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if offset >= self.vfs_inode.size {
            return Ok(0);
        }

        let mut read_bytes = 0;
        let file_size = self.vfs_inode.size;
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
                    (file_size - current_offset) as usize
                )
            );

            if phys_block == 0 {
                // Sparse block filled with zeroes
                for b in &mut buf[read_bytes..read_bytes + bytes_to_read] {
                    *b = 0;
                }
            } else {
                let mut block_buf = alloc::vec![0u8; self.fs.block_size as usize];
                if read_blocks(&*self.fs.device, phys_block as u64, &mut block_buf, self.fs.block_size).is_err() {
                    return Err(-5); // EIO
                }
                buf[read_bytes..read_bytes + bytes_to_read].copy_from_slice(
                    &block_buf[block_offset..block_offset + bytes_to_read]
                );
            }

            read_bytes += bytes_to_read;
            current_offset += bytes_to_read as u64;
        }

        Ok(read_bytes)
    }

    fn readdir(&self) -> Vec<DirEntry> {
        if !self.vfs_inode.is_dir() {
            return Vec::new();
        }

        let mut entries = Vec::new();
        let mut offset = 0u64;
        let file_size = self.vfs_inode.size;

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
            if read_blocks(&*self.fs.device, phys_block as u64, &mut block_buf, self.fs.block_size).is_err() {
                break;
            }

            let mut ptr = block_offset;
            while ptr + 8 <= self.fs.block_size as usize {
                let inode = u32::from_le_bytes([
                    block_buf[ptr], block_buf[ptr + 1], block_buf[ptr + 2], block_buf[ptr + 3]
                ]);
                let rec_len = u16::from_le_bytes([
                    block_buf[ptr + 4], block_buf[ptr + 5]
                ]) as usize;
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

    fn lookup(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        for entry in self.readdir() {
            if entry.name == name {
                return self.fs.get_inode(entry.ino as u32).ok();
            }
        }
        None
    }
}

impl FileSystem for Ext2FileSystem {
    fn root(&self) -> Arc<dyn InodeOps> {
        self.root_node.lock().clone().expect("Root node missing")
    }

    fn name(&self) -> &str {
        "ext2"
    }

    fn statfs(&self) -> FsStats {
        FsStats {
            total_blocks: 64,
            free_blocks: 54,
            total_inodes: 32,
            free_inodes: 30,
            block_size: self.block_size as u64,
            max_name_len: 255,
        }
    }
}
