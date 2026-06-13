//! PCI AHCI (SATA) Block Storage Controller Driver for KontsnorOS.

use crate::drivers::traits::{BlockDevice, DriverError, DriverInfo};
use crate::kprintln;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

// AHCI Generic Host Control Register Offsets
pub const HOST_CAP: u32 = 0x00;
pub const HOST_GHC: u32 = 0x04;
pub const HOST_IS: u32 = 0x08;
pub const HOST_PI: u32 = 0x0C;
pub const HOST_VS: u32 = 0x10;

// AHCI Port Register Offsets (relative to port base)
pub const PORT_CLB: u32 = 0x00;
pub const PORT_CLBU: u32 = 0x04;
pub const PORT_FB: u32 = 0x08;
pub const PORT_FBU: u32 = 0x0C;
pub const PORT_IS: u32 = 0x10;
pub const PORT_IE: u32 = 0x14;
pub const PORT_CMD: u32 = 0x18;
pub const PORT_TFD: u32 = 0x20;
pub const PORT_SIG: u32 = 0x24;
pub const PORT_SSTS: u32 = 0x28;
pub const PORT_SCTL: u32 = 0x2C;
pub const PORT_SERR: u32 = 0x30;
pub const PORT_SACT: u32 = 0x34;
pub const PORT_CI: u32 = 0x38;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct PrdEntry {
    dba: u32,
    dbau: u32,
    reserved: u32,
    dbc: u32, // Bits 0-21: byte count - 1, Bit 31: Interrupt on Completion (IOC)
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct CommandHeader {
    opts: u16,  // CFL (0-4), A (5), W (6), P (7), R (8), B (9), C (10), PMP (12-15)
    prdtl: u16, // PRD Table Length
    prdbc: u32, // PRD Byte Count Transferred
    ctba: u32,  // Command Table Base Address
    ctbau: u32, // Command Table Base Address Upper
    reserved: [u32; 4],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct FisRegH2d {
    fis_type: u8,     // 0x27
    pm_port_c: u8,    // PM Port (0-3), C (Command) bit (7)
    command: u8,      // Command register
    features_low: u8, // Features register low
    lba0: u8,         // LBA register
    lba1: u8,
    lba2: u8,
    device: u8, // Device register (0x40 for LBA mode)
    lba3: u8,   // LBA register upper
    lba4: u8,
    lba5: u8,
    features_high: u8, // Features register high
    count_low: u8,     // Sector count low
    count_high: u8,    // Sector count high
    icc: u8,           // Isochronous command completion
    control: u8,       // Control register
    reserved: [u8; 4],
}

struct AhciPort {
    port_idx: usize,
    virt_base: u64,
    cl_phys: u64,
    cl_virt: *mut u8,
    fis_phys: u64,
    fis_virt: *mut u8,
    ct_phys: u64,
    ct_virt: *mut u8,
    block_count: u64,
}

// SAFETY: AhciPort has raw pointers pointing to dedicated kernel-allocated DMA pages, which are protected by mutex wrappers.
unsafe impl Send for AhciPort {}
// SAFETY: AhciPort has raw pointers pointing to dedicated kernel-allocated DMA pages, which are protected by mutex wrappers.
unsafe impl Sync for AhciPort {}

impl AhciPort {
    fn read_reg(&self, reg: u32) -> u32 {
        let offset = 0x100 + (self.port_idx as u32 * 0x80) + reg;
        let ptr = (self.virt_base + offset as u64) as *const u32;
        // SAFETY: The MMIO register area is mapped with caching disabled, and access is serialized.
        unsafe { ptr.read_volatile() }
    }

    fn write_reg(&self, reg: u32, val: u32) {
        let offset = 0x100 + (self.port_idx as u32 * 0x80) + reg;
        let ptr = (self.virt_base + offset as u64) as *mut u32;
        // SAFETY: The MMIO register area is mapped with caching disabled, and access is serialized.
        unsafe { ptr.write_volatile(val) }
    }

    fn identify(&self) -> Result<u64, DriverError> {
        // Wait for BSY and DRQ to clear
        let mut timeout = 0;
        while (self.read_reg(PORT_TFD) & (0x80 | 0x08)) != 0 {
            if timeout > 1_000_000 {
                return Err(DriverError::Timeout);
            }
            core::hint::spin_loop();
            timeout += 1;
        }

        // Clear Command Table
        // SAFETY: self.ct_virt points to a valid physical frame allocated for Command Table.
        unsafe {
            core::ptr::write_bytes(self.ct_virt, 0, 1024);
        }

        // Setup PRD at offset 128 in command table page.
        // We will read the identify data into ct_virt + 1024, which corresponds to physical address ct_phys + 1024.
        let data_phys = self.ct_phys + 1024;
        let data_virt = (self.ct_virt as u64 + 1024) as *mut u8;

        let prdt = (self.ct_virt as u64 + 128) as *mut PrdEntry;
        // SAFETY: self.ct_virt is mapped and points to our allocated PRDT space.
        unsafe {
            let entry = &mut *prdt;
            entry.dba = data_phys as u32;
            entry.dbau = (data_phys >> 32) as u32;
            entry.reserved = 0;
            entry.dbc = (512 - 1) | (1 << 31); // 512 bytes, Interrupt on Completion set
        }

        // Setup Command Header at index 0 of command list page
        let cmd_hdr = self.cl_virt as *mut CommandHeader;
        // SAFETY: self.cl_virt points to the valid command list page.
        unsafe {
            (*cmd_hdr).opts = 5 & 0x1F; // CFL = 5 dwords (20 bytes for FIS), W = 0 (Read)
            (*cmd_hdr).prdtl = 1;
            (*cmd_hdr).prdbc = 0;
            (*cmd_hdr).ctba = self.ct_phys as u32;
            (*cmd_hdr).ctbau = (self.ct_phys >> 32) as u32;
        }

        // Construct H2D FIS in Command Table
        let fis = self.ct_virt as *mut FisRegH2d;
        // SAFETY: self.ct_virt points to the valid Command Table.
        unsafe {
            (*fis).fis_type = 0x27; // Register FIS - Host to Device
            (*fis).pm_port_c = 1 << 7; // C = 1 (Command)
            (*fis).command = 0xEC; // IDENTIFY DEVICE
            (*fis).device = 0;
        }

        // Issue command
        self.write_reg(PORT_CI, 1);

        // Wait for completion
        let mut timeout = 0;
        loop {
            let ci = self.read_reg(PORT_CI);
            if (ci & 1) == 0 {
                break;
            }
            if timeout > 1_000_000 {
                return Err(DriverError::Timeout);
            }
            core::hint::spin_loop();
            timeout += 1;
        }

        // Check for error
        let tfd = self.read_reg(PORT_TFD);
        if (tfd & 0x01) != 0 {
            return Err(DriverError::IoError);
        }

        // Read sector count from buffer at ct_virt + 1024
        // Word 60-61 (offset 120-123) is total sectors for LBA28
        // Word 100-103 (offset 200-207) is total sectors for LBA48
        // SAFETY: data_virt is offset within a valid kernel-mapped page.
        let (lba28_sectors, lba48_sectors) = unsafe {
            let lba28 = u32::from_le_bytes([
                *data_virt.add(120),
                *data_virt.add(121),
                *data_virt.add(122),
                *data_virt.add(123),
            ]) as u64;

            let lba48 = u64::from_le_bytes([
                *data_virt.add(200),
                *data_virt.add(201),
                *data_virt.add(202),
                *data_virt.add(203),
                *data_virt.add(204),
                *data_virt.add(205),
                *data_virt.add(206),
                *data_virt.add(207),
            ]);
            (lba28, lba48)
        };

        if lba48_sectors > 0 {
            Ok(lba48_sectors)
        } else if lba28_sectors > 0 {
            Ok(lba28_sectors)
        } else {
            Ok(131072) // Fallback to 64 MB
        }
    }

    fn dma_transfer(
        &self,
        lba: u64,
        sector_count: u32,
        buf: &[u8],
        is_write: bool,
    ) -> Result<(), DriverError> {
        use x86_64::VirtAddr;

        // 1. Wait for port to be free
        let mut timeout = 0;
        while (self.read_reg(PORT_TFD) & (0x80 | 0x08)) != 0 {
            if timeout > 1_000_000 {
                return Err(DriverError::Timeout);
            }
            core::hint::spin_loop();
            timeout += 1;
        }

        // 2. Populate PRD entries
        let mut virt_addr = buf.as_ptr() as u64;
        let mut remaining = buf.len();
        let mut entry_idx = 0;
        let prdt = (self.ct_virt as u64 + 128) as *mut PrdEntry;

        while remaining > 0 {
            let phys_addr = crate::memory::r#virtual::translate_addr(VirtAddr::new(virt_addr))
                .ok_or(DriverError::IoError)?
                .as_u64();

            let page_offset = phys_addr & 0xFFF;
            let page_remaining = 4096 - page_offset;
            let chunk_size = core::cmp::min(remaining, page_remaining as usize);

            // SAFETY: prdt is within the command table page allocated for this port.
            unsafe {
                let entry = &mut *prdt.add(entry_idx);
                entry.dba = phys_addr as u32;
                entry.dbau = (phys_addr >> 32) as u32;
                entry.reserved = 0;
                entry.dbc = (chunk_size as u32 - 1) | (1 << 31); // Set Interrupt on Completion
            }

            entry_idx += 1;
            virt_addr += chunk_size as u64;
            remaining -= chunk_size;
        }

        if entry_idx == 0 {
            return Err(DriverError::InvalidParam);
        }

        // 3. Setup Command Header
        let cmd_hdr = self.cl_virt as *mut CommandHeader;
        // SAFETY: self.cl_virt is the valid command list page.
        unsafe {
            (*cmd_hdr).opts = (5 & 0x1F) | (if is_write { 1 << 6 } else { 0 }); // CFL = 5, W
            (*cmd_hdr).prdtl = entry_idx as u16;
            (*cmd_hdr).prdbc = 0;
            (*cmd_hdr).ctba = self.ct_phys as u32;
            (*cmd_hdr).ctbau = (self.ct_phys >> 32) as u32;
        }

        // 4. Construct H2D FIS
        // SAFETY: self.ct_virt is the valid command table page.
        unsafe {
            core::ptr::write_bytes(self.ct_virt, 0, 128); // Clear FIS area
        }
        let fis = self.ct_virt as *mut FisRegH2d;
        // SAFETY: self.ct_virt points to the valid Command Table.
        unsafe {
            (*fis).fis_type = 0x27; // H2D
            (*fis).pm_port_c = 1 << 7; // Command
            (*fis).command = if is_write { 0x35 } else { 0x25 }; // WRITE_DMA_EXT / READ_DMA_EXT
            (*fis).lba0 = (lba & 0xFF) as u8;
            (*fis).lba1 = ((lba >> 8) & 0xFF) as u8;
            (*fis).lba2 = ((lba >> 16) & 0xFF) as u8;
            (*fis).device = 0x40; // LBA mode
            (*fis).lba3 = ((lba >> 24) & 0xFF) as u8;
            (*fis).lba4 = ((lba >> 32) & 0xFF) as u8;
            (*fis).lba5 = ((lba >> 40) & 0xFF) as u8;
            (*fis).count_low = (sector_count & 0xFF) as u8;
            (*fis).count_high = ((sector_count >> 8) & 0xFF) as u8;
        }

        // 5. Issue command
        self.write_reg(PORT_CI, 1);

        // 6. Wait for completion
        let mut timeout = 0;
        loop {
            let ci = self.read_reg(PORT_CI);
            if (ci & 1) == 0 {
                break;
            }
            if timeout > 1_000_000 {
                return Err(DriverError::Timeout);
            }
            core::hint::spin_loop();
            timeout += 1;
        }

        // 7. Check for error
        let tfd = self.read_reg(PORT_TFD);
        if (tfd & 0x01) != 0 {
            return Err(DriverError::IoError);
        }

        Ok(())
    }

    fn flush(&self) -> Result<(), DriverError> {
        // Wait for port to be free
        let mut timeout = 0;
        while (self.read_reg(PORT_TFD) & (0x80 | 0x08)) != 0 {
            if timeout > 1_000_000 {
                return Err(DriverError::Timeout);
            }
            core::hint::spin_loop();
            timeout += 1;
        }

        // Setup Command Header
        let cmd_hdr = self.cl_virt as *mut CommandHeader;
        // SAFETY: self.cl_virt is the valid command list page.
        unsafe {
            (*cmd_hdr).opts = 5 & 0x1F; // CFL = 5, W = 0
            (*cmd_hdr).prdtl = 0;
            (*cmd_hdr).prdbc = 0;
            (*cmd_hdr).ctba = self.ct_phys as u32;
            (*cmd_hdr).ctbau = (self.ct_phys >> 32) as u32;
        }

        // Construct H2D FIS
        // SAFETY: self.ct_virt is the valid command table page.
        unsafe {
            core::ptr::write_bytes(self.ct_virt, 0, 128); // Clear FIS area
        }
        let fis = self.ct_virt as *mut FisRegH2d;
        // SAFETY: self.ct_virt points to the valid Command Table.
        unsafe {
            (*fis).fis_type = 0x27; // H2D
            (*fis).pm_port_c = 1 << 7; // Command
            (*fis).command = 0xEA; // FLUSH CACHE EXT
            (*fis).device = 0x40; // LBA mode
        }

        // Issue command
        self.write_reg(PORT_CI, 1);

        // Wait for completion
        let mut timeout = 0;
        loop {
            let ci = self.read_reg(PORT_CI);
            if (ci & 1) == 0 {
                break;
            }
            if timeout > 1_000_000 {
                return Err(DriverError::Timeout);
            }
            core::hint::spin_loop();
            timeout += 1;
        }

        // Check for error
        let tfd = self.read_reg(PORT_TFD);
        if (tfd & 0x01) != 0 {
            return Err(DriverError::IoError);
        }

        Ok(())
    }
}

pub struct SataDrive {
    port: Mutex<AhciPort>,
    info: DriverInfo,
}

// SAFETY: SataDrive has all raw pointers inside AhciPort synchronized by Mutex.
unsafe impl Send for SataDrive {}
// SAFETY: SataDrive has all raw pointers inside AhciPort synchronized by Mutex.
unsafe impl Sync for SataDrive {}

impl BlockDevice for SataDrive {
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let port = self.port.lock();
        let sector_count = (buf.len() / 512) as u32;
        port.dma_transfer(block, sector_count, buf, false)
    }

    fn write_block(&self, block: u64, data: &[u8]) -> Result<(), DriverError> {
        let port = self.port.lock();
        let sector_count = (data.len() / 512) as u32;
        port.dma_transfer(block, sector_count, data, true)
    }

    fn block_size(&self) -> u64 {
        512
    }

    fn block_count(&self) -> u64 {
        self.port.lock().block_count
    }

    fn flush(&self) -> Result<(), DriverError> {
        let port = self.port.lock();
        port.flush()
    }

    fn info(&self) -> DriverInfo {
        self.info.clone()
    }
}

/// Helper module containing exposed initializers for memory-based mock register validation in unit tests.
#[cfg(any(test, feature = "test"))]
pub mod test_helpers {
    use super::*;

    /// Simulates AHCI Controller AE enablement, HR reset logic, and PI configuration.
    pub unsafe fn init_controller_at(virt_base: u64) -> u32 {
        let ghc_addr = (virt_base + HOST_GHC as u64) as *mut u32;
        // SAFETY: Accesses the caller-provided virtual register space.
        unsafe {
            ghc_addr.write_volatile(ghc_addr.read_volatile() | (1 << 31)); // AE
            ghc_addr.write_volatile(ghc_addr.read_volatile() | 1); // HR
        }

        let mut timeout = 0;
        // SAFETY: Accesses the caller-provided virtual register space.
        while unsafe { ghc_addr.read_volatile() & 1 } != 0 {
            if timeout > 1000 {
                break;
            }
            timeout += 1;
        }

        // SAFETY: Accesses the caller-provided virtual register space.
        unsafe {
            ghc_addr.write_volatile(ghc_addr.read_volatile() | (1 << 31) | (1 << 1));
            // AE + IE
        }

        let pi_addr = (virt_base + HOST_PI as u64) as *const u32;
        // SAFETY: Accesses the caller-provided virtual register space.
        unsafe { pi_addr.read_volatile() }
    }

    /// Simulates AHCI Port initialization (CL, FIS addresses, and CR/FR transition).
    pub unsafe fn init_port_at(virt_base: u64, port_idx: usize, cl_phys: u64, fis_phys: u64) {
        let port_base = 0x100 + port_idx as u64 * 0x80;
        let cmd_addr = (virt_base + port_base + PORT_CMD as u64) as *mut u32;
        let clb_addr = (virt_base + port_base + PORT_CLB as u64) as *mut u32;
        let clbu_addr = (virt_base + port_base + PORT_CLBU as u64) as *mut u32;
        let fb_addr = (virt_base + port_base + PORT_FB as u64) as *mut u32;
        let fbu_addr = (virt_base + port_base + PORT_FBU as u64) as *mut u32;

        // Stop port engines
        // SAFETY: Accesses the caller-provided virtual register space.
        unsafe {
            let mut cmd = cmd_addr.read_volatile();
            cmd &= !0x0001; // Clear ST
            cmd &= !0x0010; // Clear FRE
            cmd_addr.write_volatile(cmd);
        }

        // Write base addresses
        // SAFETY: Accesses the caller-provided virtual register space.
        unsafe {
            clb_addr.write_volatile(cl_phys as u32);
            clbu_addr.write_volatile((cl_phys >> 32) as u32);
            fb_addr.write_volatile(fis_phys as u32);
            fbu_addr.write_volatile((fis_phys >> 32) as u32);
        }

        // Clear interrupts
        let is_addr = (virt_base + port_base + PORT_IS as u64) as *mut u32;
        // SAFETY: Accesses the caller-provided virtual register space.
        unsafe {
            is_addr.write_volatile(is_addr.read_volatile());
        }

        // Start engines
        // SAFETY: Accesses the caller-provided virtual register space.
        unsafe {
            let mut cmd = cmd_addr.read_volatile();
            cmd |= 0x0010; // FRE
            cmd |= 0x0001; // ST
            cmd_addr.write_volatile(cmd);
        }
    }
}

/// Detects SATA controller, maps registers, configures the AHCI host, and initializes connected SATA drives.
pub fn init() -> Vec<Arc<dyn BlockDevice>> {
    let devices = crate::drivers::bus::pci::find_by_class(0x01, 0x06);
    if devices.is_empty() {
        kprintln!("[ahci] No SATA Controller found on PCI bus.");
        return Vec::new();
    }

    let mut drives = Vec::new();

    for dev in &devices {
        // Match only AHCI interface (prog_if == 1)
        if dev.prog_if != 0x01 {
            continue;
        }

        kprintln!(
            "[ahci] Found SATA AHCI Controller at [{:02x}:{:02x}.{:01x}]",
            dev.bus,
            dev.device,
            dev.function
        );

        // Enable memory space and bus master in PCI Command register
        let cmd = crate::drivers::bus::pci::read_config(dev.bus, dev.device, dev.function, 0x04);
        crate::drivers::bus::pci::write_config(dev.bus, dev.device, dev.function, 0x04, cmd | 0x06);

        // Get BAR5
        let bar5 = crate::drivers::bus::pci::read_config(dev.bus, dev.device, dev.function, 0x24);
        let base_phys = (bar5 & 0xFFFFFFF0) as u64;

        if base_phys == 0 || base_phys == 0xFFFFFFF0 {
            kprintln!("[ahci] BAR5 is invalid ({:#x})", bar5);
            continue;
        }

        kprintln!("[ahci] BAR5 physical base address: {:#x}", base_phys);

        // Map registers to a dedicated MMIO virtual address range
        let virt_base = 0xffff_d000_0000_0000u64;
        let page_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_CACHE
            | PageTableFlags::NO_EXECUTE;
        let num_pages = 8;
        for i in 0..num_pages {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt_base + i * 4096));
            let frame =
                PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(base_phys + i * 4096));
            // SAFETY: We explicitly map the AHCI MMIO register base address to a dedicated higher-half kernel virtual address space with NO_CACHE.
            unsafe {
                crate::memory::r#virtual::map_page(page, frame, page_flags)
                    .expect("Failed to map physical AHCI register page");
            }
        }

        // Initialize AHCI Host Controller
        let ghc_addr = (virt_base + HOST_GHC as u64) as *mut u32;
        // SAFETY: Registers are mapped and accessed volatile to set the AHCI Enable flag.
        unsafe {
            ghc_addr.write_volatile(ghc_addr.read_volatile() | (1 << 31));
        }

        // Reset controller (HR = bit 0)
        // SAFETY: Registers are mapped and accessed volatile to issue a reset.
        unsafe {
            ghc_addr.write_volatile(ghc_addr.read_volatile() | 1);
        }
        let mut timeout = 0;
        // SAFETY: Registers are mapped and accessed volatile.
        while unsafe { ghc_addr.read_volatile() & 1 } != 0 {
            if timeout > 1_000_000 {
                kprintln!("[ahci] Controller reset timed out!");
                break;
            }
            core::hint::spin_loop();
            timeout += 1;
        }

        // Re-enable AHCI and enable global interrupts
        // SAFETY: Registers are mapped and accessed volatile.
        unsafe {
            ghc_addr.write_volatile(ghc_addr.read_volatile() | (1 << 31) | (1 << 1));
        }

        // Read Ports Implemented (PI) bitmask
        let pi_addr = (virt_base + HOST_PI as u64) as *const u32;
        // SAFETY: Registers are mapped and accessed volatile.
        let pi = unsafe { pi_addr.read_volatile() };
        kprintln!("[ahci] Ports implemented mask: {:#b}", pi);

        // Initialize active SATA ports
        for i in 0..32 {
            if (pi & (1 << i)) != 0 {
                let port_base = 0x100 + i * 0x80;
                let ssts_addr = (virt_base + port_base as u64 + PORT_SSTS as u64) as *const u32;
                // SAFETY: Registers are mapped and accessed volatile.
                let ssts = unsafe { ssts_addr.read_volatile() };
                let det = ssts & 0x0F;

                if det == 3 {
                    kprintln!("[ahci] Active SATA drive detected on port {}", i);

                    // Allocate physical memory pages for command list, received FIS, and command table
                    let cl_phys = crate::memory::physical::allocate_frame()
                        .expect("AHCI: out of physical frames for command list");
                    let cl_virt =
                        (cl_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
                    // SAFETY: cl_virt points to a valid physical frame allocated for command list.
                    unsafe {
                        core::ptr::write_bytes(cl_virt, 0, 4096);
                    }

                    let fis_phys = crate::memory::physical::allocate_frame()
                        .expect("AHCI: out of physical frames for received FIS");
                    let fis_virt =
                        (fis_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
                    // SAFETY: fis_virt points to a valid physical frame allocated for FIS.
                    unsafe {
                        core::ptr::write_bytes(fis_virt, 0, 4096);
                    }

                    let ct_phys = crate::memory::physical::allocate_frame()
                        .expect("AHCI: out of physical frames for command table");
                    let ct_virt =
                        (ct_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
                    // SAFETY: ct_virt points to a valid physical frame allocated for command table.
                    unsafe {
                        core::ptr::write_bytes(ct_virt, 0, 4096);
                    }

                    // Map registers
                    let clb_addr = (virt_base + port_base as u64 + PORT_CLB as u64) as *mut u32;
                    let clbu_addr = (virt_base + port_base as u64 + PORT_CLBU as u64) as *mut u32;
                    let fb_addr = (virt_base + port_base as u64 + PORT_FB as u64) as *mut u32;
                    let fbu_addr = (virt_base + port_base as u64 + PORT_FBU as u64) as *mut u32;
                    let cmd_addr = (virt_base + port_base as u64 + PORT_CMD as u64) as *mut u32;

                    // Stop port engines first
                    // SAFETY: Registers are mapped and accessed volatile.
                    unsafe {
                        let mut cmd = cmd_addr.read_volatile();
                        cmd &= !0x0001; // Clear ST
                        cmd &= !0x0010; // Clear FRE
                        cmd_addr.write_volatile(cmd);
                    }
                    let mut wait_timeout = 0;
                    // SAFETY: Registers are mapped and accessed volatile.
                    while wait_timeout < 1_000_000 {
                        let cmd = unsafe { cmd_addr.read_volatile() };
                        if (cmd & (1 << 15)) == 0 && (cmd & (1 << 14)) == 0 {
                            break;
                        }
                        core::hint::spin_loop();
                        wait_timeout += 1;
                    }

                    // Write base addresses
                    // SAFETY: Registers are mapped and accessed volatile.
                    unsafe {
                        clb_addr.write_volatile(cl_phys as u32);
                        clbu_addr.write_volatile((cl_phys >> 32) as u32);
                        fb_addr.write_volatile(fis_phys as u32);
                        fbu_addr.write_volatile((fis_phys >> 32) as u32);
                    }

                    // Clear interrupts
                    let is_addr = (virt_base + port_base as u64 + PORT_IS as u64) as *mut u32;
                    // SAFETY: Registers are mapped and accessed volatile.
                    unsafe {
                        is_addr.write_volatile(is_addr.read_volatile());
                    }

                    // Start engines
                    let mut wait_timeout = 0;
                    // SAFETY: Registers are mapped and accessed volatile.
                    while wait_timeout < 1_000_000 {
                        if (unsafe { cmd_addr.read_volatile() } & (1 << 15)) == 0 {
                            break;
                        }
                        core::hint::spin_loop();
                        wait_timeout += 1;
                    }
                    // SAFETY: Registers are mapped and accessed volatile.
                    unsafe {
                        let mut cmd = cmd_addr.read_volatile();
                        cmd |= 0x0010; // FRE
                        cmd |= 0x0001; // ST
                        cmd_addr.write_volatile(cmd);
                    }

                    let port = AhciPort {
                        port_idx: i,
                        virt_base,
                        cl_phys,
                        cl_virt,
                        fis_phys,
                        fis_virt,
                        ct_phys,
                        ct_virt,
                        block_count: 0,
                    };

                    // Query capacities using IDENTIFY
                    let mut block_count = 131072;
                    if let Ok(sectors) = port.identify() {
                        block_count = sectors;
                    }
                    kprintln!(
                        "[ahci] Port {} identified: capacity {} blocks",
                        i,
                        block_count
                    );

                    let final_port = AhciPort {
                        block_count,
                        ..port
                    };

                    let info = DriverInfo {
                        name: alloc::format!("sata{}", i),
                        version: String::from("0.1.0"),
                        author: String::from("Antigravity Systems"),
                        license: String::from("MIT"),
                        description: alloc::format!("SATA Disk Drive on AHCI Port {}", i),
                    };

                    crate::drivers::register_driver(info.clone());

                    let drive = Arc::new(SataDrive {
                        port: Mutex::new(final_port),
                        info,
                    });

                    drives.push(drive as Arc<dyn BlockDevice>);
                }
            }
        }
    }

    drives
}
