//! ext physical structures and disk layout types.

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
    pub i_dir_acl: u32,
    pub i_faddr: u32,
    pub i_osd2: [u8; 12],
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
    pub ee_len: u16,      // Number of blocks covered by extent
    pub ee_start_hi: u16, // High 16 bits of physical block number
    pub ee_start_lo: u32, // Low 32 bits of physical block number
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
