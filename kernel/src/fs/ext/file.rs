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

//! Regular file read, write, and size manipulation (truncate) operations.

use super::{read_blocks, write_blocks};
use super::{Ext4Extent, Ext4ExtentHeader, Ext4ExtentIdx};
use super::{ExtInode, ExtRawInode};
use crate::fs::inode::{FileType, InodeOps};

impl ExtInode {
    /// Invalidate the cached last-resolved extent for this inode.
    #[inline]
    pub fn invalidate_extent_cache(&self) {
        self.extent_cache.invalidate();
    }

    /// Resolve an Ext4 extent-mapped block along with the remaining contiguous blocks in the extent.
    pub fn resolve_extent_block_len(
        &self,
        i_block: &[u32; 15],
        file_block: u32,
    ) -> Result<(u32, u32), &'static str> {
        // 1. Check last-extent cache for fast hit on consecutive sequential accesses
        if let Some((phys_block, remaining)) = self.extent_cache.get(file_block) {
            return Ok((phys_block, remaining));
        }

        // 2. Fast path for depth == 0 inline extents (parsed directly from the 60-byte i_block)
        // SAFETY: i_block has 15 * 4 = 60 bytes, which is sufficient for Ext4ExtentHeader (12 bytes).
        let header =
            unsafe { core::ptr::read_unaligned(i_block.as_ptr() as *const Ext4ExtentHeader) };
        if header.eh_magic != 0xF30A {
            return Err("Invalid extent header magic");
        }

        let eh_depth = header.eh_depth;
        let eh_entries = header.eh_entries as usize;

        if eh_depth == 0 {
            // Leaf entries inline in i_block. Up to 4 entries fit in 60 bytes (12 + 4 * 12 = 60).
            let max_entries = eh_entries.min(4);
            let ext_ptr = unsafe {
                // SAFETY: Header is 12 bytes; extents follow immediately within the 60-byte i_block.
                (i_block.as_ptr() as *const u8).add(12) as *const Ext4Extent
            };
            for i in 0..max_entries {
                let ext = unsafe {
                    // SAFETY: i < max_entries <= 4, strictly within i_block bounds.
                    core::ptr::read_unaligned(ext_ptr.add(i))
                };
                let ee_len = ext.len() as u32;
                if file_block >= ext.ee_block && file_block < ext.ee_block + ee_len {
                    let phys_start = ext.start_block();
                    let phys_block = phys_start + (file_block - ext.ee_block) as u64;
                    let remaining = (ext.ee_block + ee_len) - file_block;

                    // Update last-extent cache lock-free
                    self.extent_cache.update(ext.ee_block, ee_len, phys_start);
                    return Ok((phys_block as u32, remaining));
                }
            }
            return Ok((0, 0)); // Sparse block / hole
        }

        // 3. For depth >= 1: interior index traversal using MaybeUninit buffer for child nodes
        let max_idx_entries = eh_entries.min(4);
        let idx_ptr = unsafe {
            // SAFETY: Header is 12 bytes; index entries follow immediately within the 60-byte i_block.
            (i_block.as_ptr() as *const u8).add(12) as *const Ext4ExtentIdx
        };
        let mut best_idx: Option<Ext4ExtentIdx> = None;
        for i in 0..max_idx_entries {
            let idx = unsafe {
                // SAFETY: i < max_idx_entries <= 4, strictly within i_block bounds.
                core::ptr::read_unaligned(idx_ptr.add(i))
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

        let mut child_block = match best_idx {
            Some(best) => best.leaf_block(),
            None => return Ok((0, 0)),
        };

        // Descend the tree levels using uninitialized stack buffer (avoiding zeroing 4 KiB)
        let block_size = self.fs.block_size as usize;
        assert!(block_size <= 4096);
        let mut child_buf = [core::mem::MaybeUninit::<u8>::uninit(); 4096];

        loop {
            // Read child block from disk
            // SAFETY: read_blocks writes the entire slice of block_size bytes.
            let child_slice = unsafe {
                core::slice::from_raw_parts_mut(child_buf.as_mut_ptr() as *mut u8, block_size)
            };
            read_blocks(
                &*self.fs.device,
                child_block,
                child_slice,
                self.fs.block_size,
            )?;

            let node_header = unsafe {
                // SAFETY: Child node was just read and block_size >= 1024 > size_of::<Ext4ExtentHeader>() (12).
                core::ptr::read_unaligned(child_slice.as_ptr() as *const Ext4ExtentHeader)
            };
            if node_header.eh_magic != 0xF30A {
                return Err("Invalid extent child header magic");
            }

            let depth = node_header.eh_depth;
            let entries = node_header.eh_entries as usize;

            if depth == 0 {
                // Leaf node
                let leaf_entry_size = core::mem::size_of::<Ext4Extent>();
                for i in 0..entries {
                    let offset = 12 + i * leaf_entry_size;
                    if offset + leaf_entry_size > block_size {
                        return Err("Extent entry out of bounds");
                    }
                    let ext = unsafe {
                        // SAFETY: offset + leaf_entry_size <= block_size.
                        core::ptr::read_unaligned(
                            child_slice[offset..].as_ptr() as *const Ext4Extent
                        )
                    };
                    let ee_len = ext.len() as u32;
                    if file_block >= ext.ee_block && file_block < ext.ee_block + ee_len {
                        let phys_start = ext.start_block();
                        let phys_block = phys_start + (file_block - ext.ee_block) as u64;
                        let remaining = (ext.ee_block + ee_len) - file_block;

                        self.extent_cache.update(ext.ee_block, ee_len, phys_start);
                        return Ok((phys_block as u32, remaining));
                    }
                }
                return Ok((0, 0));
            } else {
                // Index node
                let idx_entry_size = core::mem::size_of::<Ext4ExtentIdx>();
                let mut next_best: Option<Ext4ExtentIdx> = None;
                for i in 0..entries {
                    let offset = 12 + i * idx_entry_size;
                    if offset + idx_entry_size > block_size {
                        return Err("Extent index entry out of bounds");
                    }
                    let idx = unsafe {
                        // SAFETY: offset + idx_entry_size <= block_size.
                        core::ptr::read_unaligned(
                            child_slice[offset..].as_ptr() as *const Ext4ExtentIdx
                        )
                    };
                    if idx.ei_block <= file_block {
                        match next_best {
                            None => next_best = Some(idx),
                            Some(ref best) => {
                                if idx.ei_block > best.ei_block {
                                    next_best = Some(idx);
                                }
                            }
                        }
                    }
                }
                match next_best {
                    Some(best) => child_block = best.leaf_block(),
                    None => return Ok((0, 0)),
                }
            }
        }
    }

    /// Resolve an Ext4 extent-mapped block.
    pub fn resolve_extent_block(
        &self,
        i_block: &[u32; 15],
        file_block: u32,
    ) -> Result<u32, &'static str> {
        self.resolve_extent_block_len(i_block, file_block)
            .map(|(phys, _)| phys)
    }

    /// Resolve logical block number to physical disk block using a provided raw inode reference.
    pub fn resolve_block_with_raw(
        &self,
        raw: &ExtRawInode,
        file_block: u32,
    ) -> Result<u32, &'static str> {
        let i_block = raw.i_block;
        self.resolve_block_from_flags_and_block(raw.i_flags, &i_block, file_block)
    }

    /// Resolve logical block number to physical disk block without holding the raw lock across I/O.
    pub fn resolve_block(&self, file_block: u32) -> Result<u32, &'static str> {
        // Fast path: check extent cache lock-free without acquiring raw lock
        if let Some((phys_block, _)) = self.extent_cache.get(file_block) {
            crate::fs::kstats::KSTATS
                .resolve_block_calls
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            return Ok(phys_block);
        }

        let (i_flags, i_block) = {
            let raw = self.raw.lock();
            (raw.i_flags, raw.i_block)
        };
        self.resolve_block_from_flags_and_block(i_flags, &i_block, file_block)
    }

    /// Resolve logical block number to physical disk block and remaining contiguous run length.
    pub fn resolve_block_run(&self, file_block: u32) -> Result<(u32, u32), &'static str> {
        if let Some((phys_block, remaining)) = self.extent_cache.get(file_block) {
            crate::fs::kstats::KSTATS
                .resolve_block_calls
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            return Ok((phys_block, remaining));
        }

        let (i_flags, i_block) = {
            let raw = self.raw.lock();
            (raw.i_flags, raw.i_block)
        };

        if (i_flags & 0x80000) != 0 {
            self.resolve_extent_block_len(&i_block, file_block)
        } else {
            self.resolve_indirect_block_run(&i_block, file_block)
        }
    }

    /// Internal core resolution routine decoupling raw lock from disk I/O.
    pub fn resolve_block_from_flags_and_block(
        &self,
        i_flags: u32,
        i_block: &[u32; 15],
        file_block: u32,
    ) -> Result<u32, &'static str> {
        if (i_flags & 0x80000) != 0 {
            return self.resolve_extent_block(i_block, file_block);
        }

        self.resolve_indirect_block_run(i_block, file_block)
            .map(|(phys, _)| phys)
    }

    /// Resolve indirect-mapped block and find contiguous run length.
    pub fn resolve_indirect_block_run(
        &self,
        i_block: &[u32; 15],
        file_block: u32,
    ) -> Result<(u32, u32), &'static str> {
        crate::fs::kstats::KSTATS
            .resolve_block_calls
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);

        let block_size = self.fs.block_size as usize;
        assert!(block_size <= 4096);
        let refs_per_block = (self.fs.block_size / 4) as u32;

        if file_block < 12 {
            let phys = i_block[file_block as usize];
            let mut run_len = 1u32;
            if phys == 0 {
                while file_block + run_len < 12 && i_block[(file_block + run_len) as usize] == 0 {
                    run_len += 1;
                }
            } else {
                while file_block + run_len < 12
                    && i_block[(file_block + run_len) as usize] == phys + run_len
                {
                    run_len += 1;
                }
                self.extent_cache.update(file_block, run_len, phys as u64);
            }
            return Ok((phys, run_len));
        }

        let indirect_index = file_block - 12;
        if indirect_index < refs_per_block {
            let sib = i_block[12];
            if sib == 0 {
                return Ok((0, refs_per_block - indirect_index));
            }

            let mut ind_buf = [core::mem::MaybeUninit::<u8>::uninit(); 4096];
            // SAFETY: read_blocks fills block_size bytes.
            let ind_slice = unsafe {
                core::slice::from_raw_parts_mut(ind_buf.as_mut_ptr() as *mut u8, block_size)
            };
            read_blocks(&*self.fs.device, sib as u64, ind_slice, self.fs.block_size)?;

            let ptr_offset = (indirect_index * 4) as usize;
            let phys_block = u32::from_le_bytes([
                ind_slice[ptr_offset],
                ind_slice[ptr_offset + 1],
                ind_slice[ptr_offset + 2],
                ind_slice[ptr_offset + 3],
            ]);

            let mut run_len = 1u32;
            if phys_block == 0 {
                while indirect_index + run_len < refs_per_block {
                    let off = ((indirect_index + run_len) * 4) as usize;
                    let next = u32::from_le_bytes([
                        ind_slice[off],
                        ind_slice[off + 1],
                        ind_slice[off + 2],
                        ind_slice[off + 3],
                    ]);
                    if next == 0 {
                        run_len += 1;
                    } else {
                        break;
                    }
                }
            } else {
                while indirect_index + run_len < refs_per_block {
                    let off = ((indirect_index + run_len) * 4) as usize;
                    let next = u32::from_le_bytes([
                        ind_slice[off],
                        ind_slice[off + 1],
                        ind_slice[off + 2],
                        ind_slice[off + 3],
                    ]);
                    if next == phys_block + run_len {
                        run_len += 1;
                    } else {
                        break;
                    }
                }
                self.extent_cache
                    .update(file_block, run_len, phys_block as u64);
            }
            return Ok((phys_block, run_len));
        }

        let double_index = indirect_index - refs_per_block;
        let max_double_blocks = refs_per_block * refs_per_block;
        if double_index < max_double_blocks {
            let dib = i_block[13];
            if dib == 0 {
                return Ok((0, 1));
            }

            let mut dib_buf = [core::mem::MaybeUninit::<u8>::uninit(); 4096];
            // SAFETY: read_blocks fills block_size bytes.
            let dib_slice = unsafe {
                core::slice::from_raw_parts_mut(dib_buf.as_mut_ptr() as *mut u8, block_size)
            };
            read_blocks(&*self.fs.device, dib as u64, dib_slice, self.fs.block_size)?;

            let sib_index = double_index / refs_per_block;
            let sib_ptr_offset = (sib_index * 4) as usize;
            let sib = u32::from_le_bytes([
                dib_slice[sib_ptr_offset],
                dib_slice[sib_ptr_offset + 1],
                dib_slice[sib_ptr_offset + 2],
                dib_slice[sib_ptr_offset + 3],
            ]);
            if sib == 0 {
                return Ok((0, 1));
            }

            let mut sib_buf = [core::mem::MaybeUninit::<u8>::uninit(); 4096];
            // SAFETY: read_blocks fills block_size bytes.
            let sib_slice = unsafe {
                core::slice::from_raw_parts_mut(sib_buf.as_mut_ptr() as *mut u8, block_size)
            };
            read_blocks(&*self.fs.device, sib as u64, sib_slice, self.fs.block_size)?;

            let data_index = double_index % refs_per_block;
            let data_ptr_offset = (data_index * 4) as usize;
            let phys_block = u32::from_le_bytes([
                sib_slice[data_ptr_offset],
                sib_slice[data_ptr_offset + 1],
                sib_slice[data_ptr_offset + 2],
                sib_slice[data_ptr_offset + 3],
            ]);

            let mut run_len = 1u32;
            if phys_block == 0 {
                while data_index + run_len < refs_per_block {
                    let off = ((data_index + run_len) * 4) as usize;
                    let next = u32::from_le_bytes([
                        sib_slice[off],
                        sib_slice[off + 1],
                        sib_slice[off + 2],
                        sib_slice[off + 3],
                    ]);
                    if next == 0 {
                        run_len += 1;
                    } else {
                        break;
                    }
                }
            } else {
                while data_index + run_len < refs_per_block {
                    let off = ((data_index + run_len) * 4) as usize;
                    let next = u32::from_le_bytes([
                        sib_slice[off],
                        sib_slice[off + 1],
                        sib_slice[off + 2],
                        sib_slice[off + 3],
                    ]);
                    if next == phys_block + run_len {
                        run_len += 1;
                    } else {
                        break;
                    }
                }
                self.extent_cache
                    .update(file_block, run_len, phys_block as u64);
            }
            return Ok((phys_block, run_len));
        }

        Err("Triple indirect blocks are unsupported in this phase.")
    }

    /// Dynamically allocate a chunk of contiguous physical blocks and register them in the Ext4 extent tree.
    pub fn allocate_extent_block_chunk(
        &self,
        raw: &mut ExtRawInode,
        file_block: u32,
        count: u32,
    ) -> Result<(u32, u32), &'static str> {
        self.invalidate_extent_cache();
        // Preferred block is after last physical block if possible
        let preferred = 0;
        let (new_phys_start, alloc_count) = self.fs.allocate_blocks_contiguous(preferred, count)?;

        let mut root_buf = [0u8; 60];
        for i in 0..15 {
            root_buf[i * 4..i * 4 + 4].copy_from_slice(&raw.i_block[i].to_le_bytes());
        }

        let mut root_hdr =
            unsafe { core::ptr::read_unaligned(root_buf.as_ptr() as *const Ext4ExtentHeader) };
        if root_hdr.eh_magic != 0xF30A {
            root_hdr = Ext4ExtentHeader {
                eh_magic: 0xF30A,
                eh_entries: 0,
                eh_max: 4,
                eh_depth: 0,
                eh_generation: 0,
            };
            unsafe {
                core::ptr::write_unaligned(
                    root_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                    root_hdr,
                );
            }
        }

        if root_hdr.eh_depth == 0 {
            // Check if we can merge with the last extent in root_buf
            if root_hdr.eh_entries > 0 {
                let last_idx = (root_hdr.eh_entries - 1) as usize;
                let offset = 12 + last_idx * core::mem::size_of::<Ext4Extent>();
                let mut last_ext = unsafe {
                    core::ptr::read_unaligned(root_buf[offset..].as_ptr() as *const Ext4Extent)
                };
                let last_len = last_ext.len() as u32;
                if last_ext.ee_block + last_len == file_block
                    && last_ext.start_block() + (last_len as u64) == new_phys_start as u64
                    && (last_ext.ee_len as u32 + alloc_count) <= 32767
                {
                    last_ext.ee_len += alloc_count as u16;
                    unsafe {
                        core::ptr::write_unaligned(
                            root_buf[offset..].as_mut_ptr() as *mut Ext4Extent,
                            last_ext,
                        );
                    }
                    for i in 0..15 {
                        raw.i_block[i] = u32::from_le_bytes([
                            root_buf[i * 4],
                            root_buf[i * 4 + 1],
                            root_buf[i * 4 + 2],
                            root_buf[i * 4 + 3],
                        ]);
                    }
                    raw.i_blocks += alloc_count * (self.fs.block_size / 512);
                    self.fs.write_inode(self.ino, raw)?;
                    return Ok((new_phys_start, alloc_count));
                }
            }

            if root_hdr.eh_entries < root_hdr.eh_max {
                let new_ext = Ext4Extent {
                    ee_block: file_block,
                    ee_len: alloc_count as u16,
                    ee_start_hi: ((new_phys_start as u64) >> 32) as u16,
                    ee_start_lo: new_phys_start as u32,
                };
                let num_entries = root_hdr.eh_entries as usize;
                let mut insert_idx = num_entries;
                for i in 0..num_entries {
                    let ext_offset = 12 + i * core::mem::size_of::<Ext4Extent>();
                    // SAFETY: Reading valid extent within bounds
                    let cur_ext = unsafe {
                        core::ptr::read_unaligned(
                            root_buf[ext_offset..].as_ptr() as *const Ext4Extent
                        )
                    };
                    if cur_ext.ee_block > file_block {
                        insert_idx = i;
                        break;
                    }
                }
                for i in (insert_idx..num_entries).rev() {
                    let src = 12 + i * core::mem::size_of::<Ext4Extent>();
                    let dst = 12 + (i + 1) * core::mem::size_of::<Ext4Extent>();
                    root_buf.copy_within(src..src + core::mem::size_of::<Ext4Extent>(), dst);
                }
                let offset = 12 + insert_idx * core::mem::size_of::<Ext4Extent>();
                // SAFETY: offset is within root_buf
                unsafe {
                    core::ptr::write_unaligned(
                        root_buf[offset..].as_mut_ptr() as *mut Ext4Extent,
                        new_ext,
                    );
                }
                root_hdr.eh_entries += 1;
                unsafe {
                    core::ptr::write_unaligned(
                        root_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                        root_hdr,
                    );
                }
                for i in 0..15 {
                    raw.i_block[i] = u32::from_le_bytes([
                        root_buf[i * 4],
                        root_buf[i * 4 + 1],
                        root_buf[i * 4 + 2],
                        root_buf[i * 4 + 3],
                    ]);
                }
                raw.i_blocks += alloc_count * (self.fs.block_size / 512);
                self.fs.write_inode(self.ino, raw)?;
                return Ok((new_phys_start, alloc_count));
            }

            // Root node is full. Split root to tree depth 1!
            let leaf_block = self.fs.allocate_block()?;
            let mut leaf_buf = alloc::vec![0u8; self.fs.block_size as usize];
            let max_leaf_entries = ((self.fs.block_size - 12) / 12) as u16;

            let leaf_hdr = Ext4ExtentHeader {
                eh_magic: 0xF30A,
                eh_entries: root_hdr.eh_entries + 1,
                eh_max: max_leaf_entries,
                eh_depth: 0,
                eh_generation: 0,
            };
            unsafe {
                core::ptr::write_unaligned(
                    leaf_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                    leaf_hdr,
                );
            }
            leaf_buf[12..60].copy_from_slice(&root_buf[12..60]);

            let new_ext = Ext4Extent {
                ee_block: file_block,
                ee_len: alloc_count as u16,
                ee_start_hi: ((new_phys_start as u64) >> 32) as u16,
                ee_start_lo: new_phys_start as u32,
            };
            unsafe {
                core::ptr::write_unaligned(leaf_buf[60..].as_mut_ptr() as *mut Ext4Extent, new_ext);
            }
            write_blocks(
                &*self.fs.device,
                leaf_block as u64,
                &leaf_buf,
                self.fs.block_size,
            )?;

            root_buf.fill(0);
            let new_root_hdr = Ext4ExtentHeader {
                eh_magic: 0xF30A,
                eh_entries: 1,
                eh_max: 4,
                eh_depth: 1,
                eh_generation: 0,
            };
            unsafe {
                core::ptr::write_unaligned(
                    root_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                    new_root_hdr,
                );
            }
            let first_ee_block =
                unsafe { core::ptr::read_unaligned(leaf_buf[12..].as_ptr() as *const Ext4Extent) }
                    .ee_block;
            let root_idx = Ext4ExtentIdx {
                ei_block: first_ee_block,
                ei_leaf_lo: leaf_block as u32,
                ei_leaf_hi: ((leaf_block as u64) >> 32) as u16,
                ei_unused: 0,
            };
            unsafe {
                core::ptr::write_unaligned(
                    root_buf[12..].as_mut_ptr() as *mut Ext4ExtentIdx,
                    root_idx,
                );
            }
            for i in 0..15 {
                raw.i_block[i] = u32::from_le_bytes([
                    root_buf[i * 4],
                    root_buf[i * 4 + 1],
                    root_buf[i * 4 + 2],
                    root_buf[i * 4 + 3],
                ]);
            }
            raw.i_blocks += (self.fs.block_size / 512) * (1 + alloc_count);
            self.fs.write_inode(self.ino, raw)?;
            return Ok((new_phys_start, alloc_count));
        } else if root_hdr.eh_depth >= 1 {
            let num_indices = root_hdr.eh_entries as usize;
            if num_indices == 0 {
                return Err("Corrupted Ext4 extent index: 0 entries at depth >= 1");
            }
            let mut target_idx_pos = 0;
            let mut best_block = 0;
            for i in 0..num_indices {
                let offset = 12 + i * core::mem::size_of::<Ext4ExtentIdx>();
                // SAFETY: Reading index entry within root_buf
                let idx = unsafe {
                    core::ptr::read_unaligned(root_buf[offset..].as_ptr() as *const Ext4ExtentIdx)
                };
                if idx.ei_block <= file_block {
                    if i == 0 || idx.ei_block >= best_block {
                        best_block = idx.ei_block;
                        target_idx_pos = i;
                    }
                }
            }
            let target_idx_offset = 12 + target_idx_pos * core::mem::size_of::<Ext4ExtentIdx>();
            let target_idx = unsafe {
                core::ptr::read_unaligned(
                    root_buf[target_idx_offset..].as_ptr() as *const Ext4ExtentIdx
                )
            };
            let leaf_block = target_idx.leaf_block();
            let mut leaf_buf = alloc::vec![0u8; self.fs.block_size as usize];
            read_blocks(
                &*self.fs.device,
                leaf_block,
                &mut leaf_buf,
                self.fs.block_size,
            )?;

            let mut leaf_hdr =
                unsafe { core::ptr::read_unaligned(leaf_buf.as_ptr() as *const Ext4ExtentHeader) };
            if leaf_hdr.eh_magic != 0xF30A {
                return Err("Corrupted extent leaf header");
            }

            if leaf_hdr.eh_depth == 0 {
                for i in 0..leaf_hdr.eh_entries as usize {
                    let ext_offset = 12 + i * core::mem::size_of::<Ext4Extent>();
                    let mut ext = unsafe {
                        core::ptr::read_unaligned(
                            leaf_buf[ext_offset..].as_ptr() as *const Ext4Extent
                        )
                    };
                    let ext_len = ext.len() as u32;
                    if ext.ee_block + ext_len == file_block
                        && ext.start_block() + (ext_len as u64) == new_phys_start as u64
                        && (ext.ee_len as u32 + alloc_count) <= 32767
                    {
                        ext.ee_len += alloc_count as u16;
                        unsafe {
                            core::ptr::write_unaligned(
                                leaf_buf[ext_offset..].as_mut_ptr() as *mut Ext4Extent,
                                ext,
                            );
                        }
                        write_blocks(&*self.fs.device, leaf_block, &leaf_buf, self.fs.block_size)?;
                        raw.i_blocks += alloc_count * (self.fs.block_size / 512);
                        self.fs.write_inode(self.ino, raw)?;
                        return Ok((new_phys_start, alloc_count));
                    }
                }

                if leaf_hdr.eh_entries < leaf_hdr.eh_max {
                    let new_ext = Ext4Extent {
                        ee_block: file_block,
                        ee_len: alloc_count as u16,
                        ee_start_hi: ((new_phys_start as u64) >> 32) as u16,
                        ee_start_lo: new_phys_start as u32,
                    };
                    let num_leaf_extents = leaf_hdr.eh_entries as usize;
                    let mut insert_pos = num_leaf_extents;
                    for i in 0..num_leaf_extents {
                        let offset = 12 + i * core::mem::size_of::<Ext4Extent>();
                        let cur_ext = unsafe {
                            core::ptr::read_unaligned(
                                leaf_buf[offset..].as_ptr() as *const Ext4Extent
                            )
                        };
                        if cur_ext.ee_block > file_block {
                            insert_pos = i;
                            break;
                        }
                    }
                    for i in (insert_pos..num_leaf_extents).rev() {
                        let src = 12 + i * core::mem::size_of::<Ext4Extent>();
                        let dst = 12 + (i + 1) * core::mem::size_of::<Ext4Extent>();
                        leaf_buf.copy_within(src..src + core::mem::size_of::<Ext4Extent>(), dst);
                    }
                    let ext_offset = 12 + insert_pos * core::mem::size_of::<Ext4Extent>();
                    unsafe {
                        core::ptr::write_unaligned(
                            leaf_buf[ext_offset..].as_mut_ptr() as *mut Ext4Extent,
                            new_ext,
                        );
                    }
                    leaf_hdr.eh_entries += 1;
                    unsafe {
                        core::ptr::write_unaligned(
                            leaf_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                            leaf_hdr,
                        );
                    }
                    write_blocks(&*self.fs.device, leaf_block, &leaf_buf, self.fs.block_size)?;
                    raw.i_blocks += alloc_count * (self.fs.block_size / 512);
                    self.fs.write_inode(self.ino, raw)?;
                    return Ok((new_phys_start, alloc_count));
                }

                // Leaf is full! Allocate a new leaf block
                if root_hdr.eh_entries < root_hdr.eh_max {
                    let new_leaf_block = self.fs.allocate_block()?;
                    let mut new_leaf_buf = alloc::vec![0u8; self.fs.block_size as usize];
                    let max_leaf_entries = ((self.fs.block_size - 12) / 12) as u16;
                    let new_leaf_hdr = Ext4ExtentHeader {
                        eh_magic: 0xF30A,
                        eh_entries: 1,
                        eh_max: max_leaf_entries,
                        eh_depth: 0,
                        eh_generation: 0,
                    };
                    unsafe {
                        core::ptr::write_unaligned(
                            new_leaf_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                            new_leaf_hdr,
                        );
                    }
                    let new_ext = Ext4Extent {
                        ee_block: file_block,
                        ee_len: alloc_count as u16,
                        ee_start_hi: ((new_phys_start as u64) >> 32) as u16,
                        ee_start_lo: new_phys_start as u32,
                    };
                    unsafe {
                        core::ptr::write_unaligned(
                            new_leaf_buf[12..].as_mut_ptr() as *mut Ext4Extent,
                            new_ext,
                        );
                    }
                    write_blocks(
                        &*self.fs.device,
                        new_leaf_block as u64,
                        &new_leaf_buf,
                        self.fs.block_size,
                    )?;

                    let new_idx = Ext4ExtentIdx {
                        ei_block: file_block,
                        ei_leaf_lo: new_leaf_block as u32,
                        ei_leaf_hi: ((new_leaf_block as u64) >> 32) as u16,
                        ei_unused: 0,
                    };
                    let num_root_indices = root_hdr.eh_entries as usize;
                    let mut insert_idx_pos = num_root_indices;
                    for i in 0..num_root_indices {
                        let offset = 12 + i * core::mem::size_of::<Ext4ExtentIdx>();
                        let cur_idx = unsafe {
                            core::ptr::read_unaligned(
                                root_buf[offset..].as_ptr() as *const Ext4ExtentIdx
                            )
                        };
                        if cur_idx.ei_block > file_block {
                            insert_idx_pos = i;
                            break;
                        }
                    }
                    for i in (insert_idx_pos..num_root_indices).rev() {
                        let src = 12 + i * core::mem::size_of::<Ext4ExtentIdx>();
                        let dst = 12 + (i + 1) * core::mem::size_of::<Ext4ExtentIdx>();
                        root_buf.copy_within(src..src + core::mem::size_of::<Ext4ExtentIdx>(), dst);
                    }
                    let new_idx_offset =
                        12 + insert_idx_pos * core::mem::size_of::<Ext4ExtentIdx>();
                    unsafe {
                        core::ptr::write_unaligned(
                            root_buf[new_idx_offset..].as_mut_ptr() as *mut Ext4ExtentIdx,
                            new_idx,
                        );
                    }
                    root_hdr.eh_entries += 1;
                    unsafe {
                        core::ptr::write_unaligned(
                            root_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                            root_hdr,
                        );
                    }
                    for i in 0..15 {
                        raw.i_block[i] = u32::from_le_bytes([
                            root_buf[i * 4],
                            root_buf[i * 4 + 1],
                            root_buf[i * 4 + 2],
                            root_buf[i * 4 + 3],
                        ]);
                    }
                    raw.i_blocks += (self.fs.block_size / 512) * (1 + alloc_count);
                    self.fs.write_inode(self.ino, raw)?;
                    return Ok((new_phys_start, alloc_count));
                } else {
                    // Root is full! Grow extent tree to depth + 1
                    let new_idx_block = self.fs.allocate_block()?;
                    let mut new_idx_buf = alloc::vec![0u8; self.fs.block_size as usize];
                    let max_idx_entries = ((self.fs.block_size - 12) / 12) as u16;
                    let new_idx_hdr = Ext4ExtentHeader {
                        eh_magic: 0xF30A,
                        eh_entries: root_hdr.eh_entries,
                        eh_max: max_idx_entries,
                        eh_depth: root_hdr.eh_depth,
                        eh_generation: 0,
                    };
                    unsafe {
                        core::ptr::write_unaligned(
                            new_idx_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                            new_idx_hdr,
                        );
                    }
                    new_idx_buf[12..60].copy_from_slice(&root_buf[12..60]);
                    write_blocks(
                        &*self.fs.device,
                        new_idx_block as u64,
                        &new_idx_buf,
                        self.fs.block_size,
                    )?;

                    // Reset root to depth + 1 with 1 entry pointing to new_idx_block
                    root_buf.fill(0);
                    let first_idx_block =
                        unsafe {
                            core::ptr::read_unaligned(
                                new_idx_buf[12..].as_ptr() as *const Ext4ExtentIdx
                            )
                        }
                        .ei_block;
                    let new_root_hdr = Ext4ExtentHeader {
                        eh_magic: 0xF30A,
                        eh_entries: 1,
                        eh_max: 4,
                        eh_depth: root_hdr.eh_depth + 1,
                        eh_generation: 0,
                    };
                    unsafe {
                        core::ptr::write_unaligned(
                            root_buf.as_mut_ptr() as *mut Ext4ExtentHeader,
                            new_root_hdr,
                        );
                    }
                    let root_idx = Ext4ExtentIdx {
                        ei_block: first_idx_block,
                        ei_leaf_lo: new_idx_block as u32,
                        ei_leaf_hi: ((new_idx_block as u64) >> 32) as u16,
                        ei_unused: 0,
                    };
                    unsafe {
                        core::ptr::write_unaligned(
                            root_buf[12..].as_mut_ptr() as *mut Ext4ExtentIdx,
                            root_idx,
                        );
                    }
                    for i in 0..15 {
                        raw.i_block[i] = u32::from_le_bytes([
                            root_buf[i * 4],
                            root_buf[i * 4 + 1],
                            root_buf[i * 4 + 2],
                            root_buf[i * 4 + 3],
                        ]);
                    }
                    raw.i_blocks += self.fs.block_size / 512;
                    self.fs.write_inode(self.ino, raw)?;
                    return self.allocate_extent_block_chunk(raw, file_block, alloc_count);
                }
            }
        }

        Err("Ext4 extent tree maximum capacity exceeded")
    }

    /// Dynamically allocate a single physical block and register it in the Ext4 extent tree.
    pub fn allocate_extent_block(
        &self,
        raw: &mut ExtRawInode,
        file_block: u32,
    ) -> Result<u32, &'static str> {
        self.allocate_extent_block_chunk(raw, file_block, 1)
            .map(|(block, _)| block)
    }

    /// Retrieve or dynamically allocate physical disk blocks for a file block index range (up to `count`).
    /// Returns `(start_phys_block, allocated_count)`.
    pub fn get_or_alloc_block_chunk(
        &self,
        raw: &mut ExtRawInode,
        file_block: u32,
        count: u32,
    ) -> Result<(u32, u32), &'static str> {
        if (raw.i_flags & 0x80000) != 0 {
            let i_block = raw.i_block;
            if let Ok((phys_block, len)) = self.resolve_extent_block_len(&i_block, file_block) {
                if phys_block != 0 {
                    return Ok((phys_block, len.min(count).max(1)));
                }
            }
            return self.allocate_extent_block_chunk(raw, file_block, count);
        }

        let phys = self.get_or_alloc_indirect_block(raw, file_block)?;
        Ok((phys, 1))
    }

    /// Helper to retrieve or allocate a block for legacy non-extent indirect files.
    fn get_or_alloc_indirect_block(
        &self,
        raw: &mut ExtRawInode,
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
                let zero_buf = [0u8; 4096];
                let block_size = self.fs.block_size as usize;
                write_blocks(
                    &*self.fs.device,
                    sib as u64,
                    &zero_buf[..block_size],
                    self.fs.block_size,
                )?;
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
            let block_size = self.fs.block_size as usize;
            assert!(block_size <= 4096);

            let mut dib = raw.i_block[13];
            if dib == 0 {
                dib = self.fs.allocate_block()?;
                let zero_buf = [0u8; 4096];
                write_blocks(
                    &*self.fs.device,
                    dib as u64,
                    &zero_buf[..block_size],
                    self.fs.block_size,
                )?;
                raw.i_block[13] = dib;
                raw.i_blocks += self.fs.block_size / 512;
                self.fs.write_inode(self.ino, raw)?;
            }

            let mut dib_buf = [0u8; 4096];
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
                let zero_buf = [0u8; 4096];
                write_blocks(
                    &*self.fs.device,
                    sib as u64,
                    &zero_buf[..block_size],
                    self.fs.block_size,
                )?;
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

    /// Retrieve or dynamically allocate a physical disk block for a file block index.
    pub fn get_or_alloc_block(
        &self,
        raw: &mut ExtRawInode,
        file_block: u32,
    ) -> Result<u32, &'static str> {
        self.get_or_alloc_block_chunk(raw, file_block, 1)
            .map(|(block, _)| block)
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
            let block_size = self.fs.block_size as usize;
            let remaining = buf.len() - written_bytes;

            // Fast path: contiguous full-block write
            if block_offset == 0 && remaining >= block_size {
                let needed_blocks = (remaining / block_size) as u32;
                let chunk_req = needed_blocks.min(64);
                let (phys_start, alloc_count) = self
                    .get_or_alloc_block_chunk(&mut raw, file_block, chunk_req)
                    .map_err(|_| -5)?;
                let bytes_to_write = (alloc_count as usize) * block_size;
                let slice = &buf[written_bytes..written_bytes + bytes_to_write];

                if write_blocks(
                    &*self.fs.device,
                    phys_start as u64,
                    slice,
                    self.fs.block_size,
                )
                .is_err()
                {
                    return Err(-5);
                }

                written_bytes += bytes_to_write;
                current_offset += bytes_to_write as u64;
                continue;
            }

            let phys_block = self
                .get_or_alloc_block(&mut raw, file_block)
                .map_err(|_| -5)?; // EIO

            let bytes_to_write = core::cmp::min(remaining, block_size - block_offset);

            let is_full_block = block_offset == 0 && bytes_to_write == block_size;
            let mut block_buf = [0u8; 4096];
            assert!(block_size <= 4096);

            if !is_full_block {
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

        let now = crate::fs::vfs::current_time_sec();
        raw.i_mtime = now;
        raw.i_ctime = now;
        vfs.mtime = now as u64;
        vfs.ctime = now as u64;

        self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?;

        Ok(written_bytes)
    }

    fn deallocate_extent_node(&self, buf: &[u8]) -> Result<(), &'static str> {
        if buf.len() < 12 {
            return Ok(());
        }
        // SAFETY: buf is verified to be at least 12 bytes, matching Ext4ExtentHeader layout.
        let header = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const Ext4ExtentHeader) };
        if header.eh_magic != 0xF30A {
            return Ok(());
        }

        let entries = header.eh_entries as usize;
        let depth = header.eh_depth;

        if depth == 0 {
            let entry_size = core::mem::size_of::<Ext4Extent>();
            for i in 0..entries {
                let offset = 12 + i * entry_size;
                if offset + entry_size <= buf.len() {
                    // SAFETY: offset + entry_size is checked within buf bounds.
                    let ext = unsafe {
                        core::ptr::read_unaligned(buf[offset..].as_ptr() as *const Ext4Extent)
                    };
                    let start = ext.start_block();
                    let len = ext.len() as u64;
                    for b in 0..len {
                        let _ = self.fs.deallocate_block((start + b) as u32);
                    }
                }
            }
        } else {
            let entry_size = core::mem::size_of::<Ext4ExtentIdx>();
            for i in 0..entries {
                let offset = 12 + i * entry_size;
                if offset + entry_size <= buf.len() {
                    // SAFETY: offset + entry_size is checked within buf bounds.
                    let idx = unsafe {
                        core::ptr::read_unaligned(buf[offset..].as_ptr() as *const Ext4ExtentIdx)
                    };
                    let child_block = idx.leaf_block();
                    let mut child_buf = alloc::vec![0u8; self.fs.block_size as usize];
                    if read_blocks(
                        &*self.fs.device,
                        child_block,
                        &mut child_buf,
                        self.fs.block_size,
                    )
                    .is_ok()
                    {
                        let _ = self.deallocate_extent_node(&child_buf);
                    }
                    let _ = self.fs.deallocate_block(child_block as u32);
                }
            }
        }
        Ok(())
    }

    /// Deallocate all blocks in an Ext4 extent tree and reinitialize an empty extent root header.
    pub fn deallocate_extent_tree(&self, i_block: &mut [u32; 15]) -> Result<(), &'static str> {
        let mut root_buf = [0u8; 60];
        for i in 0..15 {
            root_buf[i * 4..i * 4 + 4].copy_from_slice(&i_block[i].to_le_bytes());
        }
        self.deallocate_extent_node(&root_buf)?;
        self.invalidate_extent_cache();

        let mut empty_header = [0u8; 60];
        let eh = Ext4ExtentHeader {
            eh_magic: 0xF30A,
            eh_entries: 0,
            eh_max: 4,
            eh_depth: 0,
            eh_generation: 0,
        };
        // SAFETY: Size of Ext4ExtentHeader is 12 bytes, fitting within the 60-byte empty_header buffer.
        unsafe {
            core::ptr::write_unaligned(empty_header.as_mut_ptr() as *mut Ext4ExtentHeader, eh);
        }
        for i in 0..15 {
            i_block[i] = u32::from_le_bytes([
                empty_header[i * 4],
                empty_header[i * 4 + 1],
                empty_header[i * 4 + 2],
                empty_header[i * 4 + 3],
            ]);
        }
        Ok(())
    }

    /// Truncate file size to 0.
    pub fn truncate_file(&self, size: u64) -> Result<(), i32> {
        self.invalidate_extent_cache();
        crate::memory::page_cache::page_cache_truncate_inode(
            crate::fs::ext::EXT_DEV_ID,
            self.ino as u64,
            size,
        );
        if size == 0 {
            let mut raw = self.raw.lock();
            let mut vfs = self.vfs_inode.write();

            let is_symlink = (raw.i_mode & 0xF000) == 0xA000;
            let is_fast_symlink = is_symlink && (raw.i_size < 60 || raw.i_blocks == 0);

            if !is_fast_symlink {
                let is_extents = (raw.i_flags & 0x80000) != 0;
                if is_extents {
                    let mut i_block = raw.i_block;
                    let _ = self.deallocate_extent_tree(&mut i_block);
                    raw.i_block = i_block;
                } else {
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
                }
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

    /// Read data from regular file or symlink using the Page Cache with batched reads and readahead.
    pub fn read_page_cache(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let file_size = self.inode().size;
        if offset >= file_size {
            return Ok(0);
        }

        let dev = crate::fs::ext::EXT_DEV_ID;
        let ino = self.ino as u64;

        let mut read_bytes = 0;
        let mut current_offset = offset;

        while read_bytes < buf.len() && current_offset < file_size {
            let file_page_offset = current_offset & !4095;
            let page_offset = (current_offset % 4096) as usize;

            let phys_page = if let Some(entry) =
                crate::memory::page_cache::page_cache_get(dev, ino, file_page_offset)
            {
                crate::fs::kstats::KSTATS
                    .page_cache_hits
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                self.readahead
                    .last_offset
                    .store(file_page_offset, core::sync::atomic::Ordering::Relaxed);
                entry.phys_addr
            } else {
                crate::fs::kstats::KSTATS
                    .page_cache_misses
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed);

                // Adaptive readahead window calculation
                let prev_offset = self
                    .readahead
                    .last_offset
                    .load(core::sync::atomic::Ordering::Relaxed);
                let is_sequential = prev_offset.wrapping_add(4096) == file_page_offset;
                let window_pages = if is_sequential {
                    let cur = self
                        .readahead
                        .window_pages
                        .load(core::sync::atomic::Ordering::Relaxed);
                    let next = core::cmp::min(cur * 2, 32);
                    self.readahead
                        .window_pages
                        .store(next, core::sync::atomic::Ordering::Relaxed);
                    next as usize
                } else {
                    self.readahead
                        .window_pages
                        .store(8, core::sync::atomic::Ordering::Relaxed);
                    8
                };
                self.readahead
                    .last_offset
                    .store(file_page_offset, core::sync::atomic::Ordering::Relaxed);

                let max_pages_to_eof = ((file_size - file_page_offset + 4095) / 4096) as usize;
                let target_pages = core::cmp::min(window_pages, max_pages_to_eof).max(1);

                // Resolve contiguous extent run on disk
                let block_size = self.fs.block_size as u64;
                let file_block = (file_page_offset / block_size) as u32;

                let (phys_start, remaining_blocks) = match self.resolve_block_run(file_block) {
                    Ok(res) => res,
                    Err(_) => return Err(-5), // EIO
                };

                let blocks_per_page = (4096 / block_size).max(1) as u32;
                let pages_in_extent = if remaining_blocks >= blocks_per_page {
                    (remaining_blocks / blocks_per_page) as usize
                } else {
                    1
                };
                let batch_pages = core::cmp::min(target_pages, pages_in_extent).max(1);

                if phys_start == 0 {
                    // Sparse hole: zero fill frames without any disk I/O
                    let mut resolved_first_phys = None;
                    for i in 0..batch_pages {
                        let page_off = file_page_offset + (i as u64) * 4096;
                        if page_off >= file_size {
                            break;
                        }
                        if let Some(entry) =
                            crate::memory::page_cache::page_cache_get(dev, ino, page_off)
                        {
                            if i == 0 {
                                resolved_first_phys = Some(entry.phys_addr);
                            }
                            continue;
                        }
                        if let Some(phys) = crate::memory::physical::allocate_frame() {
                            let virt_off = phys + crate::memory::r#virtual::phys_mem_offset();
                            // SAFETY: phys is a freshly allocated physical frame mapped at direct map offset.
                            unsafe {
                                core::ptr::write_bytes(virt_off as *mut u8, 0, 4096);
                            }
                            let entry = crate::memory::page_cache::PageCacheEntry {
                                phys_addr: phys,
                                dirty: false,
                                referenced: true,
                            };
                            if let Some(existing) =
                                crate::memory::page_cache::page_cache_insert_or_get(
                                    dev, ino, page_off, entry,
                                )
                            {
                                crate::memory::physical::deallocate_frame(phys);
                                if i == 0 {
                                    resolved_first_phys = Some(existing);
                                }
                            } else if i == 0 {
                                resolved_first_phys = Some(phys);
                            }
                        }
                    }
                    resolved_first_phys.ok_or(-5)?
                } else {
                    // Real disk extent: try allocating contiguous frames for batched read
                    let mut handled = false;
                    let mut first_page_phys = 0u64;

                    if let Some(first_phys) =
                        crate::memory::physical::allocate_contiguous_frames(batch_pages)
                    {
                        let total_bytes = batch_pages * 4096;
                        let phys_mem_off = crate::memory::r#virtual::phys_mem_offset();
                        // SAFETY: allocate_contiguous_frames returns batch_pages contiguous frames directly mapped at phys_mem_off.
                        let buf_slice = unsafe {
                            core::slice::from_raw_parts_mut(
                                (first_phys + phys_mem_off) as *mut u8,
                                total_bytes,
                            )
                        };

                        if read_blocks(
                            &*self.fs.device,
                            phys_start as u64,
                            buf_slice,
                            self.fs.block_size,
                        )
                        .is_ok()
                        {
                            // Zero out trailing bytes if batch extends beyond EOF
                            let batch_end_offset = file_page_offset + total_bytes as u64;
                            if batch_end_offset > file_size {
                                let valid_bytes = (file_size - file_page_offset) as usize;
                                if valid_bytes < total_bytes {
                                    buf_slice[valid_bytes..].fill(0);
                                }
                            }

                            // Insert frames into page cache shards, handling duplicates from concurrent readers
                            for i in 0..batch_pages {
                                let page_off = file_page_offset + (i as u64) * 4096;
                                let frame_phys = first_phys + (i as u64) * 4096;

                                if page_off >= file_size && i > 0 {
                                    crate::memory::physical::deallocate_frame(frame_phys);
                                    continue;
                                }

                                let entry = crate::memory::page_cache::PageCacheEntry {
                                    phys_addr: frame_phys,
                                    dirty: false,
                                    referenced: true,
                                };

                                if let Some(existing) =
                                    crate::memory::page_cache::page_cache_insert_or_get(
                                        dev, ino, page_off, entry,
                                    )
                                {
                                    crate::memory::physical::deallocate_frame(frame_phys);
                                    if i == 0 {
                                        first_page_phys = existing;
                                    }
                                } else if i == 0 {
                                    first_page_phys = frame_phys;
                                }
                            }
                            handled = true;
                        } else {
                            // Read failed: deallocate all allocated frames
                            for i in 0..batch_pages {
                                crate::memory::physical::deallocate_frame(
                                    first_phys + (i as u64) * 4096,
                                );
                            }
                        }
                    }

                    if !handled {
                        // Reset readahead window if contiguous allocation failed due to fragmentation
                        self.readahead
                            .window_pages
                            .store(1, core::sync::atomic::Ordering::Relaxed);
                        // Fallback: allocate and read single page
                        first_page_phys = match crate::memory::page_cache::get_or_create_page_inner(
                            self,
                            file_page_offset,
                        ) {
                            Ok(p) => p,
                            Err(_) => return Err(-5),
                        };
                    }

                    first_page_phys
                }
            };

            let bytes_to_read = core::cmp::min(
                buf.len() - read_bytes,
                core::cmp::min(4096 - page_offset, (file_size - current_offset) as usize),
            );

            let phys_offset = phys_page + crate::memory::r#virtual::phys_mem_offset();
            // SAFETY: phys_page is a valid page frame in the page cache, mapped at phys_mem_offset.
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

        let mut cached_alloc_count = 0u32;
        let mut cached_start_file_block = 0u32;

        while written_bytes < buf.len() {
            let file_block = (current_offset / self.fs.block_size as u64) as u32;
            let page_offset = (current_offset % 4096) as usize;
            let bytes_to_write = core::cmp::min(buf.len() - written_bytes, 4096 - page_offset);
            let end_offset = current_offset + bytes_to_write as u64 - 1;
            let end_file_block = (end_offset / self.fs.block_size as u64) as u32;

            let mut b = file_block;
            while b <= end_file_block {
                if !(b >= cached_start_file_block
                    && b < cached_start_file_block + cached_alloc_count)
                {
                    let remaining_bytes = buf.len() - written_bytes;
                    let needed_blocks = ((remaining_bytes + self.fs.block_size as usize - 1)
                        / self.fs.block_size as usize)
                        as u32;
                    let chunk_req = needed_blocks.clamp(32, 128);

                    let (_p_start, p_count) = self
                        .get_or_alloc_block_chunk(&mut raw, b, chunk_req)
                        .map_err(|_| -5)?; // EIO
                    cached_alloc_count = p_count.max(1);
                    cached_start_file_block = b;
                }
                b = cached_start_file_block + cached_alloc_count;
            }

            let file_block_offset = current_offset & !4095;
            let old_size = vfs.size;
            let target_end = current_offset + bytes_to_write as u64;
            if target_end > vfs.size {
                vfs.size = target_end;
                raw.i_size = target_end as u32;
            }

            // Drop locks before accessing page cache to prevent double-locking deadlocks on self.vfs_inode
            drop(raw);
            drop(vfs);

            let is_full_page = page_offset == 0 && bytes_to_write == 4096;
            let is_new_page = file_block_offset >= old_size;
            let page_phys = match crate::memory::page_cache::get_or_create_page_for_write(
                self,
                file_block_offset,
                is_full_page || is_new_page,
                is_full_page,
            ) {
                Ok(p) => p,
                Err(_) => return Err(-5), // EIO
            };

            let phys_offset = page_phys + crate::memory::r#virtual::phys_mem_offset();
            // SAFETY: phys_offset points to a valid physical frame mapped into the physical memory direct mapping region. The slice covers one 4096-byte page.
            let dest_slice =
                unsafe { core::slice::from_raw_parts_mut(phys_offset as *mut u8, 4096) };

            dest_slice[page_offset..page_offset + bytes_to_write]
                .copy_from_slice(&buf[written_bytes..written_bytes + bytes_to_write]);

            written_bytes += bytes_to_write;
            current_offset += bytes_to_write as u64;

            // Re-acquire locks for the next block allocation check or loop finalization
            raw = self.raw.lock();
            vfs = self.vfs_inode.write();
        }

        vfs.blocks = raw.i_blocks as u64;

        let now = crate::fs::vfs::current_time_sec();
        raw.i_mtime = now;
        raw.i_ctime = now;
        vfs.mtime = now as u64;
        vfs.ctime = now as u64;

        self.fs.write_inode(self.ino, &raw).map_err(|_| -5)?;

        Ok(written_bytes)
    }
}
