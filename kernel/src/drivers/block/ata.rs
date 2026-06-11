//! Writable IDE/ATA PIO block device driver for KontsnorOS.

use alloc::string::String;
use alloc::sync::Arc;
use x86_64::instructions::port::Port;
use crate::drivers::traits::{BlockDevice, DriverError, DriverInfo};
use crate::kprintln;

/// ATA Primary Slave drive implementation.
pub struct AtaDrive {
    info: DriverInfo,
}

impl AtaDrive {
    /// Polls the status register until the BSY bit is clear and DRDY bit is set.
    fn wait_ready(&self) -> Result<(), &'static str> {
        let mut status_port = Port::<u8>::new(0x1F7);
        for i in 0..1_000_000 {
            let status = unsafe { status_port.read() };
            if (status & 0x80) == 0 && (status & 0x40) != 0 {
                return Ok(());
            }
            if i >= 100 && i % 1000 == 0 {
                crate::process::scheduler::yield_now();
            } else {
                core::hint::spin_loop();
            }
        }
        Err("ATA Drive timeout waiting for ready")
    }

    /// Polls the status register until BSY is clear and DRQ (Data Request) is set.
    fn wait_data_request(&self) -> Result<(), &'static str> {
        let mut status_port = Port::<u8>::new(0x1F7);
        for i in 0..1_000_000 {
            let status = unsafe { status_port.read() };
            if (status & 0x80) == 0 && (status & 0x08) != 0 {
                return Ok(());
            }
            if i >= 100 && i % 1000 == 0 {
                crate::process::scheduler::yield_now();
            } else {
                core::hint::spin_loop();
            }
        }
        Err("ATA Drive timeout waiting for data request (DRQ)")
    }

    /// Tries to introduce a ~400ns delay by reading the status port 4 times.
    fn io_delay(&self) {
        let mut status_port = Port::<u8>::new(0x1F7);
        unsafe {
            status_port.read();
            status_port.read();
            status_port.read();
            status_port.read();
        }
    }

    /// Selects the Primary Slave drive and specifies LBA mode.
    fn select_drive(&self, lba: u32) -> Result<(), &'static str> {
        let mut drive_head_port = Port::<u8>::new(0x1F6);
        self.wait_ready()?;
        // Drive select value: 0xF0 selects Primary Slave and LBA mode
        let val = 0xF0 | ((lba >> 24) & 0x0F) as u8;
        unsafe { drive_head_port.write(val); }
        self.io_delay();
        Ok(())
    }

    /// Configures the LBA registers and sector count.
    fn setup_lba(&self, lba: u32, sector_count: u8) -> Result<(), &'static str> {
        self.select_drive(lba)?;
        
        let mut features_port = Port::<u8>::new(0x1F1);
        let mut sector_count_port = Port::<u8>::new(0x1F2);
        let mut lba_low_port = Port::<u8>::new(0x1F3);
        let mut lba_mid_port = Port::<u8>::new(0x1F4);
        let mut lba_high_port = Port::<u8>::new(0x1F5);
        
        unsafe {
            features_port.write(0);
            sector_count_port.write(sector_count);
            lba_low_port.write((lba & 0xFF) as u8);
            lba_mid_port.write(((lba >> 8) & 0xFF) as u8);
            lba_high_port.write(((lba >> 16) & 0xFF) as u8);
        }
        
        Ok(())
    }

    /// Reads a chunk of consecutive sectors (up to 256) in a single command.
    fn read_sectors_chunk(&self, lba: u32, sector_count: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        let sc_val = if sector_count == 256 { 0 } else { sector_count as u8 };
        self.setup_lba(lba, sc_val)?;
        
        let mut command_port = Port::<u8>::new(0x1F7);
        unsafe { command_port.write(0x20); } // 0x20 = Read Sectors
        
        let mut data_port = Port::<u16>::new(0x1F0);
        for sector in 0..sector_count {
            self.io_delay();
            self.wait_data_request()?;
            
            let sector_offset = (sector * 512) as usize;
            for i in 0..256 {
                let word = unsafe { data_port.read() };
                let bytes = word.to_le_bytes();
                buf[sector_offset + i * 2] = bytes[0];
                buf[sector_offset + i * 2 + 1] = bytes[1];
            }
        }
        
        Ok(())
    }

    /// Writes a chunk of consecutive sectors (up to 256) in a single command.
    fn write_sectors_chunk(&self, lba: u32, sector_count: u32, data: &[u8]) -> Result<(), &'static str> {
        let sc_val = if sector_count == 256 { 0 } else { sector_count as u8 };
        self.setup_lba(lba, sc_val)?;
        
        let mut command_port = Port::<u8>::new(0x1F7);
        unsafe { command_port.write(0x30); } // 0x30 = Write Sectors
        
        let mut data_port = Port::<u16>::new(0x1F0);
        for sector in 0..sector_count {
            self.io_delay();
            self.wait_data_request()?;
            
            let sector_offset = (sector * 512) as usize;
            for i in 0..256 {
                let word = u16::from_le_bytes([
                    data[sector_offset + i * 2],
                    data[sector_offset + i * 2 + 1]
                ]);
                unsafe { data_port.write(word); }
            }
            
            self.io_delay();
            self.wait_ready()?;
        }
        
        Ok(())
    }
}

impl BlockDevice for AtaDrive {
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let mut lba = block as u32;
        let sector_count = (buf.len() / 512) as u32;
        let mut sectors_read = 0;
        while sectors_read < sector_count {
            let chunk = core::cmp::min(sector_count - sectors_read, 256);
            let chunk_len = (chunk * 512) as usize;
            self.read_sectors_chunk(
                lba,
                chunk,
                &mut buf[(sectors_read * 512) as usize .. (sectors_read * 512) as usize + chunk_len]
            ).map_err(|_| DriverError::IoError)?;
            lba += chunk;
            sectors_read += chunk;
        }
        Ok(())
    }

    fn write_block(&self, block: u64, data: &[u8]) -> Result<(), DriverError> {
        let mut lba = block as u32;
        let sector_count = (data.len() / 512) as u32;
        let mut sectors_written = 0;
        while sectors_written < sector_count {
            let chunk = core::cmp::min(sector_count - sectors_written, 256);
            let chunk_len = (chunk * 512) as usize;
            self.write_sectors_chunk(
                lba,
                chunk,
                &data[(sectors_written * 512) as usize .. (sectors_written * 512) as usize + chunk_len]
            ).map_err(|_| DriverError::IoError)?;
            lba += chunk;
            sectors_written += chunk;
        }
        Ok(())
    }

    fn block_size(&self) -> u64 {
        512
    }

    fn block_count(&self) -> u64 {
        131072 // 64 MB raw disk divided by 512 bytes per block = 131072 blocks
    }

    fn flush(&self) -> Result<(), DriverError> {
        let mut command_port = Port::<u8>::new(0x1F7);
        self.wait_ready().map_err(|_| DriverError::Timeout)?;
        unsafe { command_port.write(0xE7); } // 0xE7 = Cache Flush
        self.wait_ready().map_err(|_| DriverError::Timeout)?;
        Ok(())
    }

    fn info(&self) -> DriverInfo {
        self.info.clone()
    }
}

/// Probes the registers to check if the Primary Slave drive is present.
fn probe_ata_drive() -> bool {
    let mut drive_head_port = Port::<u8>::new(0x1F6);
    let mut lba_low_port = Port::<u8>::new(0x1F3);
    let mut lba_mid_port = Port::<u8>::new(0x1F4);
    
    unsafe {
        // Select Primary Slave (0xF0 selects LBA mode & Slave)
        drive_head_port.write(0xF0);
        
        // Write pattern values to check consistency
        lba_low_port.write(0x55);
        lba_mid_port.write(0xAA);
        
        // Read back pattern
        let low = lba_low_port.read();
        let mid = lba_mid_port.read();
        
        low == 0x55 && mid == 0xAA
    }
}

/// Probes and initializes the ATA Primary Slave drive.
pub fn init_ata_drive() -> Option<Arc<dyn BlockDevice>> {
    if probe_ata_drive() {
        kprintln!("[ata] Detected Primary Slave hard disk drive.");
        let info = DriverInfo {
            name: String::from("ata-drive"),
            version: String::from("0.1.0"),
            author: String::from("Antigravity Systems"),
            license: String::from("MIT"),
            description: String::from("Standard IDE/ATA PIO Primary Slave disk driver"),
        };
        crate::drivers::register_driver(info.clone());
        Some(Arc::new(AtaDrive { info }))
    } else {
        kprintln!("[ata] Primary Slave hard disk drive not detected.");
        None
    }
}
