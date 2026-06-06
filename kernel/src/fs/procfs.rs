//! Process filesystem (procfs) — `/proc`.
//!
//! Provides process and kernel information as virtual files,
//! following the Unix tradition of exposing kernel internals
//! through the filesystem.
//!
//! Standard entries:
//! - `/proc/version` — kernel version string
//! - `/proc/meminfo` — memory statistics
//! - `/proc/uptime` — system uptime

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;


use super::inode::{DirEntry, FileType, Inode, InodeOps};
use super::vfs::FileSystem;

/// The procfs filesystem.
pub struct ProcFs {
    root: Arc<ProcFsDir>,
}

impl FileSystem for ProcFs {
    fn root(&self) -> Option<Arc<dyn InodeOps>> {
        Some(self.root.clone())
    }

    fn name(&self) -> &str {
        "procfs"
    }
}

/// A procfs directory.
struct ProcFsDir {
    inode: Inode,
    entries: Vec<(String, Arc<dyn InodeOps>)>,
}

impl InodeOps for ProcFsDir {
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

/// A virtual file that generates its content dynamically.
struct ProcFile {
    inode: Inode,
    generator: fn() -> String,
}

impl InodeOps for ProcFile {
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
}

/// Generate `/proc/version` content.
fn gen_version() -> String {
    format!(
        "KontsnorOS version {} (rustc {}) #1 SMP\n",
        env!("CARGO_PKG_VERSION"),
        "nightly"
    )
}

/// Generate `/proc/meminfo` content.
fn gen_meminfo() -> String {
    let (total, allocated, free) = crate::memory::physical::stats();
    let page_size = crate::memory::PAGE_SIZE;

    format!(
        "MemTotal:    {} kB\nMemFree:     {} kB\nMemUsed:     {} kB\nPageSize:    {} B\n",
        (total * page_size) / 1024,
        (free * page_size) / 1024,
        (allocated * page_size) / 1024,
        page_size
    )
}

/// Generate `/proc/uptime` content.
fn gen_uptime() -> String {
    let ticks = crate::arch::x86_64::interrupts::timer_ticks();
    // Assuming ~18.2 ticks per second (PIT default frequency)
    let seconds = ticks / 18;
    format!("{}.{:02}\n", seconds, (ticks % 18) * 100 / 18)
}

/// Generate `/proc/tasks` content.
fn gen_tasks() -> String {
    let mut out = String::new();
    out.push_str("PID  PPID  STATE    NAME\n");
    if let Some(sched) = crate::process::scheduler::SCHEDULER.lock().as_ref() {
        for slot in &sched.tasks {
            if let Some(task) = slot {
                let state_str = match task.state {
                    crate::process::task::TaskState::Ready => "Ready",
                    crate::process::task::TaskState::Running => "Running",
                    crate::process::task::TaskState::Blocked => "Blocked",
                    crate::process::task::TaskState::Zombie => "Zombie",
                };
                out.push_str(&format!(
                    "{:<5} {:<5} {:<8} {}\n",
                    task.pid.as_u64(),
                    task.parent_pid.as_u64(),
                    state_str,
                    task.name
                ));
            }
        }
    }
    out
}

/// Initialize procfs and mount at `/proc`.
pub fn init() {
    let entries = vec![
        (
            String::from("version"),
            Arc::new(ProcFile {
                inode: Inode::new(50, FileType::Regular),
                generator: gen_version,
            }) as Arc<dyn InodeOps>,
        ),
        (
            String::from("meminfo"),
            Arc::new(ProcFile {
                inode: Inode::new(51, FileType::Regular),
                generator: gen_meminfo,
            }) as Arc<dyn InodeOps>,
        ),
        (
            String::from("uptime"),
            Arc::new(ProcFile {
                inode: Inode::new(52, FileType::Regular),
                generator: gen_uptime,
            }) as Arc<dyn InodeOps>,
        ),
        (
            String::from("tasks"),
            Arc::new(ProcFile {
                inode: Inode::new(53, FileType::Regular),
                generator: gen_tasks,
            }) as Arc<dyn InodeOps>,
        ),
    ];

    let root = Arc::new(ProcFsDir {
        inode: Inode::new(49, FileType::Directory),
        entries,
    });

    let procfs = Arc::new(ProcFs { root });
    super::vfs::mount(String::from("/proc"), procfs);
}
