//! ext2 writable filesystem driver for KontsnorOS.

use crate::drivers::traits::BlockDevice;
use crate::fs::inode::{DirEntry, FilePermissions, FileType, Inode, InodeOps};
use crate::fs::vfs::{FileSystem, FsStats};
use crate::kprintln;
use ::alloc::string::String;
use ::alloc::sync::Arc;
use ::alloc::vec::Vec;
use spin::Mutex;

pub mod alloc;
pub mod dir;
pub mod file;
pub mod types;

pub use types::{Ext2RawInode, GroupDescriptor, Superblock};

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

/// ext2 FileSystem implementation.
pub struct Ext2FileSystem {
    pub(crate) device: Arc<dyn BlockDevice>,
    pub(crate) block_size: u32,
    pub(crate) inodes_per_block: u32,
    pub(crate) inodes_per_group: u32,
    pub(crate) inode_size: u16,
    pub(crate) superblock: Mutex<Superblock>,
    pub(crate) group_descriptors: Mutex<Vec<GroupDescriptor>>,
    pub(crate) root_node: Mutex<Option<Arc<dyn InodeOps>>>,
}

impl Ext2FileSystem {
    /// Mount an ext2 volume on a block device.
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
            return Err("Invalid ext2 superblock magic");
        }

        // Validate metadata parameters
        if s_log_block_size > 10 || s_inode_size == 0 || s_inodes_per_group == 0 {
            return Err("Malformed ext2 superblock fields");
        }

        if sb.s_inodes_count == 0 || sb.s_blocks_count == 0 {
            return Err("Malformed ext2 superblock: inodes or blocks count is zero");
        }

        if sb.s_inodes_count > sb.s_inodes_per_group {
            return Err("Multi-group ext2 filesystems are not supported");
        }

        let block_size = 1024 << s_log_block_size;
        let inode_size = s_inode_size;
        let inodes_per_group = s_inodes_per_group;
        let inodes_per_block = block_size / inode_size as u32;

        kprintln!(
            "[ext2] Volume detected. s_magic: {:#x}, Block Size: {}, Inode Size: {}",
            s_magic,
            block_size,
            inode_size
        );

        // Read Group Descriptor Table
        let gdt_block = if block_size == 1024 { 2 } else { 1 };
        let mut gdt_buf = ::alloc::vec![0u8; block_size as usize];
        read_blocks(&*device, gdt_block, &mut gdt_buf, block_size)?;

        // SAFETY: gdt_buf contains at least 32 bytes read from the block device, which is valid for GroupDescriptor.
        let mut gd =
            unsafe { core::ptr::read_unaligned(gdt_buf.as_ptr() as *const GroupDescriptor) };

        // Validate GDT offsets are within filesystem bounds
        if gd.bg_block_bitmap >= sb.s_blocks_count
            || gd.bg_inode_bitmap >= sb.s_blocks_count
            || gd.bg_inode_table >= sb.s_blocks_count
        {
            return Err("Metadata blocks exceed filesystem blocks count");
        }

        // Self-healing bitmaps check on mount
        let mut block_bitmap = ::alloc::vec![0u8; block_size as usize];
        read_blocks(
            &*device,
            gd.bg_block_bitmap as u64,
            &mut block_bitmap,
            block_size,
        )?;
        let mut inode_bitmap = ::alloc::vec![0u8; block_size as usize];
        read_blocks(
            &*device,
            gd.bg_inode_bitmap as u64,
            &mut inode_bitmap,
            block_size,
        )?;

        let mut block_bitmap_changed = false;
        if block_bitmap.iter().all(|&x| x == 0) {
            kprintln!("[ext2] Block bitmap is all zeros, healing...");
            block_bitmap[0] = 0xFF;
            block_bitmap[1] = 0xFF;
            block_bitmap[2] = 0xFF;
            block_bitmap[3] = 0xFF;
            block_bitmap[4] = 0x03; // 34 blocks total
            write_blocks(
                &*device,
                gd.bg_block_bitmap as u64,
                &block_bitmap,
                block_size,
            )?;
            block_bitmap_changed = true;
        }

        let mut inode_bitmap_changed = false;
        if inode_bitmap.iter().all(|&x| x == 0) {
            kprintln!("[ext2] Inode bitmap is all zeros, healing...");
            inode_bitmap[0] = 0xFF;
            inode_bitmap[1] = 0x7F; // 15 inodes total
            write_blocks(
                &*device,
                gd.bg_inode_bitmap as u64,
                &inode_bitmap,
                block_size,
            )?;
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
            // SAFETY: sb is stack-allocated, and sb_buf_write is 1024 bytes, copy is within bounds.
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
            let gd_size = core::mem::size_of::<GroupDescriptor>();
            let src_ptr = &gd as *const GroupDescriptor as *const u8;
            // SAFETY: gd is stack-allocated, and gdt_buf has size equal to block_size which is at least gd_size.
            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, gdt_buf.as_mut_ptr(), gd_size);
            }
            write_blocks(&*device, gdt_block, &gdt_buf, block_size)?;
        }

        // --- Consistency Check and Self-Healing (FSCK) ---
        let mut calc_block_bitmap = ::alloc::vec![0u8; block_size as usize];
        let mut calc_inode_bitmap = ::alloc::vec![0u8; block_size as usize];

        // 1. Mark reserved metadata blocks as allocated
        let it_blocks = match (s_inodes_per_group as u64).checked_mul(s_inode_size as u64) {
            Some(prod) => ((prod + block_size as u64 - 1) / block_size as u64) as u32,
            None => return Err("Overflow in metadata size calculation"),
        };
        let reserved_blocks_count = match (gd.bg_inode_table as u32).checked_add(it_blocks) {
            Some(count) => count,
            None => return Err("Overflow in reserved blocks count calculation"),
        };
        if reserved_blocks_count > sb.s_blocks_count {
            return Err("Reserved metadata block count exceeds total block count");
        }
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

        // 3. Scan all inodes from 2 to sb.s_inodes_count
        let table_block = gd.bg_inode_table as u64;
        let mut block_cache_idx = 0u64;
        let mut block_cache_buf = ::alloc::vec![0u8; block_size as usize];

        for ino in 2..=sb.s_inodes_count {
            let index = (ino - 1) % s_inodes_per_group;
            let inode_offset_in_table = (index * s_inode_size as u32) as u64;
            let logical_block = table_block + (inode_offset_in_table / block_size as u64);
            let offset_in_block = (inode_offset_in_table % block_size as u64) as usize;

            if block_cache_idx != logical_block {
                read_blocks(&*device, logical_block, &mut block_cache_buf, block_size)?;
                block_cache_idx = logical_block;
            }

            // SAFETY: block_cache_buf has size block_size, offset_in_block is within bounds, and raw inode layout matches Ext2RawInode structure.
            let raw_inode = unsafe {
                core::ptr::read_unaligned(
                    block_cache_buf[offset_in_block..].as_ptr() as *const Ext2RawInode
                )
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
                    let mut ind_buf = ::alloc::vec![0u8; block_size as usize];
                    if read_blocks(&*device, sib as u64, &mut ind_buf, block_size).is_ok() {
                        let refs_per_block = block_size / 4;
                        for r in 0..refs_per_block {
                            let ptr_offset = (r * 4) as usize;
                            let phys_block = u32::from_le_bytes([
                                ind_buf[ptr_offset],
                                ind_buf[ptr_offset + 1],
                                ind_buf[ptr_offset + 2],
                                ind_buf[ptr_offset + 3],
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

            write_blocks(
                &*device,
                gd.bg_block_bitmap as u64,
                &calc_block_bitmap,
                block_size,
            )?;
            write_blocks(
                &*device,
                gd.bg_inode_bitmap as u64,
                &calc_inode_bitmap,
                block_size,
            )?;

            let free_b = count_free_bits(&calc_block_bitmap, sb.s_blocks_count);
            let free_i = count_free_bits(&calc_inode_bitmap, sb.s_inodes_count);
            sb.s_free_blocks_count = free_b;
            sb.s_free_inodes_count = free_i;
            gd.bg_free_blocks_count = free_b as u16;
            gd.bg_free_inodes_count = free_i as u16;

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
            let gd_size = core::mem::size_of::<GroupDescriptor>();
            let src_ptr = &gd as *const GroupDescriptor as *const u8;
            // SAFETY: gd is stack-allocated, and gdt_buf has size equal to block_size which is at least gd_size.
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
            gds.get(group as usize)
                .copied()
                .ok_or("Group descriptor index out of bounds")?
        };

        let table_block = gd.bg_inode_table as u64;
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

        // SAFETY: block_buf is allocated with size block_size, offset_in_block is within bounds, and layout matches Ext2RawInode.
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
            0xA000 => FileType::Symlink,
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

        let mut gdt_buf = ::alloc::vec![0u8; block_size as usize];
        let gd_size = core::mem::size_of::<GroupDescriptor>();
        for (i, gd) in gds.iter().enumerate() {
            let offset = i * gd_size;
            if offset + gd_size <= block_size as usize {
                let src_ptr = gd as *const GroupDescriptor as *const u8;
                // SAFETY: gd is valid for reading, gdt_buf is block_size bytes, copy is within bounds.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src_ptr,
                        gdt_buf.as_mut_ptr().add(offset),
                        gd_size,
                    );
                }
            }
        }
        write_blocks(&*self.device, gdt_block, &gdt_buf, block_size)?;
        Ok(())
    }
}

/// ext2 Inode wrapper implementing InodeOps.
pub struct Ext2Inode {
    pub(crate) fs: Arc<Ext2FileSystem>,
    pub(crate) ino: u32,
    pub(crate) raw: Mutex<Ext2RawInode>,
    pub(crate) vfs_inode: Mutex<Inode>,
}

impl InodeOps for Ext2Inode {
    fn inode(&self) -> &Inode {
        // SAFETY: The reference to Inode is protected by Mutex but the caller requires a lifetime matched reference.
        // We cast the reference to a raw pointer to satisfy the signature.
        unsafe { &*(&*self.vfs_inode.lock() as *const Inode) }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        self.read_file(offset, buf)
    }

    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, i32> {
        self.write_file(offset, buf)
    }

    fn create(&self, name: &str, file_type: FileType) -> Option<Arc<dyn InodeOps>> {
        self.create_dir_entry(name, file_type)
    }

    fn unlink(&self, name: &str) -> Result<(), i32> {
        self.unlink_dir_entry(name)
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

impl FileSystem for Ext2FileSystem {
    fn root(&self) -> Option<Arc<dyn InodeOps>> {
        self.root_node.lock().clone()
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
