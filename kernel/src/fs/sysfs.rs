//! System filesystem (sysfs) — `/sys`.
//!
//! Exposes kernel objects, attributes, and configuration as virtual files,
//! following standard Linux sysfs hierarchy rules.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::inode::{DirEntry, FileType, Inode, InodeOps};
use super::vfs::FileSystem;

/// The sysfs filesystem.
pub struct SysFs {
    root: Arc<SysFsDir>,
}

impl FileSystem for SysFs {
    fn root(&self) -> Option<Arc<dyn InodeOps>> {
        Some(self.root.clone())
    }

    fn name(&self) -> &str {
        "sysfs"
    }
}

/// A sysfs directory node.
struct SysFsDir {
    inode: Inode,
    entries: Vec<(String, Arc<dyn InodeOps>)>,
}

impl InodeOps for SysFsDir {
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

/// A sysfs dynamic virtual file.
struct SysFsFile {
    inode: Inode,
    generator: fn() -> String,
}

impl InodeOps for SysFsFile {
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

fn gen_cpu_online() -> String {
    let count = crate::arch::x86_64::smp::get_cpu_count();
    if count <= 1 {
        String::from("0\n")
    } else {
        format!("0-{}\n", count - 1)
    }
}

fn gen_lo_address() -> String {
    String::from("00:00:00:00:00:00\n")
}

fn gen_eth0_address() -> String {
    if let Some(mac) = crate::net::interface::get_mac_address("eth0") {
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        )
    } else {
        String::from("52:54:00:12:34:56\n")
    }
}

fn gen_selinux_enforce() -> String {
    String::from("0\n")
}

fn gen_selinux_policyvers() -> String {
    String::from("0\n")
}

/// Initialize sysfs layout.
pub fn create_sysfs() -> Arc<SysFs> {
    // /sys/fs/selinux
    let selinux_entries = vec![
        (
            String::from("enforce"),
            Arc::new(SysFsFile {
                inode: Inode::new(317, FileType::Regular),
                generator: gen_selinux_enforce,
            }) as Arc<dyn InodeOps>,
        ),
        (
            String::from("policyvers"),
            Arc::new(SysFsFile {
                inode: Inode::new(318, FileType::Regular),
                generator: gen_selinux_policyvers,
            }) as Arc<dyn InodeOps>,
        ),
    ];
    let selinux_dir = Arc::new(SysFsDir {
        inode: Inode::new(316, FileType::Directory),
        entries: selinux_entries,
    });

    // /sys/fs/cgroup (mountpoint for cgroup2)
    let cgroup_dir = Arc::new(SysFsDir {
        inode: Inode::new(315, FileType::Directory),
        entries: Vec::new(),
    });

    // /sys/fs
    let fs_entries = vec![
        (String::from("selinux"), selinux_dir as Arc<dyn InodeOps>),
        (String::from("cgroup"), cgroup_dir as Arc<dyn InodeOps>),
    ];
    let fs_dir = Arc::new(SysFsDir {
        inode: Inode::new(314, FileType::Directory),
        entries: fs_entries,
    });

    // /sys/kernel/security (mountpoint for securityfs)
    let security_dir = Arc::new(SysFsDir {
        inode: Inode::new(313, FileType::Directory),
        entries: Vec::new(),
    });

    // /sys/kernel
    let kernel_entries = vec![(String::from("security"), security_dir as Arc<dyn InodeOps>)];
    let kernel_dir = Arc::new(SysFsDir {
        inode: Inode::new(312, FileType::Directory),
        entries: kernel_entries,
    });

    // /sys/class/net/lo/address
    let lo_entries = vec![(
        String::from("address"),
        Arc::new(SysFsFile {
            inode: Inode::new(309, FileType::Regular),
            generator: gen_lo_address,
        }) as Arc<dyn InodeOps>,
    )];
    let lo_dir = Arc::new(SysFsDir {
        inode: Inode::new(308, FileType::Directory),
        entries: lo_entries,
    });

    // /sys/class/net/eth0/address
    let eth0_entries = vec![(
        String::from("address"),
        Arc::new(SysFsFile {
            inode: Inode::new(311, FileType::Regular),
            generator: gen_eth0_address,
        }) as Arc<dyn InodeOps>,
    )];
    let eth0_dir = Arc::new(SysFsDir {
        inode: Inode::new(310, FileType::Directory),
        entries: eth0_entries,
    });

    // /sys/class/net
    let net_entries = vec![
        (String::from("lo"), lo_dir as Arc<dyn InodeOps>),
        (String::from("eth0"), eth0_dir as Arc<dyn InodeOps>),
    ];
    let net_dir = Arc::new(SysFsDir {
        inode: Inode::new(307, FileType::Directory),
        entries: net_entries,
    });

    // /sys/class
    let class_entries = vec![(String::from("net"), net_dir as Arc<dyn InodeOps>)];
    let class_dir = Arc::new(SysFsDir {
        inode: Inode::new(301, FileType::Directory),
        entries: class_entries,
    });

    // /sys/block
    let block_dir = Arc::new(SysFsDir {
        inode: Inode::new(302, FileType::Directory),
        entries: Vec::new(),
    });

    // /sys/devices/system/cpu/online
    let cpu_entries = vec![(
        String::from("online"),
        Arc::new(SysFsFile {
            inode: Inode::new(306, FileType::Regular),
            generator: gen_cpu_online,
        }) as Arc<dyn InodeOps>,
    )];
    let cpu_dir = Arc::new(SysFsDir {
        inode: Inode::new(305, FileType::Directory),
        entries: cpu_entries,
    });

    // /sys/devices/system/cpu
    let system_entries = vec![(String::from("cpu"), cpu_dir as Arc<dyn InodeOps>)];
    let system_dir = Arc::new(SysFsDir {
        inode: Inode::new(304, FileType::Directory),
        entries: system_entries,
    });

    // /sys/devices
    let devices_entries = vec![(String::from("system"), system_dir as Arc<dyn InodeOps>)];
    let devices_dir = Arc::new(SysFsDir {
        inode: Inode::new(303, FileType::Directory),
        entries: devices_entries,
    });

    // /sys root entries
    let root_entries = vec![
        (String::from("class"), class_dir as Arc<dyn InodeOps>),
        (String::from("block"), block_dir as Arc<dyn InodeOps>),
        (String::from("devices"), devices_dir as Arc<dyn InodeOps>),
        (String::from("kernel"), kernel_dir as Arc<dyn InodeOps>),
        (String::from("fs"), fs_dir as Arc<dyn InodeOps>),
    ];

    let root = Arc::new(SysFsDir {
        inode: Inode::new(300, FileType::Directory),
        entries: root_entries,
    });

    Arc::new(SysFs { root })
}
