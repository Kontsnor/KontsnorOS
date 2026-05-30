//! ext2 writable filesystem driver for KontsnorOS.

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

/// Helper to count free bits (zeros) in a bitmap buffer.
fn count_free_bits(bitmap: &[u8], total_count: u32) -> u32 {
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

/// Logical-to-physical block writing helper.
fn write_blocks(device: &dyn BlockDevice, block: u64, buf: &[u8], block_size: u32) -> Result<(), &'static str> {
    let dev_block_size = device.block_size();
    let dev_blocks_per_fs_block = (block_size as u64) / dev_block_size;
    let start_dev_block = block * dev_blocks_per_fs_block;
    
    for i in 0..dev_blocks_per_fs_block {
        let offset = (i * dev_block_size) as usize;
        device.write_block(start_dev_block + i, &buf[offset..offset + dev_block_size as usize])
            .map_err(|_| "Block device write error")?;
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
    superblock: Mutex<Superblock>,
    group_descriptors: Mutex<Vec<GroupDescriptor>>,
    root_node: Mutex<Option<Arc<dyn InodeOps>>>,
}

impl Ext2FileSystem {
    /// Mount an ext2 volume on a block device.
    pub fn mount(device: Arc<dyn BlockDevice>) -> Result<Arc<Self>, &'static str> {
        let mut sb_buf = [0u8; 1024];
        
        // Superblock starts at offset 1024 (sectors 2 and 3 of 512-byte physical sectors)
        device.read_block(2, &mut sb_buf[0..512]).map_err(|_| "Error reading superblock low")?;
        device.read_block(3, &mut sb_buf[512..1024]).map_err(|_| "Error reading superblock high")?;
        
        let mut sb = unsafe { core::ptr::read_unaligned(sb_buf.as_ptr() as *const Superblock) };
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

        let mut gd = unsafe { core::ptr::read_unaligned(gdt_buf.as_ptr() as *const GroupDescriptor) };

        // Self-healing bitmaps check on mount
        let mut block_bitmap = alloc::vec![0u8; block_size as usize];
        read_blocks(&*device, gd.bg_block_bitmap as u64, &mut block_bitmap, block_size)?;
        let mut inode_bitmap = alloc::vec![0u8; block_size as usize];
        read_blocks(&*device, gd.bg_inode_bitmap as u64, &mut inode_bitmap, block_size)?;

        let mut block_bitmap_changed = false;
        if block_bitmap.iter().all(|&x| x == 0) {
            kprintln!("[ext2] Block bitmap is all zeros, healing...");
            block_bitmap[0] = 0xFF;
            block_bitmap[1] = 0xFF;
            block_bitmap[2] = 0xFF;
            block_bitmap[3] = 0xFF;
            block_bitmap[4] = 0x03; // 34 blocks total
            write_blocks(&*device, gd.bg_block_bitmap as u64, &block_bitmap, block_size)?;
            block_bitmap_changed = true;
        }

        let mut inode_bitmap_changed = false;
        if inode_bitmap.iter().all(|&x| x == 0) {
            kprintln!("[ext2] Inode bitmap is all zeros, healing...");
            inode_bitmap[0] = 0xFF;
            inode_bitmap[1] = 0x7F; // 15 inodes total
            write_blocks(&*device, gd.bg_inode_bitmap as u64, &inode_bitmap, block_size)?;
            inode_bitmap_changed = true;
        }

        if block_bitmap_changed || inode_bitmap_changed {
            let free_b = count_free_bits(&block_bitmap, sb.s_blocks_count);
            let free_i = count_free_bits(&inode_bitmap, sb.s_inodes_count);
            sb.s_free_blocks_count = free_b;
            sb.s_free_inodes_count = free_i;
            gd.bg_free_blocks_count = free_b as u16;
            gd.bg_free_inodes_count = free_i as u16;
            
            // Write superblock back
            let sb_ptr = &sb as *const Superblock as *const u8;
            let mut sb_buf_write = [0u8; 1024];
            unsafe {
                core::ptr::copy_nonoverlapping(sb_ptr, sb_buf_write.as_mut_ptr(), core::mem::size_of::<Superblock>());
            }
            device.write_block(2, &sb_buf_write[0..512]).map_err(|_| "Error writing superblock low")?;
            device.write_block(3, &sb_buf_write[512..1024]).map_err(|_| "Error writing superblock high")?;
            
            // Write gd back
            let gd_size = core::mem::size_of::<GroupDescriptor>();
            let src_ptr = &gd as *const GroupDescriptor as *const u8;
            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, gdt_buf.as_mut_ptr(), gd_size);
            }
            write_blocks(&*device, gdt_block, &gdt_buf, block_size)?;
        }

        // --- Phase 33: Consistency Check and Self-Healing (FSCK) ---
        let mut calc_block_bitmap = alloc::vec![0u8; block_size as usize];
        let mut calc_inode_bitmap = alloc::vec![0u8; block_size as usize];
        
        // 1. Mark reserved metadata blocks as allocated
        let it_blocks = (s_inodes_per_group * s_inode_size as u32 + block_size - 1) / block_size;
        let reserved_blocks_count = gd.bg_inode_table as u32 + it_blocks;
        for b in 0..reserved_blocks_count {
            let byte = (b / 8) as usize;
            let bit = b % 8;
            if byte < calc_block_bitmap.len() {
                calc_block_bitmap[byte] |= 1 << bit;
            }
        }
        
        // 2. Mark reserved inodes (1 to 10) as allocated
        for i in 0..10 {
            let byte = (i / 8) as usize;
            let bit = i % 8;
            calc_inode_bitmap[byte] |= 1 << bit;
        }
        
        // 3. Scan all inodes from 11 to sb.s_inodes_count
        let table_block = gd.bg_inode_table as u64;
        let mut block_cache_idx = 0u64;
        let mut block_cache_buf = alloc::vec![0u8; block_size as usize];
        
        for ino in 11..=sb.s_inodes_count {
            let index = (ino - 1) % s_inodes_per_group;
            let inode_offset_in_table = (index * s_inode_size as u32) as u64;
            let logical_block = table_block + (inode_offset_in_table / block_size as u64);
            let offset_in_block = (inode_offset_in_table % block_size as u64) as usize;
            
            if block_cache_idx != logical_block {
                read_blocks(&*device, logical_block, &mut block_cache_buf, block_size)?;
                block_cache_idx = logical_block;
            }
            
            let raw_inode = unsafe {
                core::ptr::read_unaligned(block_cache_buf[offset_in_block..].as_ptr() as *const Ext2RawInode)
            };
            
            if raw_inode.i_links_count > 0 {
                // Mark inode as allocated
                let i_idx = ino - 1;
                let byte = (i_idx / 8) as usize;
                let bit = i_idx % 8;
                if byte < calc_inode_bitmap.len() {
                    calc_inode_bitmap[byte] |= 1 << bit;
                }
                
                // Trace block pointers
                for file_block in 0..12 {
                    let block_num = raw_inode.i_block[file_block];
                    if block_num != 0 && block_num < sb.s_blocks_count {
                        let byte = (block_num / 8) as usize;
                        let bit = block_num % 8;
                        if byte < calc_block_bitmap.len() {
                            calc_block_bitmap[byte] |= 1 << bit;
                        }
                    }
                }
                
                let sib = raw_inode.i_block[12];
                if sib != 0 && sib < sb.s_blocks_count {
                    // Mark indirect block as allocated
                    let sib_byte = (sib / 8) as usize;
                    let sib_bit = sib % 8;
                    if sib_byte < calc_block_bitmap.len() {
                        calc_block_bitmap[sib_byte] |= 1 << sib_bit;
                    }
                    
                    // Read indirect block and trace its pointers
                    let mut ind_buf = alloc::vec![0u8; block_size as usize];
                    if read_blocks(&*device, sib as u64, &mut ind_buf, block_size).is_ok() {
                        let refs_per_block = block_size / 4;
                        for r in 0..refs_per_block {
                            let ptr_offset = (r * 4) as usize;
                            let phys_block = u32::from_le_bytes([
                                ind_buf[ptr_offset], ind_buf[ptr_offset + 1],
                                ind_buf[ptr_offset + 2], ind_buf[ptr_offset + 3]
                            ]);
                            if phys_block != 0 && phys_block < sb.s_blocks_count {
                                let byte = (phys_block / 8) as usize;
                                let bit = phys_block % 8;
                                if byte < calc_block_bitmap.len() {
                                    calc_block_bitmap[byte] |= 1 << bit;
                                }
                            }
                        }
                    }
                }
            }
        }
        
        // 4. Compare bitmaps and self-heal if necessary
        let mut mismatch = false;
        for i in 0..block_size as usize {
            if block_bitmap[i] != calc_block_bitmap[i] {
                mismatch = true;
                break;
            }
        }
        if !mismatch {
            for i in 0..block_size as usize {
                if inode_bitmap[i] != calc_inode_bitmap[i] {
                    mismatch = true;
                    break;
                }
            }
        }
        
        if mismatch {
            kprintln!("[ext2] Integrity mismatch found. Self-healing filesystem metadata...");
            
            write_blocks(&*device, gd.bg_block_bitmap as u64, &calc_block_bitmap, block_size)?;
            write_blocks(&*device, gd.bg_inode_bitmap as u64, &calc_inode_bitmap, block_size)?;
            
            let free_b = count_free_bits(&calc_block_bitmap, sb.s_blocks_count);
            let free_i = count_free_bits(&calc_inode_bitmap, sb.s_inodes_count);
            sb.s_free_blocks_count = free_b;
            sb.s_free_inodes_count = free_i;
            gd.bg_free_blocks_count = free_b as u16;
            gd.bg_free_inodes_count = free_i as u16;
            
            // Write superblock back
            let sb_ptr = &sb as *const Superblock as *const u8;
            let mut sb_buf_write = [0u8; 1024];
            unsafe {
                core::ptr::copy_nonoverlapping(sb_ptr, sb_buf_write.as_mut_ptr(), core::mem::size_of::<Superblock>());
            }
            device.write_block(2, &sb_buf_write[0..512]).map_err(|_| "Error writing superblock low")?;
            device.write_block(3, &sb_buf_write[512..1024]).map_err(|_| "Error writing superblock high")?;
            
            // Write gd back
            let gd_size = core::mem::size_of::<GroupDescriptor>();
            let src_ptr = &gd as *const GroupDescriptor as *const u8;
            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, gdt_buf.as_mut_ptr(), gd_size);
            }
            write_blocks(&*device, gdt_block, &gdt_buf, block_size)?;
        } else {
            kprintln!("[ext2] Filesystem consistency check succeeded. No corruption detected.");
        }

        let mut group_descriptors = Vec::new();
        group_descriptors.push(gd);

        let fs = Arc::new(Self {
            device: device.clone(),
            block_size,
            inodes_per_block,
            inodes_per_group,
            inode_size,
            superblock: Mutex::new(sb),
            group_descriptors: Mutex::new(group_descriptors),
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
        
        let gd = {
            let gds = self.group_descriptors.lock();
            gds.get(group as usize).copied()
                .ok_or("Group descriptor index out of bounds")?
        };

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
        inode.atime = raw_inode.i_atime as u64;
        inode.mtime = raw_inode.i_mtime as u64;
        inode.ctime = raw_inode.i_ctime as u64;

        Ok(Arc::new(Ext2Inode {
            fs: self.clone(),
            ino,
            raw: Mutex::new(raw_inode),
            vfs_inode: Mutex::new(inode),
        }))
    }

    /// Write superblock back to the block device.
    pub fn write_superblock(&self, sb: &Superblock) -> Result<(), &'static str> {
        let sb_ptr = sb as *const Superblock as *const u8;
        let mut sb_buf = [0u8; 1024];
        unsafe {
            core::ptr::copy_nonoverlapping(sb_ptr, sb_buf.as_mut_ptr(), core::mem::size_of::<Superblock>());
        }
        self.device.write_block(2, &sb_buf[0..512]).map_err(|_| "Error writing superblock low")?;
        self.device.write_block(3, &sb_buf[512..1024]).map_err(|_| "Error writing superblock high")?;
        Ok(())
    }

    /// Write group descriptors back to the block device.
    pub fn write_group_descriptors(&self) -> Result<(), &'static str> {
        let gds = self.group_descriptors.lock();
        let block_size = self.block_size;
        let gdt_block = if block_size == 1024 { 2 } else { 1 };
        
        let mut gdt_buf = alloc::vec![0u8; block_size as usize];
        let gd_size = core::mem::size_of::<GroupDescriptor>();
        for (i, gd) in gds.iter().enumerate() {
            let offset = i * gd_size;
            if offset + gd_size <= block_size as usize {
                let src_ptr = gd as *const GroupDescriptor as *const u8;
                unsafe {
                    core::ptr::copy_nonoverlapping(src_ptr, gdt_buf.as_mut_ptr().add(offset), gd_size);
                }
            }
        }
        write_blocks(&*self.device, gdt_block, &gdt_buf, block_size)?;
        Ok(())
    }

    /// Allocate a block from the filesystem block bitmap.
    pub fn allocate_block(&self) -> Result<u32, &'static str> {
        let mut sb = self.superblock.lock();
        let mut gds = self.group_descriptors.lock();
        
        if sb.s_free_blocks_count == 0 {
            return Err("No free blocks");
        }
        
        let gd = &mut gds[0];
        let mut bitmap = alloc::vec![0u8; self.block_size as usize];
        read_blocks(&*self.device, gd.bg_block_bitmap as u64, &mut bitmap, self.block_size)?;
        
        for i in 0..sb.s_blocks_count {
            let byte = (i / 8) as usize;
            let bit = i % 8;
            if (bitmap[byte] & (1 << bit)) == 0 {
                bitmap[byte] |= 1 << bit;
                write_blocks(&*self.device, gd.bg_block_bitmap as u64, &bitmap, self.block_size)?;
                
                sb.s_free_blocks_count -= 1;
                gd.bg_free_blocks_count -= 1;
                
                self.write_superblock(&sb)?;
                drop(gds);
                self.write_group_descriptors()?;
                
                // Zero out the newly allocated block
                let zero_buf = alloc::vec![0u8; self.block_size as usize];
                write_blocks(&*self.device, i as u64, &zero_buf, self.block_size)?;
                
                return Ok(i);
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
        
        let gd = &mut gds[0];
        let mut bitmap = alloc::vec![0u8; self.block_size as usize];
        read_blocks(&*self.device, gd.bg_block_bitmap as u64, &mut bitmap, self.block_size)?;
        
        let byte = (block_num / 8) as usize;
        let bit = block_num % 8;
        if (bitmap[byte] & (1 << bit)) != 0 {
            bitmap[byte] &= !(1 << bit);
            write_blocks(&*self.device, gd.bg_block_bitmap as u64, &bitmap, self.block_size)?;
            
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
        
        let gd = &mut gds[0];
        let mut bitmap = alloc::vec![0u8; self.block_size as usize];
        read_blocks(&*self.device, gd.bg_inode_bitmap as u64, &mut bitmap, self.block_size)?;
        
        for i in 0..sb.s_inodes_count {
            let byte = (i / 8) as usize;
            let bit = i % 8;
            if (bitmap[byte] & (1 << bit)) == 0 {
                bitmap[byte] |= 1 << bit;
                write_blocks(&*self.device, gd.bg_inode_bitmap as u64, &bitmap, self.block_size)?;
                
                sb.s_free_inodes_count -= 1;
                gd.bg_free_inodes_count -= 1;
                if is_dir {
                    gd.bg_used_dirs_count += 1;
                }
                
                self.write_superblock(&sb)?;
                drop(gds);
                self.write_group_descriptors()?;
                
                let ino = i + 1;
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
        
        let gd = &mut gds[0];
        let mut bitmap = alloc::vec![0u8; self.block_size as usize];
        read_blocks(&*self.device, gd.bg_inode_bitmap as u64, &mut bitmap, self.block_size)?;
        
        let i = ino - 1;
        let byte = (i / 8) as usize;
        let bit = i % 8;
        if (bitmap[byte] & (1 << bit)) != 0 {
            bitmap[byte] &= !(1 << bit);
            write_blocks(&*self.device, gd.bg_inode_bitmap as u64, &bitmap, self.block_size)?;
            
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
    pub fn write_inode(&self, ino: u32, raw_inode: &Ext2RawInode) -> Result<(), &'static str> {
        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;
        
        let gd = {
            let gds = self.group_descriptors.lock();
            gds.get(group as usize).copied()
                .ok_or("Group descriptor index out of bounds")?
        };

        let table_block = gd.bg_inode_table as u64;
        let inode_offset_in_table = (index * self.inode_size as u32) as u64;
        
        let logical_block = table_block + (inode_offset_in_table / self.block_size as u64);
        let offset_in_block = (inode_offset_in_table % self.block_size as u64) as usize;

        let mut block_buf = alloc::vec![0u8; self.block_size as usize];
        read_blocks(&*self.device, logical_block, &mut block_buf, self.block_size)?;
        
        let dst_ptr = block_buf[offset_in_block..].as_mut_ptr();
        let src_ptr = raw_inode as *const Ext2RawInode as *const u8;
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
            gds.get(group as usize).copied()
                .ok_or("Group descriptor index out of bounds")?
        };
        let table_block = gd.bg_inode_table as u64;
        let inode_offset_in_table = (index * self.inode_size as u32) as u64;
        let logical_block = table_block + (inode_offset_in_table / self.block_size as u64);
        let offset_in_block = (inode_offset_in_table % self.block_size as u64) as usize;

        let mut block_buf = alloc::vec![0u8; self.block_size as usize];
        read_blocks(&*self.device, logical_block, &mut block_buf, self.block_size)?;

        let mut raw_inode = unsafe {
            core::ptr::read_unaligned(block_buf[offset_in_block..].as_ptr() as *const Ext2RawInode)
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
                        ind_buf[ptr_offset], ind_buf[ptr_offset + 1], ind_buf[ptr_offset + 2], ind_buf[ptr_offset + 3]
                    ]);
                    if phys_block != 0 {
                        self.deallocate_block(phys_block)?;
                    }
                }
                self.deallocate_block(sib)?;
            }
            
            // Release the inode
            self.deallocate_inode(ino, is_dir)?;
        } else {
            let dst_ptr = block_buf[offset_in_block..].as_mut_ptr();
            let src_ptr = &raw_inode as *const Ext2RawInode as *const u8;
            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, self.inode_size as usize);
            }
            write_blocks(&*self.device, logical_block, &block_buf, self.block_size)?;
        }
        Ok(())
    }
}

/// ext2 Inode wrapper implementing InodeOps.
pub struct Ext2Inode {
    fs: Arc<Ext2FileSystem>,
    ino: u32,
    raw: Mutex<Ext2RawInode>,
    vfs_inode: Mutex<Inode>,
}

impl Ext2Inode {
    /// Resolve logical block number to physical disk block using a provided raw inode reference.
    fn resolve_block_with_raw(&self, raw: &Ext2RawInode, file_block: u32) -> Result<u32, &'static str> {
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
            read_blocks(&*self.fs.device, sib as u64, &mut ind_buf, self.fs.block_size)?;
            
            let ptr_offset = (indirect_index * 4) as usize;
            let phys_block = u32::from_le_bytes([
                ind_buf[ptr_offset], ind_buf[ptr_offset + 1], ind_buf[ptr_offset + 2], ind_buf[ptr_offset + 3]
            ]);
            return Ok(phys_block);
        }

        Err("Double/triple indirect blocks are unsupported in this phase.")
    }

    /// Resolve logical block number to physical disk block.
    fn resolve_block(&self, file_block: u32) -> Result<u32, &'static str> {
        let raw = self.raw.lock();
        self.resolve_block_with_raw(&raw, file_block)
    }

    /// Retrieve or dynamically allocate a physical disk block for a file block index.
    fn get_or_alloc_block(&self, raw: &mut Ext2RawInode, file_block: u32) -> Result<u32, &'static str> {
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
            read_blocks(&*self.fs.device, sib as u64, &mut ind_buf, self.fs.block_size)?;
            
            let ptr_offset = (indirect_index * 4) as usize;
            let mut phys_block = u32::from_le_bytes([
                ind_buf[ptr_offset], ind_buf[ptr_offset + 1], ind_buf[ptr_offset + 2], ind_buf[ptr_offset + 3]
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
        
        Err("Double/triple indirect blocks are unsupported in this phase.")
    }

    /// Add a directory entry to the parent.
    fn add_directory_entry(&self, child_ino: u32, child_name: &str, child_type: FileType) -> Result<(), &'static str> {
        let mut raw = self.raw.lock();
        let mut vfs = self.vfs_inode.lock();
        
        let file_size = vfs.size;
        let block_size = self.fs.block_size;
        
        let file_type_byte: u8 = match child_type {
            FileType::Regular => 1,
            FileType::Directory => 2,
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
            read_blocks(&*self.fs.device, phys_block as u64, &mut block_buf, block_size)?;
            
            let mut ptr = 0;
            while ptr < block_size as usize {
                let inode = u32::from_le_bytes([
                    block_buf[ptr], block_buf[ptr + 1], block_buf[ptr + 2], block_buf[ptr + 3]
                ]);
                let rec_len = u16::from_le_bytes([
                    block_buf[ptr + 4], block_buf[ptr + 5]
                ]) as usize;
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
                        block_buf[new_ptr + 4..new_ptr + 6].copy_from_slice(&remaining_rec_len.to_le_bytes());
                        block_buf[new_ptr + 6] = child_name.len() as u8;
                        block_buf[new_ptr + 7] = file_type_byte;
                        block_buf[new_ptr + 8..new_ptr + 8 + child_name.len()].copy_from_slice(child_name.as_bytes());
                        
                        write_blocks(&*self.fs.device, phys_block as u64, &block_buf, block_size)?;
                        return Ok(());
                    }
                } else {
                    // Reuse deleted slot
                    if rec_len >= new_entry_min_len {
                        block_buf[ptr..ptr + 4].copy_from_slice(&child_ino.to_le_bytes());
                        block_buf[ptr + 6] = child_name.len() as u8;
                        block_buf[ptr + 7] = file_type_byte;
                        block_buf[ptr + 8..ptr + 8 + child_name.len()].copy_from_slice(child_name.as_bytes());
                        
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
    fn remove_directory_entry(&self, child_name: &str) -> Result<u32, &'static str> {
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
            read_blocks(&*self.fs.device, phys_block as u64, &mut block_buf, block_size)?;
            
            let mut ptr = 0;
            let mut prev_ptr = None;
            while ptr < block_size as usize {
                let inode = u32::from_le_bytes([
                    block_buf[ptr], block_buf[ptr + 1], block_buf[ptr + 2], block_buf[ptr + 3]
                ]);
                let rec_len = u16::from_le_bytes([
                    block_buf[ptr + 4], block_buf[ptr + 5]
                ]) as usize;
                let name_len = block_buf[ptr + 6] as usize;
                
                if rec_len == 0 {
                    break;
                }
                
                if inode != 0 && name_len == child_name.len() {
                    let name_bytes = &block_buf[ptr + 8..ptr + 8 + name_len];
                    if let Ok(name_str) = core::str::from_utf8(name_bytes) {
                        if name_str == child_name {
                            if let Some(prev) = prev_ptr {
                                let prev_rec_len = u16::from_le_bytes([
                                    block_buf[prev + 4], block_buf[prev + 5]
                                ]) as usize;
                                let merged_rec_len = (prev_rec_len + rec_len) as u16;
                                block_buf[prev + 4..prev + 6].copy_from_slice(&merged_rec_len.to_le_bytes());
                            } else {
                                let zero_ino = 0u32;
                                block_buf[ptr..ptr + 4].copy_from_slice(&zero_ino.to_le_bytes());
                            }
                            write_blocks(&*self.fs.device, phys_block as u64, &block_buf, block_size)?;
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
}

impl InodeOps for Ext2Inode {
    fn inode(&self) -> &Inode {
        unsafe {
            &*(&*self.vfs_inode.lock() as *const Inode)
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let file_size = self.vfs_inode.lock().size;
        if offset >= file_size {
            return Ok(0);
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
                    (file_size - current_offset) as usize
                )
            );

            if phys_block == 0 {
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

    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, i32> {
        let mut raw = self.raw.lock();
        let mut vfs = self.vfs_inode.lock();
        
        let mut written_bytes = 0;
        let mut current_offset = offset;
        
        while written_bytes < buf.len() {
            let file_block = (current_offset / self.fs.block_size as u64) as u32;
            let block_offset = (current_offset % self.fs.block_size as u64) as usize;
            
            let phys_block = self.get_or_alloc_block(&mut raw, file_block)
                .map_err(|_| -5)?; // EIO
                
            let bytes_to_write = core::cmp::min(
                buf.len() - written_bytes,
                self.fs.block_size as usize - block_offset
            );
            
            let mut block_buf = alloc::vec![0u8; self.fs.block_size as usize];
            if read_blocks(&*self.fs.device, phys_block as u64, &mut block_buf, self.fs.block_size).is_err() {
                return Err(-5); // EIO
            }
            
            block_buf[block_offset..block_offset + bytes_to_write].copy_from_slice(
                &buf[written_bytes..written_bytes + bytes_to_write]
            );
            
            if write_blocks(&*self.fs.device, phys_block as u64, &block_buf, self.fs.block_size).is_err() {
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

    fn create(&self, name: &str, file_type: FileType) -> Option<Arc<dyn InodeOps>> {
        if file_type != FileType::Regular && file_type != FileType::Directory {
            return None;
        }
        
        let is_dir = file_type == FileType::Directory;
        let child_ino = self.fs.allocate_inode(is_dir).ok()?;
        
        let mut raw_child = Ext2RawInode {
            i_mode: if is_dir { 0x4000 | 0o755 } else { 0x8000 | 0o644 },
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
            block_buf[dotdot_ptr + 4..dotdot_ptr + 6].copy_from_slice(&dotdot_rec_len.to_le_bytes());
            block_buf[dotdot_ptr + 6] = 2;
            block_buf[dotdot_ptr + 7] = 2;
            block_buf[dotdot_ptr + 8] = b'.';
            block_buf[dotdot_ptr + 9] = b'.';
            
            write_blocks(&*self.fs.device, block as u64, &block_buf, self.fs.block_size).ok()?;
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

    fn unlink(&self, name: &str) -> Result<(), i32> {
        let child_ino = self.remove_directory_entry(name).map_err(|_| -2)?; // ENOENT
        self.fs.decrement_links_count(child_ino, false).map_err(|_| -5)?; // EIO
        Ok(())
    }

    fn mkdir(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        self.create(name, FileType::Directory)
    }

    fn rmdir(&self, name: &str) -> Result<(), i32> {
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
        
        self.fs.decrement_links_count(child_ino, true).map_err(|_| -5)?;
        self.fs.decrement_links_count(child_ino, true).map_err(|_| -5)?;
        
        Ok(())
    }

    fn readdir(&self) -> Vec<DirEntry> {
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

    fn truncate(&self, size: u64) -> Result<(), i32> {
        if size != 0 {
            return Err(-22); // EINVAL (only truncate to 0 is supported in this phase)
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
            read_blocks(&*self.fs.device, sib as u64, &mut ind_buf, self.fs.block_size).map_err(|_| -5)?;
            let refs_per_block = self.fs.block_size / 4;
            for j in 0..refs_per_block {
                let ptr_offset = (j * 4) as usize;
                let phys_block = u32::from_le_bytes([
                    ind_buf[ptr_offset], ind_buf[ptr_offset + 1], ind_buf[ptr_offset + 2], ind_buf[ptr_offset + 3]
                ]);
                if phys_block != 0 {
                    self.fs.deallocate_block(phys_block).map_err(|_| -5)?;
                }
            }
            self.fs.deallocate_block(sib).map_err(|_| -5)?;
            raw.i_block[12] = 0;
        }
        
        raw.i_size = 0;
        raw.i_blocks = 0;
        vfs.size = 0;
        vfs.blocks = 0;
        
        self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?;
        Ok(())
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
