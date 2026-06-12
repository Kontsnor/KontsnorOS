//! Writable IDE/ATA Bus Master DMA block device driver for KontsnorOS.

use crate::drivers::traits::{BlockDevice, DriverError, DriverInfo};
use crate::kprintln;
use alloc::string::String;
use alloc::sync::Arc;
use spin::Mutex;
use x86_64::instructions::port::Port;

#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
struct PrdEntry {
    phys_addr: u32,
    byte_count_eot: u32, // Lower 16 bits: byte count, Bit 31: End-of-Table
}

struct AtaDriveInner {
    dma_base: Option<u16>,
    prdt_phys: u64,
    prdt_virt: *mut PrdEntry,
}

// SAFETY: The PRD physical and virtual buffers are thread-safe and protected by a Mutex.
unsafe impl Send for AtaDriveInner {}
unsafe impl Sync for AtaDriveInner {}

/// ATA Primary Slave drive implementation.
pub struct AtaDrive {
    info: DriverInfo,
    inner: Mutex<AtaDriveInner>,
}

// SAFETY: AtaDrive implements safe multithreaded serialization.
unsafe impl Send for AtaDrive {}
unsafe impl Sync for AtaDrive {}

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
        unsafe {
            drive_head_port.write(val);
        }
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

    /// Reads a chunk of consecutive sectors (up to 256) in a single command using PIO.
    fn read_sectors_chunk(
        &self,
        lba: u32,
        sector_count: u32,
        buf: &mut [u8],
    ) -> Result<(), &'static str> {
        let sc_val = if sector_count == 256 {
            0
        } else {
            sector_count as u8
        };
        self.setup_lba(lba, sc_val)?;

        let mut command_port = Port::<u8>::new(0x1F7);
        unsafe {
            command_port.write(0x20);
        } // 0x20 = Read Sectors

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

    /// Writes a chunk of consecutive sectors (up to 256) in a single command using PIO.
    fn write_sectors_chunk(
        &self,
        lba: u32,
        sector_count: u32,
        data: &[u8],
    ) -> Result<(), &'static str> {
        let sc_val = if sector_count == 256 {
            0
        } else {
            sector_count as u8
        };
        self.setup_lba(lba, sc_val)?;

        let mut command_port = Port::<u8>::new(0x1F7);
        unsafe {
            command_port.write(0x30);
        } // 0x30 = Write Sectors

        let mut data_port = Port::<u16>::new(0x1F0);
        for sector in 0..sector_count {
            self.io_delay();
            self.wait_data_request()?;

            let sector_offset = (sector * 512) as usize;
            for i in 0..256 {
                let word = u16::from_le_bytes([
                    data[sector_offset + i * 2],
                    data[sector_offset + i * 2 + 1],
                ]);
                unsafe {
                    data_port.write(word);
                }
            }

            self.io_delay();
            self.wait_ready()?;
        }

        Ok(())
    }

    /// Performs Bus Master DMA read/write transfer
    fn dma_transfer(
        &self,
        prdt_phys: u64,
        prdt_virt: *mut PrdEntry,
        dma_base: u16,
        lba: u32,
        sector_count: u32,
        buf: &[u8],
        is_read: bool,
    ) -> Result<(), &'static str> {
        use x86_64::VirtAddr;

        // 1. Prepare the PRDT entries
        let mut virt_addr = buf.as_ptr() as u64;
        let mut remaining = buf.len();
        let mut entry_idx = 0;

        while remaining > 0 {
            let phys_addr = crate::memory::r#virtual::translate_addr(VirtAddr::new(virt_addr))
                .ok_or("ATA DMA: address translation failed")?
                .as_u64();

            let page_offset = phys_addr & 0xFFF;
            let page_remaining = 4096 - page_offset;
            let chunk_size = core::cmp::min(remaining, page_remaining as usize);

            if phys_addr > u32::MAX as u64 {
                return Err("ATA DMA: physical address exceeds 32-bit");
            }

            unsafe {
                let entry = &mut *prdt_virt.add(entry_idx);
                entry.phys_addr = phys_addr as u32;
                entry.byte_count_eot = chunk_size as u32;
                entry_idx += 1;
            }

            virt_addr += chunk_size as u64;
            remaining -= chunk_size;
        }

        if entry_idx == 0 {
            return Err("ATA DMA: empty buffer");
        }

        // Set End-of-Table (EOT) flag on the last descriptor
        unsafe {
            (*prdt_virt.add(entry_idx - 1)).byte_count_eot |= 0x8000_0000;
        }

        // 2. Stop DMA engine just in case, clear status flags
        let mut cmd_port = Port::<u8>::new(dma_base + 0x00);
        let mut prdt_port = Port::<u32>::new(dma_base + 0x04);
        let mut status_port = Port::<u8>::new(dma_base + 0x02);

        unsafe {
            cmd_port.write(0);
            let status = status_port.read();
            status_port.write(status | 0x06); // Clear Interrupt and Error flags
        }

        // 3. Write physical address of PRDT to the PRD Table Address Register
        unsafe {
            prdt_port.write(prdt_phys as u32);
        }

        // 4. Set direction: Bit 3 is 1 for Read (Device -> Memory), 0 for Write (Memory -> Device)
        let direction_bit = if is_read { 0x08 } else { 0x00 };
        unsafe {
            cmd_port.write(direction_bit);
        }

        // 5. Setup LBA registers on the command ports
        let sc_val = if sector_count == 256 {
            0
        } else {
            sector_count as u8
        };
        self.setup_lba(lba, sc_val)?;

        // 6. Issue DMA Command to Command Port 0x1F7 (0xC8 = Read DMA, 0xCA = Write DMA)
        let ata_cmd = if is_read { 0xC8 } else { 0xCA };
        let mut command_port = Port::<u8>::new(0x1F7);
        unsafe {
            command_port.write(ata_cmd);
        }

        // 7. Start DMA engine
        unsafe {
            cmd_port.write(direction_bit | 0x01);
        }

        // 8. Poll Bus Master Status register for completion
        let mut success = false;
        for i in 0..1_000_000 {
            let status = unsafe { status_port.read() };
            // Bit 2: Interrupt, Bit 1: Error, Bit 0: Active
            if (status & 0x04) != 0 {
                success = (status & 0x02) == 0;
                break;
            }
            if (status & 0x01) == 0 {
                success = (status & 0x02) == 0;
                break;
            }

            if i >= 100 && i % 1000 == 0 {
                crate::process::scheduler::yield_now();
            } else {
                core::hint::spin_loop();
            }
        }

        // 9. Stop DMA engine
        unsafe {
            cmd_port.write(direction_bit);
            let status = status_port.read();
            status_port.write(status | 0x06); // Clear Interrupt and Error again
            if !success || (status & 0x02) != 0 {
                return Err("ATA DMA: transfer failed or timed out");
            }
        }

        Ok(())
    }
}

impl BlockDevice for AtaDrive {
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let mut inner = self.inner.lock();
        let mut lba = block as u32;
        let sector_count = (buf.len() / 512) as u32;
        let mut sectors_read = 0;

        while sectors_read < sector_count {
            let chunk = core::cmp::min(sector_count - sectors_read, 256);
            let chunk_len = (chunk * 512) as usize;
            let sub_buf =
                &mut buf[(sectors_read * 512) as usize..(sectors_read * 512) as usize + chunk_len];

            let mut dma_ok = false;
            if let Some(dma_base) = inner.dma_base {
                if self
                    .dma_transfer(
                        inner.prdt_phys,
                        inner.prdt_virt,
                        dma_base,
                        lba,
                        chunk,
                        sub_buf,
                        true,
                    )
                    .is_ok()
                {
                    dma_ok = true;
                }
            }

            if !dma_ok {
                self.read_sectors_chunk(lba, chunk, sub_buf)
                    .map_err(|_| DriverError::IoError)?;
            }

            lba += chunk;
            sectors_read += chunk;
        }
        Ok(())
    }

    fn write_block(&self, block: u64, data: &[u8]) -> Result<(), DriverError> {
        let mut inner = self.inner.lock();
        let mut lba = block as u32;
        let sector_count = (data.len() / 512) as u32;
        let mut sectors_written = 0;

        while sectors_written < sector_count {
            let chunk = core::cmp::min(sector_count - sectors_written, 256);
            let chunk_len = (chunk * 512) as usize;
            let sub_data = &data
                [(sectors_written * 512) as usize..(sectors_written * 512) as usize + chunk_len];

            let mut dma_ok = false;
            if let Some(dma_base) = inner.dma_base {
                if self
                    .dma_transfer(
                        inner.prdt_phys,
                        inner.prdt_virt,
                        dma_base,
                        lba,
                        chunk,
                        sub_data,
                        false,
                    )
                    .is_ok()
                {
                    dma_ok = true;
                }
            }

            if !dma_ok {
                self.write_sectors_chunk(lba, chunk, sub_data)
                    .map_err(|_| DriverError::IoError)?;
            }

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
        let _inner = self.inner.lock();
        let mut command_port = Port::<u8>::new(0x1F7);
        self.wait_ready().map_err(|_| DriverError::Timeout)?;
        unsafe {
            command_port.write(0xE7);
        } // 0xE7 = Cache Flush
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

fn find_ata_pci_dma() -> Option<u16> {
    // Make sure PCI is initialized
    crate::drivers::bus::pci::init();

    let devices = crate::drivers::bus::pci::find_device(0x8086, 0x7010);
    if devices.is_empty() {
        kprintln!("[ata] PCI IDE Controller (8086:7010) not found.");
        return None;
    }

    let dev = &devices[0];
    let bar4 = crate::drivers::bus::pci::read_config(dev.bus, dev.device, dev.function, 0x20);

    if bar4 == 0 || bar4 == 0xFFFFFFFF {
        kprintln!("[ata] PCI IDE Controller BAR4 is invalid ({:#x}).", bar4);
        return None;
    }

    if bar4 & 1 == 0 {
        kprintln!("[ata] PCI IDE Controller BAR4 is not an I/O space BAR.");
        return None;
    }

    let base_addr = (bar4 & 0xFFFFFFFC) as u16;
    kprintln!(
        "[ata] Found PCI IDE Controller. BAR4: {:#x}, DMA I/O Base: {:#x}",
        bar4,
        base_addr
    );

    // Enable I/O Space (bit 0) and Bus Master (bit 2) in Command Register
    let cmd = crate::drivers::bus::pci::read_config(dev.bus, dev.device, dev.function, 0x04);
    crate::drivers::bus::pci::write_config(dev.bus, dev.device, dev.function, 0x04, cmd | 0x05);
    kprintln!("[ata] Enabled PCI Bus Master & I/O space on IDE Controller.");

    Some(base_addr)
}

/// Probes and initializes the ATA Primary Slave drive.
pub fn init_ata_drive() -> Option<Arc<dyn BlockDevice>> {
    if probe_ata_drive() {
        kprintln!("[ata] Detected Primary Slave hard disk drive.");
        let dma_base = find_ata_pci_dma();

        let prdt_phys =
            crate::memory::physical::allocate_frame().expect("ata: out of memory for PRDT");
        let prdt_virt = (prdt_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut PrdEntry;

        unsafe {
            core::ptr::write_bytes(prdt_virt as *mut u8, 0, 4096);
        }

        let info = DriverInfo {
            name: String::from("ata-drive"),
            version: String::from("0.1.0"),
            author: String::from("Antigravity Systems"),
            license: String::from("MIT"),
            description: String::from(
                "Standard IDE/ATA Primary Slave disk driver with Bus Master DMA",
            ),
        };
        crate::drivers::register_driver(info.clone());
        Some(Arc::new(AtaDrive {
            info,
            inner: Mutex::new(AtaDriveInner {
                dma_base,
                prdt_phys,
                prdt_virt,
            }),
        }))
    } else {
        kprintln!("[ata] Primary Slave hard disk drive not detected.");
        None
    }
}
