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

//! ext physical structures and disk layout types.

// Superblock Incompatible Feature Flags
pub const INCOMPAT_FILETYPE: u32 = 0x0002;
pub const INCOMPAT_RECOVER: u32 = 0x0004;
pub const INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
pub const INCOMPAT_EXTENTS: u32 = 0x0040;
pub const INCOMPAT_64BIT: u32 = 0x0080;
pub const INCOMPAT_MMP: u32 = 0x0100;
pub const INCOMPAT_FLEX_BG: u32 = 0x0200;
pub const INCOMPAT_EA_INODE: u32 = 0x0400;
pub const INCOMPAT_DIRDATA: u32 = 0x1000;
pub const INCOMPAT_CSUM_SEED: u32 = 0x2000;
pub const INCOMPAT_LARGEDIR: u32 = 0x4000;
pub const INCOMPAT_INLINE_DATA: u32 = 0x8000;

// Superblock Read-Only Compatible Feature Flags
pub const RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
pub const RO_COMPAT_LARGE_FILE: u32 = 0x0002;
pub const RO_COMPAT_BTREE_DIR: u32 = 0x0004;
pub const RO_COMPAT_HUGE_FILE: u32 = 0x0008;
pub const RO_COMPAT_GDT_CSUM: u32 = 0x0010;
pub const RO_COMPAT_DIR_NLINK: u32 = 0x0020;
pub const RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;
pub const RO_COMPAT_QUOTA: u32 = 0x0100;
pub const RO_COMPAT_BIGALLOC: u32 = 0x0200;
pub const RO_COMPAT_METADATA_CSUM: u32 = 0x0400;
pub const RO_COMPAT_READONLY: u32 = 0x1000;
pub const RO_COMPAT_PROJECT: u32 = 0x2000;

// Inode Flags
pub const EXT4_SECRM_FL: u32 = 0x0000_0001;
pub const EXT4_UNRM_FL: u32 = 0x0000_0002;
pub const EXT4_COMPR_FL: u32 = 0x0000_0004;
pub const EXT4_SYNC_FL: u32 = 0x0000_0008;
pub const EXT4_IMMUTABLE_FL: u32 = 0x0000_0010;
pub const EXT4_APPEND_FL: u32 = 0x0000_0020;
pub const EXT4_NODUMP_FL: u32 = 0x0000_0040;
pub const EXT4_NOATIME_FL: u32 = 0x0000_0080;
pub const EXT4_INDEX_FL: u32 = 0x0000_1000;
pub const EXT4_JOURNAL_DATA_FL: u32 = 0x0000_4000;
pub const EXT4_NOTAIL_FL: u32 = 0x0000_8000;
pub const EXT4_DIRSYNC_FL: u32 = 0x0001_0000;
pub const EXT4_TOPDIR_FL: u32 = 0x0002_0000;
pub const EXT4_HUGE_FILE_FL: u32 = 0x0004_0000;
pub const EXT4_EXTENTS_FL: u32 = 0x0008_0000;
pub const EXT4_EA_INODE_FL: u32 = 0x0020_0000;
pub const EXT4_EOFBLOCKS_FL: u32 = 0x0040_0000;
pub const EXT4_INLINE_DATA_FL: u32 = 0x1000_0000;

/// ext superblock structure (located at offset 1024).
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
    pub s_block_group_nr: u16,
    pub s_feature_compat: u32,
    pub s_feature_incompat: u32,
    pub s_feature_ro_compat: u32,
    pub s_uuid: [u8; 16],
    pub s_volume_name: [u8; 16],
    pub s_last_mounted: [u8; 64],
    pub s_algorithm_usage_bitmap: u32,
    pub s_prealloc_blocks: u8,
    pub s_prealloc_dir_blocks: u8,
    pub s_reserved_gdb: u16,
    pub s_journal_uuid: [u8; 16],
    pub s_journal_inum: u32,
    pub s_journal_dev: u32,
    pub s_last_orphan: u32,
    pub s_hash_seed: [u32; 4],
    pub s_def_hash_version: u8,
    pub s_jnl_backup_type: u8,
    pub s_desc_size: u16,
    pub s_default_mount_opts: u32,
    pub s_first_meta_bg: u32,
    pub s_mkfs_time: u32,
    pub s_jnl_blocks: [u32; 17],
    pub s_blocks_count_hi: u32,
    pub s_r_blocks_count_hi: u32,
    pub s_free_blocks_count_hi: u32,
    pub s_min_extra_isize: u16,
    pub s_want_extra_isize: u16,
    pub s_flags: u32,
    pub s_raid_stride: u16,
    pub s_mmp_interval: u16,
    pub s_mmp_block: u64,
    pub s_raid_stripe_width: u32,
    pub s_log_groups_per_flex: u8,
    pub s_checksum_type: u8,
    pub s_reserved_pad: u16,
    pub s_kbytes_written: u64,
    pub s_snapshot_inum: u32,
    pub s_snapshot_id: u32,
    pub s_snapshot_r_blocks_count: u64,
    pub s_snapshot_list: u32,
    pub s_error_count: u32,
    pub s_first_error_time: u32,
    pub s_first_error_ino: u32,
    pub s_first_error_block: u64,
    pub s_first_error_func: [u8; 32],
    pub s_first_error_line: u32,
    pub s_last_error_time: u32,
    pub s_last_error_ino: u32,
    pub s_last_error_line: u32,
    pub s_last_error_block: u64,
    pub s_last_error_func: [u8; 32],
    pub s_mount_opts: [u8; 64],
    pub s_usr_quota_inum: u32,
    pub s_grp_quota_inum: u32,
    pub s_overhead_clusters: u32,
    pub s_backup_bgs: [u32; 2],
    pub s_encrypt_algos: [u8; 4],
    pub s_encrypt_pw_salt: [u8; 16],
    pub s_lvf_inum: u32,
    pub s_prj_quota_inum: u32,
    pub s_checksum_seed: u32,
    pub s_wtime_hi: u8,
    pub s_mtime_hi: u8,
    pub s_mkfs_time_hi: u8,
    pub s_lastcheck_hi: u8,
    pub s_first_error_time_hi: u8,
    pub s_last_error_time_hi: u8,
    pub s_pad: [u8; 2],
    pub s_encoding: u16,
    pub s_encoding_flags: u16,
    pub s_orphan_file_inum: u32,
    pub s_reserved: [u32; 94],
    pub s_checksum: u32,
}

impl Superblock {
    /// Helper to get the Group Descriptor size in bytes.
    pub fn desc_size(&self) -> usize {
        if (self.s_feature_incompat & INCOMPAT_64BIT) != 0 && self.s_desc_size >= 32 {
            self.s_desc_size as usize
        } else {
            32
        }
    }

    /// Total blocks count supporting 64-bit feature.
    pub fn total_blocks(&self) -> u64 {
        if (self.s_feature_incompat & INCOMPAT_64BIT) != 0 {
            ((self.s_blocks_count_hi as u64) << 32) | (self.s_blocks_count as u64)
        } else {
            self.s_blocks_count as u64
        }
    }

    /// Total free blocks count supporting 64-bit feature.
    pub fn free_blocks(&self) -> u64 {
        if (self.s_feature_incompat & INCOMPAT_64BIT) != 0 {
            ((self.s_free_blocks_count_hi as u64) << 32) | (self.s_free_blocks_count as u64)
        } else {
            self.s_free_blocks_count as u64
        }
    }

    /// Set free blocks count across 32-bit/64-bit fields.
    pub fn set_free_blocks(&mut self, free_blocks: u64) {
        self.s_free_blocks_count = free_blocks as u32;
        if (self.s_feature_incompat & INCOMPAT_64BIT) != 0 {
            self.s_free_blocks_count_hi = (free_blocks >> 32) as u32;
        }
    }
}

/// ext block group descriptor (32 bytes standard, 64 bytes for 64-bit ext4).
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
    // 64-bit extension fields (present when desc_size >= 64)
    pub bg_block_bitmap_hi: u32,
    pub bg_inode_bitmap_hi: u32,
    pub bg_inode_table_hi: u32,
    pub bg_free_blocks_count_hi: u16,
    pub bg_free_inodes_count_hi: u16,
    pub bg_used_dirs_count_hi: u16,
    pub bg_itable_unused_hi: u16,
    pub bg_exclude_bitmap_hi: u32,
    pub bg_block_bitmap_csum_hi: u16,
    pub bg_inode_bitmap_csum_hi: u16,
    pub bg_reserved_64: u32,
}

impl GroupDescriptor {
    /// Get full 64-bit block bitmap address.
    pub fn block_bitmap(&self, is_64bit: bool) -> u64 {
        if is_64bit {
            ((self.bg_block_bitmap_hi as u64) << 32) | (self.bg_block_bitmap as u64)
        } else {
            self.bg_block_bitmap as u64
        }
    }

    /// Set full 64-bit block bitmap address.
    pub fn set_block_bitmap(&mut self, is_64bit: bool, block: u64) {
        self.bg_block_bitmap = block as u32;
        if is_64bit {
            self.bg_block_bitmap_hi = (block >> 32) as u32;
        }
    }

    /// Get full 64-bit inode bitmap address.
    pub fn inode_bitmap(&self, is_64bit: bool) -> u64 {
        if is_64bit {
            ((self.bg_inode_bitmap_hi as u64) << 32) | (self.bg_inode_bitmap as u64)
        } else {
            self.bg_inode_bitmap as u64
        }
    }

    /// Set full 64-bit inode bitmap address.
    pub fn set_inode_bitmap(&mut self, is_64bit: bool, block: u64) {
        self.bg_inode_bitmap = block as u32;
        if is_64bit {
            self.bg_inode_bitmap_hi = (block >> 32) as u32;
        }
    }

    /// Get full 64-bit inode table address.
    pub fn inode_table(&self, is_64bit: bool) -> u64 {
        if is_64bit {
            ((self.bg_inode_table_hi as u64) << 32) | (self.bg_inode_table as u64)
        } else {
            self.bg_inode_table as u64
        }
    }

    /// Set full 64-bit inode table address.
    pub fn set_inode_table(&mut self, is_64bit: bool, block: u64) {
        self.bg_inode_table = block as u32;
        if is_64bit {
            self.bg_inode_table_hi = (block >> 32) as u32;
        }
    }

    /// Get total free blocks count for group.
    pub fn free_blocks_count(&self, is_64bit: bool) -> u32 {
        if is_64bit {
            ((self.bg_free_blocks_count_hi as u32) << 16) | (self.bg_free_blocks_count as u32)
        } else {
            self.bg_free_blocks_count as u32
        }
    }

    /// Set total free blocks count for group.
    pub fn set_free_blocks_count(&mut self, is_64bit: bool, count: u32) {
        self.bg_free_blocks_count = count as u16;
        if is_64bit {
            self.bg_free_blocks_count_hi = (count >> 16) as u16;
        }
    }

    /// Get total free inodes count for group.
    pub fn free_inodes_count(&self, is_64bit: bool) -> u32 {
        if is_64bit {
            ((self.bg_free_inodes_count_hi as u32) << 16) | (self.bg_free_inodes_count as u32)
        } else {
            self.bg_free_inodes_count as u32
        }
    }

    /// Set total free inodes count for group.
    pub fn set_free_inodes_count(&mut self, is_64bit: bool, count: u32) {
        self.bg_free_inodes_count = count as u16;
        if is_64bit {
            self.bg_free_inodes_count_hi = (count >> 16) as u16;
        }
    }

    /// Get used directories count for group.
    pub fn used_dirs_count(&self, is_64bit: bool) -> u32 {
        if is_64bit {
            ((self.bg_used_dirs_count_hi as u32) << 16) | (self.bg_used_dirs_count as u32)
        } else {
            self.bg_used_dirs_count as u32
        }
    }

    /// Set used directories count for group.
    pub fn set_used_dirs_count(&mut self, is_64bit: bool, count: u32) {
        self.bg_used_dirs_count = count as u16;
        if is_64bit {
            self.bg_used_dirs_count_hi = (count >> 16) as u16;
        }
    }
}

/// ext raw inode structure on disk (128 bytes basic, up to 256 bytes extended).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ExtRawInode {
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
    pub i_dir_acl: u32, // For regular files in ext4, this field holds i_size_high
    pub i_faddr: u32,
    pub i_osd2: [u8; 12],
    // Extended inode fields (when inode_size > 128)
    pub i_extra_isize: u16,
    pub i_checksum_hi: u16,
    pub i_ctime_extra: u32,
    pub i_mtime_extra: u32,
    pub i_atime_extra: u32,
    pub i_crtime: u32,
    pub i_crtime_extra: u32,
    pub i_version_hi: u32,
    pub i_projid: u32,
}

impl ExtRawInode {
    /// Return a zero-initialized ExtRawInode.
    pub fn zeroed() -> Self {
        // SAFETY: All fields of ExtRawInode are primitive numeric or array types.
        unsafe { core::mem::zeroed() }
    }

    /// Get full 64-bit file size.
    pub fn file_size(&self) -> u64 {
        let is_dir = (self.i_mode & 0xF000) == 0x4000;
        if is_dir {
            self.i_size as u64
        } else {
            ((self.i_dir_acl as u64) << 32) | (self.i_size as u64)
        }
    }

    /// Set full 64-bit file size.
    pub fn set_file_size(&mut self, size: u64) {
        let is_dir = (self.i_mode & 0xF000) == 0x4000;
        self.i_size = size as u32;
        if !is_dir {
            self.i_dir_acl = (size >> 32) as u32;
        }
    }
}

/// Ext4 extent header structure.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4ExtentHeader {
    pub eh_magic: u16,   // Must be 0xF30A
    pub eh_entries: u16, // Number of valid entries
    pub eh_max: u16,     // Maximum number of entries
    pub eh_depth: u16,   // Depth of the tree (0 for leaf nodes)
    pub eh_generation: u32,
}

/// Ext4 extent leaf entry structure.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4Extent {
    pub ee_block: u32,    // First logical block number
    pub ee_len: u16,      // Number of blocks covered by extent (high bit set = uninitialized)
    pub ee_start_hi: u16, // High 16 bits of physical block number
    pub ee_start_lo: u32, // Low 32 bits of physical block number
}

impl Ext4Extent {
    /// Physical block address starting this extent.
    pub fn start_block(&self) -> u64 {
        ((self.ee_start_hi as u64) << 32) | (self.ee_start_lo as u64)
    }

    /// Set physical block address starting this extent.
    pub fn set_start_block(&mut self, block: u64) {
        self.ee_start_lo = block as u32;
        self.ee_start_hi = (block >> 32) as u16;
    }

    /// Actual block count (masking uninitialized flag if present).
    pub fn len(&self) -> u16 {
        self.ee_len & 0x7FFF
    }

    /// Check if extent is uninitialized/preallocated.
    pub fn is_uninitialized(&self) -> bool {
        self.ee_len > 32768
    }
}

/// Ext4 extent internal index entry structure.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4ExtentIdx {
    pub ei_block: u32,   // Index covers logical blocks starting here
    pub ei_leaf_lo: u32, // Low 32 bits of physical block pointing to next level
    pub ei_leaf_hi: u16, // High 16 bits of next level block
    pub ei_unused: u16,
}

impl Ext4ExtentIdx {
    /// Physical block address pointing to child node.
    pub fn leaf_block(&self) -> u64 {
        ((self.ei_leaf_hi as u64) << 32) | (self.ei_leaf_lo as u64)
    }

    /// Set physical block address pointing to child node.
    pub fn set_leaf_block(&mut self, block: u64) {
        self.ei_leaf_lo = block as u32;
        self.ei_leaf_hi = (block >> 32) as u16;
    }
}

/// JBD2 journal block header.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct JournalHeader {
    pub h_magic: u32,     // Must be 0xC03B3998
    pub h_blocktype: u32, // 3 or 4
    pub h_sequence: u32,
}

/// JBD2 journal superblock.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct JournalSuperblock {
    pub s_header: JournalHeader,
    pub s_blocksize: u32,
    pub s_maxlen: u32,
    pub s_first: u32,
    pub s_sequence: u32,
    pub s_start: u32,
    pub s_errno: u32,
    pub s_feature_compat: u32,
    pub s_feature_incompat: u32,
    pub s_feature_ro_compat: u32,
    pub s_uuid: [u8; 16],
    pub s_nr_users: u32,
    pub s_dynsuper: u32,
    pub s_max_transaction: u32,
    pub s_max_user_data: u32,
}
