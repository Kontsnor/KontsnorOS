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

//! RAM Disk block device driver for KontsnorOS.
//!
//! Provides a virtual block device pre-populated in memory with a minimal
//! valid ext2 filesystem layout, useful for self-contained testing.

use crate::drivers::traits::{BlockDevice, DriverError, DriverInfo};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

/// A memory-backed virtual block device.
pub struct RamDisk {
    data: Mutex<Vec<u8>>,
    info: DriverInfo,
}

impl BlockDevice for RamDisk {
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let block_size = 512; // Sector size is 512 bytes
        let offset = (block * block_size) as usize;
        let data = self.data.lock();
        if offset + buf.len() > data.len() {
            return Err(DriverError::InvalidParam);
        }
        buf.copy_from_slice(&data[offset..offset + buf.len()]);
        Ok(())
    }

    fn write_block(&self, block: u64, data: &[u8]) -> Result<(), DriverError> {
        let block_size = 512;
        let offset = (block * block_size) as usize;
        let mut disk_data = self.data.lock();
        if offset + data.len() > disk_data.len() {
            return Err(DriverError::InvalidParam);
        }
        disk_data[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }

    fn block_size(&self) -> u64 {
        512
    }

    fn block_count(&self) -> u64 {
        let data = self.data.lock();
        (data.len() / 512) as u64
    }

    fn info(&self) -> DriverInfo {
        self.info.clone()
    }
}

/// Create a new 128KB RAM disk and format it dynamically with a valid minimal ext2 filesystem.
///
/// Layout:
/// - Block Size: 1024 bytes
/// - Block 0: Boot block (unused)
/// - Block 1: Superblock (offset 1024)
/// - Block 2: Block Group Descriptor (offset 2048)
/// - Block 3: Block allocation bitmap
/// - Block 4: Inode allocation bitmap
/// - Block 5: Inode Table (contains root inode 2, hello.txt inode 12, bin dir inode 13, sh file inode 14, hello file inode 15)
/// - Block 9: Root Directory Data Block (contains '.' -> 2, '..' -> 2, 'hello.txt' -> 12, 'bin' -> 13)
/// - Block 10: hello.txt File Data Block (contains "Hello from the ext2 disk on KontsnorOS!")
/// - Block 11: /bin Directory Data Block (contains '.' -> 13, '..' -> 2, 'sh' -> 14, 'hello' -> 15)
/// - Block 12..23: /bin/sh ELF data blocks
/// - Block 24..33: /bin/hello ELF data blocks
pub fn create_ext2_ramdisk() -> Arc<dyn BlockDevice> {
    let mut data = Vec::new();
    data.resize(128 * 1024, 0); // 128 KiB disk

    // 1. Populate Superblock (Block 1, offset 1024)
    let sb_offset = 1024;
    // s_inodes_count = 32
    data[sb_offset..sb_offset + 4].copy_from_slice(&32u32.to_le_bytes());
    // s_blocks_count = 128
    data[sb_offset + 4..sb_offset + 8].copy_from_slice(&128u32.to_le_bytes());
    // s_free_blocks_count = 78 (we use Blocks 0-11 for structures, Blocks 12-23 for sh, Blocks 24-33 for hello, and 34-49 for net_test)
    data[sb_offset + 12..sb_offset + 16].copy_from_slice(&78u32.to_le_bytes());
    // s_free_inodes_count = 26 (we used 2, 12, 13, 14, 15, 16)
    data[sb_offset + 16..sb_offset + 20].copy_from_slice(&26u32.to_le_bytes());
    // s_first_data_block = 1
    data[sb_offset + 20..sb_offset + 24].copy_from_slice(&1u32.to_le_bytes());
    // s_log_block_size = 0 (1024 bytes logical blocks)
    data[sb_offset + 24..sb_offset + 28].copy_from_slice(&0u32.to_le_bytes());
    // s_blocks_per_group = 8192
    data[sb_offset + 32..sb_offset + 36].copy_from_slice(&8192u32.to_le_bytes());
    // s_inodes_per_group = 32
    data[sb_offset + 40..sb_offset + 44].copy_from_slice(&32u32.to_le_bytes());
    // s_magic = 0xEF53
    data[sb_offset + 56..sb_offset + 58].copy_from_slice(&0xEF53u16.to_le_bytes());
    // s_state = 1 (valid)
    data[sb_offset + 58..sb_offset + 60].copy_from_slice(&1u16.to_le_bytes());
    // s_rev_level = 0
    data[sb_offset + 76..sb_offset + 80].copy_from_slice(&0u32.to_le_bytes());
    // s_first_ino = 11 (first non-reserved inode)
    data[sb_offset + 84..sb_offset + 88].copy_from_slice(&11u32.to_le_bytes());
    // s_inode_size = 128
    data[sb_offset + 88..sb_offset + 90].copy_from_slice(&128u16.to_le_bytes());

    // 2. Populate Group Descriptor Table (Block 2, offset 2048)
    let gd_offset = 2048;
    // bg_block_bitmap = Block 3
    data[gd_offset..gd_offset + 4].copy_from_slice(&3u32.to_le_bytes());
    // bg_inode_bitmap = Block 4
    data[gd_offset + 4..gd_offset + 8].copy_from_slice(&4u32.to_le_bytes());
    // bg_inode_table = Block 5
    data[gd_offset + 8..gd_offset + 12].copy_from_slice(&5u32.to_le_bytes());
    // bg_free_blocks_count = 78
    data[gd_offset + 12..gd_offset + 14].copy_from_slice(&78u16.to_le_bytes());
    // bg_free_inodes_count = 26
    data[gd_offset + 14..gd_offset + 16].copy_from_slice(&26u16.to_le_bytes());
    // bg_used_dirs_count = 2 (Root directory + /bin directory)
    data[gd_offset + 16..gd_offset + 18].copy_from_slice(&2u16.to_le_bytes());

    // 3. Populate Inode Table (Block 5, offset 5 * 1024 = 5120)
    let it_offset = 5120;

    // Root Directory is Inode 2 (offset inside table = 1 * 128 = 128 bytes)
    let ino2_offset = it_offset + 128;
    // i_mode = 0x41ED (directory, rwxr-xr-x)
    data[ino2_offset..ino2_offset + 2].copy_from_slice(&0x41EDu16.to_le_bytes());
    // i_size = 1024
    data[ino2_offset + 4..ino2_offset + 8].copy_from_slice(&1024u32.to_le_bytes());
    // i_links_count = 3 (includes self, parent, and /bin/.. pointing to root)
    data[ino2_offset + 26..ino2_offset + 28].copy_from_slice(&3u16.to_le_bytes());
    // i_blocks = 2 (512-byte blocks = 1024 bytes)
    data[ino2_offset + 28..ino2_offset + 32].copy_from_slice(&2u32.to_le_bytes());
    // i_block[0] = 9 (Root directory blocks are stored in block 9)
    data[ino2_offset + 40..ino2_offset + 44].copy_from_slice(&9u32.to_le_bytes());

    // hello.txt is Inode 12 (offset inside table = 11 * 128 = 1408 bytes)
    let ino12_offset = it_offset + 1408;
    // i_mode = 0x81A4 (regular file, rw-r--r--)
    data[ino12_offset..ino12_offset + 2].copy_from_slice(&0x81A4u16.to_le_bytes());
    // i_size = 38 bytes
    data[ino12_offset + 4..ino12_offset + 8].copy_from_slice(&38u32.to_le_bytes());
    // i_links_count = 1
    data[ino12_offset + 26..ino12_offset + 28].copy_from_slice(&1u16.to_le_bytes());
    // i_blocks = 2 (512-byte blocks = 1024 bytes)
    data[ino12_offset + 28..ino12_offset + 32].copy_from_slice(&2u32.to_le_bytes());
    // i_block[0] = 10 (File content data block is stored in block 10)
    data[ino12_offset + 40..ino12_offset + 44].copy_from_slice(&10u32.to_le_bytes());

    // /bin directory is Inode 13 (offset inside table = 12 * 128 = 1536 bytes)
    let ino13_offset = it_offset + 1536;
    // i_mode = 0x41ED (directory, rwxr-xr-x)
    data[ino13_offset..ino13_offset + 2].copy_from_slice(&0x41EDu16.to_le_bytes());
    // i_size = 1024
    data[ino13_offset + 4..ino13_offset + 8].copy_from_slice(&1024u32.to_le_bytes());
    // i_links_count = 2 (includes self and parent)
    data[ino13_offset + 26..ino13_offset + 28].copy_from_slice(&2u16.to_le_bytes());
    // i_blocks = 2 (512-byte blocks = 1024 bytes)
    data[ino13_offset + 28..ino13_offset + 32].copy_from_slice(&2u32.to_le_bytes());
    // i_block[0] = 11 (/bin directory blocks are stored in block 11)
    data[ino13_offset + 40..ino13_offset + 44].copy_from_slice(&11u32.to_le_bytes());

    // /bin/sh is Inode 14 (offset inside table = 13 * 128 = 1664 bytes)
    let ino14_offset = it_offset + 1664;
    let shell_elf_len = crate::process::shell_elf::SHELL_ELF.len() as u32;
    // i_mode = 0x81A4 (regular file, rw-r--r--)
    data[ino14_offset..ino14_offset + 2].copy_from_slice(&0x81A4u16.to_le_bytes());
    // i_size = shell_elf_len
    data[ino14_offset + 4..ino14_offset + 8].copy_from_slice(&shell_elf_len.to_le_bytes());
    // i_links_count = 1
    data[ino14_offset + 26..ino14_offset + 28].copy_from_slice(&1u16.to_le_bytes());
    // i_blocks = number of 512-byte sectors
    let shell_sectors = (shell_elf_len + 511) / 512;
    data[ino14_offset + 28..ino14_offset + 32].copy_from_slice(&shell_sectors.to_le_bytes());
    // i_block[0..11] = Blocks 12 to 23
    for i in 0..12 {
        let block_num = (12 + i) as u32;
        let p_offset = ino14_offset + 40 + i * 4;
        data[p_offset..p_offset + 4].copy_from_slice(&block_num.to_le_bytes());
    }

    // /bin/hello is Inode 15 (offset inside table = 14 * 128 = 1792 bytes)
    let ino15_offset = it_offset + 1792;
    let hello_elf_bytes = crate::process::hello_elf::HELLO_ELF;
    let hello_elf_len = hello_elf_bytes.len() as u32;
    // i_mode = 0x81A4 (regular file, rw-r--r--)
    data[ino15_offset..ino15_offset + 2].copy_from_slice(&0x81A4u16.to_le_bytes());
    // i_size = hello_elf_len
    data[ino15_offset + 4..ino15_offset + 8].copy_from_slice(&hello_elf_len.to_le_bytes());
    // i_links_count = 1
    data[ino15_offset + 26..ino15_offset + 28].copy_from_slice(&1u16.to_le_bytes());
    // i_blocks = number of 512-byte sectors
    let hello_sectors = (hello_elf_len + 511) / 512;
    data[ino15_offset + 28..ino15_offset + 32].copy_from_slice(&hello_sectors.to_le_bytes());
    // i_block[0..9] = Blocks 24 to 33
    let hello_blocks_needed = (hello_elf_len + 1023) / 1024;
    for i in 0..hello_blocks_needed {
        let block_num = (24 + i) as u32;
        let p_offset = ino15_offset + 40 + (i as usize) * 4;
        data[p_offset..p_offset + 4].copy_from_slice(&block_num.to_le_bytes());
    }

    // /bin/net_test is Inode 16 (offset inside table = 15 * 128 = 1920 bytes)
    let ino16_offset = it_offset + 1920;
    let net_test_bytes = crate::process::net_test_elf::NET_TEST_ELF;
    let net_test_len = net_test_bytes.len() as u32;
    // i_mode = 0x81A4 (regular file, rw-r--r--)
    data[ino16_offset..ino16_offset + 2].copy_from_slice(&0x81A4u16.to_le_bytes());
    // i_size = net_test_len
    data[ino16_offset + 4..ino16_offset + 8].copy_from_slice(&net_test_len.to_le_bytes());
    // i_links_count = 1
    data[ino16_offset + 26..ino16_offset + 28].copy_from_slice(&1u16.to_le_bytes());
    // i_blocks = number of 512-byte sectors (including data blocks + 1 indirect block = 15*2 + 2 = 32 sectors)
    let net_test_sectors = ((net_test_len + 511) / 512) + 2;
    data[ino16_offset + 28..ino16_offset + 32].copy_from_slice(&net_test_sectors.to_le_bytes());
    // Map first 12 logical blocks directly to blocks 34..45
    for i in 0..12 {
        let block_num = (34 + i) as u32;
        let p_offset = ino16_offset + 40 + i * 4;
        data[p_offset..p_offset + 4].copy_from_slice(&block_num.to_le_bytes());
    }
    // Map i_block[12] (singly-indirect pointer) to block 50
    let sib_block = 50u32;
    data[ino16_offset + 40 + 12 * 4..ino16_offset + 40 + 12 * 4 + 4]
        .copy_from_slice(&sib_block.to_le_bytes());

    // 4. Populate Root Directory Entries (Block 9, offset 9 * 1024 = 9216)
    let dir_offset = 9216;

    // Entry 1: "." -> Inode 2
    data[dir_offset..dir_offset + 4].copy_from_slice(&2u32.to_le_bytes()); // Inode 2
    data[dir_offset + 4..dir_offset + 6].copy_from_slice(&12u16.to_le_bytes()); // rec_len
    data[dir_offset + 6] = 1; // name_len
    data[dir_offset + 7] = 2; // file_type (directory)
    data[dir_offset + 8] = b'.';

    // Entry 2: ".." -> Inode 2
    let ent2_offset = dir_offset + 12;
    data[ent2_offset..ent2_offset + 4].copy_from_slice(&2u32.to_le_bytes()); // Inode 2
    data[ent2_offset + 4..ent2_offset + 6].copy_from_slice(&12u16.to_le_bytes()); // rec_len
    data[ent2_offset + 6] = 2; // name_len
    data[ent2_offset + 7] = 2; // file_type (directory)
    data[ent2_offset + 8] = b'.';
    data[ent2_offset + 9] = b'.';

    // Entry 3: "hello.txt" -> Inode 12
    let ent3_offset = dir_offset + 24;
    data[ent3_offset..ent3_offset + 4].copy_from_slice(&12u32.to_le_bytes()); // Inode 12
    data[ent3_offset + 4..ent3_offset + 6].copy_from_slice(&20u16.to_le_bytes()); // rec_len (aligned to 4)
    data[ent3_offset + 6] = 9; // name_len
    data[ent3_offset + 7] = 1; // file_type (regular file)
    data[ent3_offset + 8..ent3_offset + 17].copy_from_slice(b"hello.txt");

    // Entry 4: "bin" -> Inode 13
    let ent4_offset = dir_offset + 44;
    data[ent4_offset..ent4_offset + 4].copy_from_slice(&13u32.to_le_bytes()); // Inode 13
    data[ent4_offset + 4..ent4_offset + 6].copy_from_slice(&980u16.to_le_bytes()); // rec_len (rest of 1024 block)
    data[ent4_offset + 6] = 3; // name_len
    data[ent4_offset + 7] = 2; // file_type (directory)
    data[ent4_offset + 8..ent4_offset + 11].copy_from_slice(b"bin");

    // 5. Populate hello.txt File Data (Block 10, offset 10 * 1024 = 10240)
    let file_data_offset = 10240;
    let file_content = b"Hello from the ext2 disk on KontsnorOS!";
    data[file_data_offset..file_data_offset + file_content.len()].copy_from_slice(file_content);

    // 6. Populate /bin Directory Entries (Block 11, offset 11 * 1024 = 11264)
    let bin_dir_offset = 11264;

    // Entry 1: "." -> Inode 13
    data[bin_dir_offset..bin_dir_offset + 4].copy_from_slice(&13u32.to_le_bytes()); // Inode 13
    data[bin_dir_offset + 4..bin_dir_offset + 6].copy_from_slice(&12u16.to_le_bytes()); // rec_len
    data[bin_dir_offset + 6] = 1; // name_len
    data[bin_dir_offset + 7] = 2; // file_type (directory)
    data[bin_dir_offset + 8] = b'.';

    // Entry 2: ".." -> Inode 2
    let bin_ent2_offset = bin_dir_offset + 12;
    data[bin_ent2_offset..bin_ent2_offset + 4].copy_from_slice(&2u32.to_le_bytes()); // Inode 2
    data[bin_ent2_offset + 4..bin_ent2_offset + 6].copy_from_slice(&12u16.to_le_bytes()); // rec_len
    data[bin_ent2_offset + 6] = 2; // name_len
    data[bin_ent2_offset + 7] = 2; // file_type (directory)
    data[bin_ent2_offset + 8] = b'.';
    data[bin_ent2_offset + 9] = b'.';

    // Entry 3: "sh" -> Inode 14
    let bin_ent3_offset = bin_dir_offset + 24;
    data[bin_ent3_offset..bin_ent3_offset + 4].copy_from_slice(&14u32.to_le_bytes()); // Inode 14
    data[bin_ent3_offset + 4..bin_ent3_offset + 6].copy_from_slice(&12u16.to_le_bytes()); // rec_len
    data[bin_ent3_offset + 6] = 2; // name_len
    data[bin_ent3_offset + 7] = 1; // file_type (regular file)
    data[bin_ent3_offset + 8] = b's';
    data[bin_ent3_offset + 9] = b'h';

    // Entry 4: "hello" -> Inode 15
    let bin_ent4_offset = bin_dir_offset + 36;
    data[bin_ent4_offset..bin_ent4_offset + 4].copy_from_slice(&15u32.to_le_bytes()); // Inode 15
    data[bin_ent4_offset + 4..bin_ent4_offset + 6].copy_from_slice(&16u16.to_le_bytes()); // rec_len = 16
    data[bin_ent4_offset + 6] = 5; // name_len
    data[bin_ent4_offset + 7] = 1; // file_type (regular file)
    data[bin_ent4_offset + 8] = b'h';
    data[bin_ent4_offset + 9] = b'e';
    data[bin_ent4_offset + 10] = b'l';
    data[bin_ent4_offset + 11] = b'l';
    data[bin_ent4_offset + 12] = b'o';

    // Entry 5: "net_test" -> Inode 16
    let bin_ent5_offset = bin_dir_offset + 52;
    data[bin_ent5_offset..bin_ent5_offset + 4].copy_from_slice(&16u32.to_le_bytes()); // Inode 16
    data[bin_ent5_offset + 4..bin_ent5_offset + 6].copy_from_slice(&972u16.to_le_bytes()); // rec_len = rest of block
    data[bin_ent5_offset + 6] = 8; // name_len
    data[bin_ent5_offset + 7] = 1; // file_type (regular file)
    data[bin_ent5_offset + 8] = b'n';
    data[bin_ent5_offset + 9] = b'e';
    data[bin_ent5_offset + 10] = b't';
    data[bin_ent5_offset + 11] = b'_';
    data[bin_ent5_offset + 12] = b't';
    data[bin_ent5_offset + 13] = b'e';
    data[bin_ent5_offset + 14] = b's';
    data[bin_ent5_offset + 15] = b't';

    // 7. Populate /bin/sh ELF data (Blocks 12 to 23, offset 12 * 1024 = 12288)
    let shell_elf_bytes = crate::process::shell_elf::SHELL_ELF;
    data[12288..12288 + shell_elf_bytes.len()].copy_from_slice(shell_elf_bytes);

    // 8. Populate /bin/hello ELF data (Blocks 24 to 33, offset 24 * 1024 = 24576)
    let hello_elf_bytes = crate::process::hello_elf::HELLO_ELF;
    data[24576..24576 + hello_elf_bytes.len()].copy_from_slice(hello_elf_bytes);

    // 9. Populate /bin/net_test ELF data (Blocks 34 to 48, offset 34 * 1024 = 34816)
    let net_test_elf_bytes = crate::process::net_test_elf::NET_TEST_ELF;
    data[34816..34816 + net_test_elf_bytes.len()].copy_from_slice(net_test_elf_bytes);

    // 10. Populate singly-indirect block for net_test (Block 50, offset 50 * 1024 = 51200)
    let sib_offset = 51200;
    // Map logical block 12 -> block 46, logical block 13 -> block 47, logical block 14 -> block 48
    data[sib_offset..sib_offset + 4].copy_from_slice(&46u32.to_le_bytes());
    data[sib_offset + 4..sib_offset + 8].copy_from_slice(&47u32.to_le_bytes());
    data[sib_offset + 8..sib_offset + 12].copy_from_slice(&48u32.to_le_bytes());

    let info = DriverInfo {
        name: alloc::string::String::from("ramdisk"),
        version: alloc::string::String::from("1.0.0"),
        author: alloc::string::String::from("KontsnorOS Team"),
        license: alloc::string::String::from("GPL-3.0-only"),
        description: alloc::string::String::from(
            "In-memory ramdisk pre-populated with ext2 file system",
        ),
    };

    Arc::new(RamDisk {
        data: Mutex::new(data),
        info,
    })
}
