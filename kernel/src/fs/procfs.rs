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
//! - `/proc/mounts` — active filesystem mount table

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::inode::{DirEntry, FileType, Inode, InodeOps};
use super::vfs::FileSystem;
use crate::process::pid::Pid;

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
            return Some(Arc::new(ProcFsProcessDir {
                target_pid: None,
                inode: Inode::new(100, FileType::Directory),
            }));
        }

        if let Ok(pid_val) = name.parse::<u64>() {
            let caller_ns_id = crate::process::scheduler::current_pid()
                .and_then(crate::process::scheduler::get_task_arc)
                .map(|t| t.lock().pid_ns_id)
                .unwrap_or(0);
            let target_pid = Pid::from_raw(pid_val);
            if let Some(task_arc) = crate::process::scheduler::get_task_arc(target_pid) {
                let task = task_arc.lock();
                if caller_ns_id == 0 || task.pid_ns_id == caller_ns_id {
                    return Some(Arc::new(ProcFsProcessDir {
                        target_pid: Some(target_pid),
                        inode: Inode::new(1000 + pid_val, FileType::Directory),
                    }));
                }
            }
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

        let caller_ns_id = crate::process::scheduler::current_pid()
            .and_then(crate::process::scheduler::get_task_arc)
            .map(|t| t.lock().pid_ns_id)
            .unwrap_or(0);

        {
            let tasks = crate::process::scheduler::TASKS.read();
            for slot in tasks.iter() {
                if let Some(task_arc) = slot {
                    let task = task_arc.lock();
                    if caller_ns_id != 0 && task.pid_ns_id != caller_ns_id {
                        continue;
                    }
                    let pid_val = task.pid.as_u64();
                    result.push(DirEntry {
                        name: format!("{}", pid_val),
                        ino: 1000 + pid_val,
                        file_type: FileType::Directory,
                    });
                }
            }
        }

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
    let caller_ns_id = crate::process::scheduler::current_pid()
        .and_then(crate::process::scheduler::get_task_arc)
        .map(|t| t.lock().pid_ns_id)
        .unwrap_or(0);
    let tasks = crate::process::scheduler::TASKS.read();
    for slot in tasks.iter() {
        if let Some(task_arc) = slot {
            let task = task_arc.lock();
            if caller_ns_id != 0 && task.pid_ns_id != caller_ns_id {
                continue;
            }
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

/// Generate `/proc/kstats` content — kernel performance counters.
fn gen_kstats() -> String {
    crate::fs::kstats::render()
}

/// Generate `/proc/mounts` content.
///
/// Reads the current task's mount namespace (or the global initial namespace
/// as a fallback) and formats each entry in the standard Linux
/// `/proc/mounts` format:
///
/// ```text
/// <device> <mountpoint> <fstype> <options> <dump> <pass>
/// ```
///
/// This is the file pacman reads (via `/etc/mtab -> /proc/mounts`) to
/// determine which filesystems are mounted before committing a transaction.
fn gen_mounts() -> String {
    // Prefer the current task's private namespace; fall back to the global one.
    let mounts: alloc::vec::Vec<(String, String)> = {
        let task_ns = crate::process::scheduler::current_pid()
            .and_then(crate::process::scheduler::get_task_arc)
            .map(|t| {
                let task = t.lock();
                let ns = task.fs_ctx.read().mount_ns.clone();
                ns
            });

        if let Some(ns_arc) = task_ns {
            ns_arc
                .read()
                .mounts
                .iter()
                .map(|(path, fs)| (path.clone(), String::from(fs.name())))
                .collect()
        } else {
            crate::fs::namespace::INITIAL_MOUNT_NS
                .read()
                .mounts
                .iter()
                .map(|(path, fs)| (path.clone(), String::from(fs.name())))
                .collect()
        }
    };

    let mut out = String::new();
    // Always emit a root entry so pacman finds at least one mount point.
    let mut has_root = false;
    for (mountpoint, fsname) in &mounts {
        let mp = if mountpoint.is_empty() {
            "/"
        } else {
            mountpoint.as_str()
        };
        if mp == "/" {
            has_root = true;
        }
        // Device name: use "none" for virtual filesystems, otherwise the fs name.
        let device = match fsname.as_str() {
            "ext2" | "ext4" => "/dev/vda",
            _ => "none",
        };
        out.push_str(&format!("{} {} {} rw,relatime 0 0\n", device, mp, fsname));
    }
    if !has_root {
        // Guarantee a root entry so pacman's mount-point check never fails.
        out.push_str("/dev/vda / ext2 rw,relatime 0 0\n");
    }
    out
}

/// Create a new procfs instance.
pub fn create_procfs() -> Arc<ProcFs> {
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
        (
            String::from("kstats"),
            Arc::new(ProcFile {
                inode: Inode::new(55, FileType::Regular),
                generator: gen_kstats,
            }) as Arc<dyn InodeOps>,
        ),
        (
            String::from("mounts"),
            Arc::new(ProcFile {
                inode: Inode::new(56, FileType::Regular),
                generator: gen_mounts,
            }) as Arc<dyn InodeOps>,
        ),
    ];

    let root = Arc::new(ProcFsDir {
        inode: Inode::new(49, FileType::Directory),
        entries,
    });

    Arc::new(ProcFs { root })
}

/// Initialize procfs and mount at `/proc`.
pub fn init() {
    let procfs = create_procfs();
    super::vfs::mount(String::from("/proc"), procfs);
}

fn resolve_target_task(
    target_pid: Option<Pid>,
) -> Option<(Arc<spin::Mutex<crate::process::task::Task>>, Pid)> {
    let pid = target_pid.or_else(|| crate::process::scheduler::current_pid())?;
    let task_arc = crate::process::scheduler::get_task_arc(pid)?;
    Some((task_arc, pid))
}

/// Special `/proc/self` or `/proc/<pid>` directory.
struct ProcFsProcessDir {
    target_pid: Option<Pid>,
    inode: Inode,
}

impl InodeOps for ProcFsProcessDir {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn lookup(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        let base_ino = self.inode.ino;
        if name == "exe" {
            return Some(Arc::new(ProcFsProcessExe {
                target_pid: self.target_pid,
                inode: Inode::new(base_ino * 10 + 1, FileType::Symlink),
            }));
        }
        if name == "fd" {
            return Some(Arc::new(ProcFsProcessFdDir {
                target_pid: self.target_pid,
                parent_ino: base_ino,
                inode: Inode::new(base_ino * 10 + 2, FileType::Directory),
            }));
        }
        if name == "maps" {
            return Some(Arc::new(ProcFsProcessMaps {
                target_pid: self.target_pid,
                inode: Inode::new(base_ino * 10 + 3, FileType::Regular),
            }));
        }
        if name == "status" {
            return Some(Arc::new(ProcFsProcessStatus {
                target_pid: self.target_pid,
                inode: Inode::new(base_ino * 10 + 4, FileType::Regular),
            }));
        }
        if name == "cmdline" {
            return Some(Arc::new(ProcFsProcessCmdline {
                target_pid: self.target_pid,
                inode: Inode::new(base_ino * 10 + 5, FileType::Regular),
            }));
        }
        None
    }

    fn readdir(&self) -> Vec<DirEntry> {
        let base_ino = self.inode.ino;
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
                ino: base_ino * 10 + 1,
                file_type: FileType::Symlink,
            },
            DirEntry {
                name: String::from("fd"),
                ino: base_ino * 10 + 2,
                file_type: FileType::Directory,
            },
            DirEntry {
                name: String::from("maps"),
                ino: base_ino * 10 + 3,
                file_type: FileType::Regular,
            },
            DirEntry {
                name: String::from("status"),
                ino: base_ino * 10 + 4,
                file_type: FileType::Regular,
            },
            DirEntry {
                name: String::from("cmdline"),
                ino: base_ino * 10 + 5,
                file_type: FileType::Regular,
            },
        ]
    }
}

/// Special `/proc/self/fd` or `/proc/<pid>/fd` directory.
struct ProcFsProcessFdDir {
    target_pid: Option<Pid>,
    parent_ino: u64,
    inode: Inode,
}

impl InodeOps for ProcFsProcessFdDir {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn lookup(&self, name: &str) -> Option<Arc<dyn InodeOps>> {
        let fd = name.parse::<i32>().ok()?;
        if fd < 0 {
            return None;
        }
        let (task_arc, _) = resolve_target_task(self.target_pid)?;
        let task = task_arc.lock();
        let fd_table = task.fd_table.lock();
        let _ = fd_table.entries.get(fd as usize)?.as_ref()?;
        Some(Arc::new(ProcFsProcessFdLink {
            target_pid: self.target_pid,
            fd,
            inode: Inode::new(self.inode.ino * 100 + fd as u64, FileType::Symlink),
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
                ino: self.parent_ino,
                file_type: FileType::Directory,
            },
        ];

        if let Some((task_arc, _)) = resolve_target_task(self.target_pid) {
            let task = task_arc.lock();
            let fd_table = task.fd_table.lock();
            for (i, entry) in fd_table.entries.iter().enumerate() {
                if entry.is_some() {
                    result.push(DirEntry {
                        name: format!("{}", i),
                        ino: self.inode.ino * 100 + i as u64,
                        file_type: FileType::Symlink,
                    });
                }
            }
        }

        result
    }
}

/// Special `/proc/self/fd/<fd>` symlink.
struct ProcFsProcessFdLink {
    target_pid: Option<Pid>,
    fd: i32,
    inode: Inode,
}

impl InodeOps for ProcFsProcessFdLink {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let (task_arc, _) = resolve_target_task(self.target_pid).ok_or(-3)?; // ESRCH
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

/// Special `/proc/self/exe` or `/proc/<pid>/exe` symlink.
struct ProcFsProcessExe {
    target_pid: Option<Pid>,
    inode: Inode,
}

impl InodeOps for ProcFsProcessExe {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let exe_path = if let Some((task_arc, _)) = resolve_target_task(self.target_pid) {
            task_arc.lock().executable_path.clone()
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

/// Special `/proc/self/maps` or `/proc/<pid>/maps` file.
struct ProcFsProcessMaps {
    target_pid: Option<Pid>,
    inode: Inode,
}

impl InodeOps for ProcFsProcessMaps {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let (task_arc, _) = resolve_target_task(self.target_pid).ok_or(-3)?; // ESRCH
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

/// Special `/proc/self/status` or `/proc/<pid>/status` file.
struct ProcFsProcessStatus {
    target_pid: Option<Pid>,
    inode: Inode,
}

impl InodeOps for ProcFsProcessStatus {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let (task_arc, resolved_pid) = resolve_target_task(self.target_pid).ok_or(-3)?; // ESRCH
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
            name, state, tgid, resolved_pid.as_u64(), ppid, threads, vmsize, vmrss
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

/// Special `/proc/self/cmdline` or `/proc/<pid>/cmdline` file.
struct ProcFsProcessCmdline {
    target_pid: Option<Pid>,
    inode: Inode,
}

impl InodeOps for ProcFsProcessCmdline {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let (task_arc, _) = resolve_target_task(self.target_pid).ok_or(-3)?; // ESRCH

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
