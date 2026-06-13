//! Virtual File System (VFS) for KontsnorOS.
//!
//! The VFS provides a unified interface for all file system operations,
//! implementing the Unix "everything is a file" philosophy.
//!
//! ## Architecture
//!
//! ```text
//! User Space:  open("/dev/sda1", ...)  →  read(fd, buf, 512)
//!                     │                          │
//!                     ▼                          ▼
//! VFS Layer:   path_resolve()           fd_table_lookup()
//!                     │                          │
//!                     ▼                          ▼
//! FS Driver:   ext2_lookup()            ext2_read()
//!                     │                          │
//!                     ▼                          ▼
//! Block Layer: block_device_read()      block_device_read()
//! ```

use crate::kprintln;
pub mod cgroupfs;
pub mod devfs;
pub mod epoll;
pub mod eventfd;
pub mod ext;
pub mod file;
pub mod inode;
pub mod path;
pub mod pipe;
pub mod procfs;
pub mod pty;
pub mod securityfs;
pub mod signalfd;
pub mod sysfs;
pub mod timerfd;
pub mod tmpfs;
pub mod tty;
pub mod vfs;

/// Initialize the Virtual File System.
pub fn init() {
    vfs::init();
    devfs::init();
    tmpfs::init();
    procfs::init();

    // Mount sysfs at /sys
    let sysfs = sysfs::create_sysfs();
    vfs::mount(alloc::string::String::from("/sys"), sysfs);

    // Mount cgroup2 at /sys/fs/cgroup
    let cgroupfs = cgroupfs::create_cgroupfs();
    vfs::mount(alloc::string::String::from("/sys/fs/cgroup"), cgroupfs);

    // Mount securityfs at /sys/kernel/security
    let securityfs = securityfs::create_securityfs();
    vfs::mount(
        alloc::string::String::from("/sys/kernel/security"),
        securityfs,
    );

    // Create the RAM disk pre-populated with our ext2 filesystem
    let ramdisk = crate::drivers::ramdisk::create_ext2_ramdisk();
    crate::fs::vfs::register_block_device(alloc::string::String::from("ramdisk"), ramdisk.clone());

    let mut mounted_ata = false;

    // Probe the physical ATA Primary Slave drive
    if let Some(ata_drive) = crate::drivers::block::ata::init_ata_drive() {
        crate::fs::vfs::register_block_device(
            alloc::string::String::from("ata0"),
            ata_drive.clone(),
        );
        let mut buf = [0u8; 512];
        if ata_drive.read_block(2, &mut buf).is_ok() {
            let magic = u16::from_le_bytes([buf[56], buf[57]]);
            if magic != 0xEF53 {
                kprintln!("[fs] ATA drive is unformatted (magic: {:#X}). Formatting with live ext2 image...", magic);
                let mut success = true;
                for block_idx in 0..256 {
                    let mut block_data = [0u8; 512];
                    if ramdisk
                        .read_block(block_idx as u64, &mut block_data)
                        .is_ok()
                    {
                        if ata_drive
                            .write_block(block_idx as u64, &block_data)
                            .is_err()
                        {
                            kprintln!("[fs] Failed to write block {} to ATA drive.", block_idx);
                            success = false;
                            break;
                        }
                    } else {
                        kprintln!("[fs] Failed to read block {} from RAM disk.", block_idx);
                        success = false;
                        break;
                    }
                }
                if success {
                    if let Err(e) = ata_drive.flush() {
                        kprintln!("[fs] Failed to flush ATA drive: {:?}", e);
                    } else {
                        kprintln!("[fs] ATA drive formatted and flushed successfully.");
                    }
                }
            } else {
                kprintln!("[fs] ATA drive is already formatted (magic: 0xEF53).");
            }
        }

        // Try mounting the physical ATA drive
        let cached_drive = alloc::sync::Arc::new(crate::drivers::block::cache::BlockCache::new(
            ata_drive, 2048,
        ));
        if let Ok(ext_fs) = ext::ExtFileSystem::mount(cached_drive) {
            vfs::mount(alloc::string::String::from("/disk"), ext_fs.clone());
            vfs::mount(alloc::string::String::from("/"), ext_fs);
            kprintln!("[fs] Persistent ext ATA drive mounted at /disk and /.");
            mounted_ata = true;
        } else {
            kprintln!("[fs] Failed to mount ext ATA drive. Falling back to RAM disk.");
        }
    }

    if !mounted_ata {
        // Mount it using the ext driver
        if let Ok(ext_fs) = ext::ExtFileSystem::mount(ramdisk) {
            vfs::mount(alloc::string::String::from("/disk"), ext_fs.clone());
            vfs::mount(alloc::string::String::from("/"), ext_fs);
            kprintln!("[fs] Pre-populated ext RAM disk mounted at /disk and /.");
        } else {
            kprintln!("[fs] Failed to mount ext RAM disk.");
        }
    }

    // Probe and initialize SATA AHCI drives
    let sata_drives = crate::drivers::block::ahci::init();
    for (idx, drive) in sata_drives.into_iter().enumerate() {
        crate::fs::vfs::register_block_device(alloc::format!("sata{}", idx), drive);
    }

    kprintln!("[fs] VFS initialized with devfs, tmpfs, procfs, ext.");
}
