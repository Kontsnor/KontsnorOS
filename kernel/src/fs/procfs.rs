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
        if name == "self" {
            return Some(Arc::new(ProcFsSelfDir {
                inode: Inode::new(100, FileType::Directory),
            }));
        }
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
            DirEntry {
                name: String::from("self"),
                ino: 100,
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

/// Generate `/proc/cpuinfo` content.
fn gen_cpuinfo() -> String {
    let mut out = String::new();
    let cpu_count = crate::arch::x86_64::smp::get_cpu_count();
    for i in 0..cpu_count {
        out.push_str(&format!(
            "processor\t: {}\nvendor_id\t: GenuineIntel\ncpu family\t: 6\nmodel\t\t: 158\nmodel name\t: QEMU Virtual CPU\ncpu cores\t: {}\n\n",
            i, cpu_count
        ));
    }
    out
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
    let tasks = crate::process::scheduler::TASKS.read();
    for slot in tasks.iter() {
        if let Some(task_arc) = slot {
            let task = task_arc.lock();
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
        (
            String::from("cpuinfo"),
            Arc::new(ProcFile {
                inode: Inode::new(54, FileType::Regular),
                generator: gen_cpuinfo,
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

/// Special `/proc/self` directory.
struct ProcFsSelfDir {
    inode: Inode,
}

impl InodeOps for ProcFsSelfDir {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn lookup(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        if name == "exe" {
            return Some(Arc::new(ProcFsSelfExe {
                inode: Inode::new(101, FileType::Symlink),
            }));
        }
        if name == "fd" {
            return Some(Arc::new(ProcFsSelfFdDir {
                inode: Inode::new(102, FileType::Directory),
            }));
        }
        if name == "maps" {
            return Some(Arc::new(ProcFsSelfMaps {
                inode: Inode::new(103, FileType::Regular),
            }));
        }
        if name == "status" {
            return Some(Arc::new(ProcFsSelfStatus {
                inode: Inode::new(104, FileType::Regular),
            }));
        }
        if name == "cmdline" {
            return Some(Arc::new(ProcFsSelfCmdline {
                inode: Inode::new(105, FileType::Regular),
            }));
        }
        None
    }

    fn readdir(&self) -> Vec<DirEntry> {
        vec![
            DirEntry {
                name: String::from("."),
                ino: self.inode.ino,
                file_type: FileType::Directory,
            },
            DirEntry {
                name: String::from(".."),
                ino: 49,
                file_type: FileType::Directory,
            },
            DirEntry {
                name: String::from("exe"),
                ino: 101,
                file_type: FileType::Symlink,
            },
            DirEntry {
                name: String::from("fd"),
                ino: 102,
                file_type: FileType::Directory,
            },
            DirEntry {
                name: String::from("maps"),
                ino: 103,
                file_type: FileType::Regular,
            },
            DirEntry {
                name: String::from("status"),
                ino: 104,
                file_type: FileType::Regular,
            },
            DirEntry {
                name: String::from("cmdline"),
                ino: 105,
                file_type: FileType::Regular,
            },
        ]
    }
}

/// Special `/proc/self/fd` directory.
struct ProcFsSelfFdDir {
    inode: Inode,
}

impl InodeOps for ProcFsSelfFdDir {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn lookup(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        let fd = name.parse::<i32>().ok()?;
        if fd < 0 {
            return None;
        }
        // Verify fd is open in current task
        let current_pid = crate::process::scheduler::current_pid()?;
        let task_arc = crate::process::scheduler::get_task_arc(current_pid)?;
        let task = task_arc.lock();
        let fd_table = task.fd_table.lock();
        let _ = fd_table.entries.get(fd as usize)?.as_ref()?;
        Some(Arc::new(ProcFsSelfFdLink {
            fd,
            inode: Inode::new(1000 + fd as u64, FileType::Symlink),
        }))
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
                ino: 100, // /proc/self inode is 100
                file_type: FileType::Directory,
            },
        ];

        if let Some(current_pid) = crate::process::scheduler::current_pid() {
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(current_pid) {
                let task = task_arc.lock();
                let fd_table = task.fd_table.lock();
                for (i, entry) in fd_table.entries.iter().enumerate() {
                    if entry.is_some() {
                        result.push(DirEntry {
                            name: format!("{}", i),
                            ino: 1000 + i as u64,
                            file_type: FileType::Symlink,
                        });
                    }
                }
            }
        }
        result
    }
}

/// Special `/proc/self/fd/N` symlink.
struct ProcFsSelfFdLink {
    fd: i32,
    inode: Inode,
}

impl InodeOps for ProcFsSelfFdLink {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let current_pid = crate::process::scheduler::current_pid().ok_or(-3)?; // ESRCH
        let task_arc = crate::process::scheduler::get_task_arc(current_pid).ok_or(-3)?;
        let task = task_arc.lock();
        let fd_table = task.fd_table.lock();
        let file_desc = fd_table
            .entries
            .get(self.fd as usize)
            .and_then(|x| x.as_ref())
            .ok_or(-9)?; // EBADF

        // Get the path
        let path_str = if let Some(ref p) = file_desc.path {
            p.clone()
        } else {
            // Fallback to anonymous format if no path exists
            let inode = file_desc.inode.inode();
            match inode.file_type {
                FileType::Pipe => format!("pipe:[{}]", inode.ino),
                FileType::Socket => format!("socket:[{}]", inode.ino),
                _ => {
                    // Check for other types (timerfd, epoll, etc.)
                    if file_desc.inode.as_timerfd().is_some() {
                        String::from("anon_inode:[timerfd]")
                    } else if file_desc.inode.as_epoll().is_some() {
                        String::from("anon_inode:[eventpoll]")
                    } else if file_desc.inode.as_eventfd().is_some() {
                        String::from("anon_inode:[eventfd]")
                    } else if file_desc.inode.as_signalfd().is_some() {
                        String::from("anon_inode:[signalfd]")
                    } else {
                        format!("anon_inode:[{}]", inode.ino)
                    }
                }
            }
        };

        let bytes = path_str.as_bytes();
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

/// Special `/proc/self/exe` symlink.
struct ProcFsSelfExe {
    inode: Inode,
}

impl InodeOps for ProcFsSelfExe {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let exe_path = if let Some(pid) = crate::process::scheduler::current_pid() {
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(pid) {
                task_arc.lock().name.clone()
            } else {
                String::from("/bin/sh")
            }
        } else {
            String::from("/bin/sh")
        };

        let bytes = exe_path.as_bytes();
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

/// Special `/proc/self/maps` file.
struct ProcFsSelfMaps {
    inode: Inode,
}

impl InodeOps for ProcFsSelfMaps {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let current_pid = crate::process::scheduler::current_pid().ok_or(-3)?; // ESRCH
        let task_arc = crate::process::scheduler::get_task_arc(current_pid).ok_or(-3)?;
        let mut regions = {
            let task = task_arc.lock();
            let addr_space = task.address_space.lock();
            addr_space.mmap_regions.clone()
        };

        regions.sort_by_key(|r| r.start);

        let mut content = String::new();
        for r in regions {
            let r_bit = if (r.prot & 1) != 0 { 'r' } else { '-' };
            let w_bit = if (r.prot & 2) != 0 { 'w' } else { '-' };
            let x_bit = if (r.prot & 4) != 0 { 'x' } else { '-' };
            let p_s_bit = if r.is_shared { 's' } else { 'p' };
            let perms = format!("{}{}{}{}", r_bit, w_bit, x_bit, p_s_bit);

            let start = r.start;
            let end = r.start + r.len as u64;
            let offset = r.offset;
            let ino = r.inode.as_ref().map(|i| i.inode().ino).unwrap_or(0);
            let pathname_str = r.pathname.as_deref().unwrap_or("");

            if pathname_str.is_empty() {
                content.push_str(&format!(
                    "{:08x}-{:08x} {} {:08x} 00:00 {:<10}\n",
                    start, end, perms, offset, ino
                ));
            } else {
                content.push_str(&format!(
                    "{:08x}-{:08x} {} {:08x} 00:00 {:<10} {}\n",
                    start, end, perms, offset, ino, pathname_str
                ));
            }
        }

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

/// Special `/proc/self/status` file.
struct ProcFsSelfStatus {
    inode: Inode,
}

impl InodeOps for ProcFsSelfStatus {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let current_pid = crate::process::scheduler::current_pid().ok_or(-3)?; // ESRCH
        let task_arc = crate::process::scheduler::get_task_arc(current_pid).ok_or(-3)?;

        let (name, ppid, tgid, state, vmsize, vmrss) = {
            let task = task_arc.lock();
            let name = task.name.clone();
            let ppid = task.parent_pid.as_u64();
            let tgid = task.tgid.as_u64();
            let state_char = match task.state {
                crate::process::task::TaskState::Running => "R (running)",
                crate::process::task::TaskState::Ready => "S (sleeping)",
                crate::process::task::TaskState::Blocked => "D (disk sleep)",
                crate::process::task::TaskState::Zombie => "Z (zombie)",
            };

            // Sum memory region sizes
            let mut size_bytes = 0;
            let addr_space = task.address_space.lock();
            for r in &addr_space.mmap_regions {
                size_bytes += r.len;
            }
            let vmsize_kb = size_bytes / 1024;
            let vmrss_kb = vmsize_kb; // For simplicity, resident matches virtual size in this environment

            (name, ppid, tgid, state_char, vmsize_kb, vmrss_kb)
        };

        // Count threads with the same tgid (outside the task lock)
        let threads = {
            let tasks = crate::process::scheduler::TASKS.read();
            tasks
                .iter()
                .filter_map(|t| t.as_ref())
                .filter(|t| {
                    let t_lock = t.lock();
                    t_lock.tgid.as_u64() == tgid
                        && t_lock.state != crate::process::task::TaskState::Zombie
                })
                .count()
        };

        let content = format!(
            "Name:\t{}\nState:\t{}\nTgid:\t{}\nPid:\t{}\nPPid:\t{}\nThreads:\t{}\nVmSize:\t{} kB\nVmRSS:\t{} kB\n",
            name, state, tgid, current_pid.as_u64(), ppid, threads, vmsize, vmrss
        );

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

/// Special `/proc/self/cmdline` file.
struct ProcFsSelfCmdline {
    inode: Inode,
}

impl InodeOps for ProcFsSelfCmdline {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let current_pid = crate::process::scheduler::current_pid().ok_or(-3)?; // ESRCH
        let task_arc = crate::process::scheduler::get_task_arc(current_pid).ok_or(-3)?;

        let cmdline_bytes = {
            let task = task_arc.lock();
            if task.cmdline.is_empty() {
                let mut bytes = task.name.as_bytes().to_vec();
                bytes.push(0);
                bytes
            } else {
                let mut bytes = Vec::new();
                for arg in &task.cmdline {
                    bytes.extend_from_slice(arg.as_bytes());
                    bytes.push(0);
                }
                bytes
            }
        };

        let offset = offset as usize;
        if offset >= cmdline_bytes.len() {
            return Ok(0);
        }

        let available = cmdline_bytes.len() - offset;
        let to_read = buf.len().min(available);
        buf[..to_read].copy_from_slice(&cmdline_bytes[offset..offset + to_read]);

        Ok(to_read)
    }
}
