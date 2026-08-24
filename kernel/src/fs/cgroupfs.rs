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

//! Cgroup v2 filesystem (cgroupfs) — `/sys/fs/cgroup`.
//!
//! Provides control group nodes with default values that satisfy systemd
//! and other service managers' startup validation checks.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::inode::{DirEntry, FileType, Inode, InodeOps};
use super::vfs::FileSystem;

/// The cgroupfs filesystem.
pub struct CgroupFs {
    root: Arc<CgroupFsDir>,
}

impl FileSystem for CgroupFs {
    fn root(&self) -> Option<Arc<dyn InodeOps>> {
        Some(self.root.clone())
    }

    fn name(&self) -> &str {
        "cgroup2"
    }
}

/// A cgroupfs directory node.
struct CgroupFsDir {
    inode: Inode,
    entries: Vec<(String, Arc<dyn InodeOps>)>,
}

impl InodeOps for CgroupFsDir {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn lookup(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        self.entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, node)| node.clone())
    }

    fn readdir(&self) -> Vec<DirEntry> {
        let mut result = vec![
            DirEntry {
                name: String::from("."),
                ino: self.inode.ino,
                file_type: FileType::Directory,
            },
            DirEntry {
                name: String::from(".."),
                ino: 1,
                file_type: FileType::Directory,
            },
        ];

        for (name, node) in &self.entries {
            result.push(DirEntry {
                name: name.clone(),
                ino: node.inode().ino,
                file_type: node.inode().file_type,
            });
        }

        result
    }
}

/// A cgroupfs control file node. Supports reads and stubs success for writes.
struct CgroupFsFile {
    inode: Inode,
    generator: fn() -> String,
}

impl InodeOps for CgroupFsFile {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let content = (self.generator)();
        let bytes = content.as_bytes();
        let offset = offset as usize;

        if offset >= bytes.len() {
            return Ok(0);
        }

        let available = bytes.len() - offset;
        let to_read = buf.len().min(available);
        buf[..to_read].copy_from_slice(&bytes[offset..offset + to_read]);

        Ok(to_read)
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        // Discard data and succeed to satisfy systemd startup configuration writes
        Ok(data.len())
    }
}

fn gen_cgroup_controllers() -> String {
    String::from("cpu memory io pids\n")
}

fn gen_cgroup_subtree_control() -> String {
    String::from("\n")
}

fn gen_cgroup_procs() -> String {
    let mut out = String::new();
    let tasks = crate::process::scheduler::TASKS.read();
    for slot in tasks.iter() {
        if let Some(task_arc) = slot {
            let task = task_arc.lock();
            use crate::process::task::TaskState;
            if task.state != TaskState::Zombie {
                out.push_str(&format!("{}\n", task.pid.as_u64()));
            }
        }
    }
    out
}

/// Create a new instance of CgroupFs.
pub fn create_cgroupfs() -> Arc<CgroupFs> {
    let mut root_inode = Inode::new(400, FileType::Directory);
    // mode 0o755
    root_inode.permissions.mode = 0o755;

    let mut controllers_inode = Inode::new(401, FileType::Regular);
    controllers_inode.permissions.mode = 0o644;

    let mut subtree_inode = Inode::new(402, FileType::Regular);
    subtree_inode.permissions.mode = 0o644;

    let mut procs_inode = Inode::new(403, FileType::Regular);
    procs_inode.permissions.mode = 0o644;

    let entries = vec![
        (
            String::from("cgroup.controllers"),
            Arc::new(CgroupFsFile {
                inode: controllers_inode,
                generator: gen_cgroup_controllers,
            }) as Arc<dyn InodeOps>,
        ),
        (
            String::from("cgroup.subtree_control"),
            Arc::new(CgroupFsFile {
                inode: subtree_inode,
                generator: gen_cgroup_subtree_control,
            }) as Arc<dyn InodeOps>,
        ),
        (
            String::from("cgroup.procs"),
            Arc::new(CgroupFsFile {
                inode: procs_inode,
                generator: gen_cgroup_procs,
            }) as Arc<dyn InodeOps>,
        ),
    ];

    let root = Arc::new(CgroupFsDir {
        inode: root_inode,
        entries,
    });

    Arc::new(CgroupFs { root })
}
