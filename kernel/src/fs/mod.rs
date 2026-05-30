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
pub mod devfs;
pub mod ext2;
pub mod file;
pub mod inode;
pub mod path;
pub mod pipe;
pub mod procfs;
pub mod tmpfs;
pub mod tty;
pub mod vfs;

/// Initialize the Virtual File System.
pub fn init() {
    vfs::init();
    devfs::init();
    tmpfs::init();
    procfs::init();

    // Create the RAM disk pre-populated with our ext2 filesystem
    let ramdisk = crate::drivers::ramdisk::create_ext2_ramdisk();
    
    // Mount it using the ext2 driver
    if let Ok(ext2_fs) = ext2::Ext2FileSystem::mount(ramdisk) {
        vfs::mount(alloc::string::String::from("/disk"), ext2_fs.clone());
        vfs::mount(alloc::string::String::from("/"), ext2_fs);
        kprintln!("[fs] Pre-populated ext2 RAM disk mounted at /disk and /.");
    } else {
        kprintln!("[fs] Failed to mount ext2 RAM disk.");
    }

    kprintln!("[fs] VFS initialized with devfs, tmpfs, procfs, ext2.");
}
