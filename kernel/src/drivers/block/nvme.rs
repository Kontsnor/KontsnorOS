//! PCIe NVMe Block Storage Controller Driver for KontsnorOS.

use crate::drivers::traits::{BlockDevice, DriverError, DriverInfo};
use crate::kprintln;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

// NVMe Controller Registers (offsets relative to BAR0)
pub const CAP: u32 = 0x00; // Controller Capabilities (8 bytes)
pub const VS: u32 = 0x08; // Version (4 bytes)
pub const CC: u32 = 0x14; // Controller Configuration (4 bytes)
pub const CSTS: u32 = 0x1C; // Controller Status (4 bytes)
pub const AQA: u32 = 0x24; // Admin Queue Attributes (4 bytes)
pub const ASQ: u32 = 0x28; // Admin Submission Queue Base Address (8 bytes)
pub const ACQ: u32 = 0x30; // Admin Completion Queue Base Address (8 bytes)
pub const DOORBELL_BASE: u32 = 0x1000;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct NvmeCmd {
    pub opcode: u8,
    pub flags: u8,
    pub cid: u16,
    pub nsid: u32,
    pub reserved0: u64,
    pub mptr: u64,
    pub prp1: u64,
    pub prp2: u64,
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
    pub cdw13: u32,
    pub cdw14: u32,
    pub cdw15: u32,
}

impl Default for NvmeCmd {
    fn default() -> Self {
        Self {
            opcode: 0,
            flags: 0,
            cid: 0,
            nsid: 0,
            reserved0: 0,
            mptr: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct NvmeCqe {
    pub result: u32,
    pub reserved: u32,
    pub sq_head: u16,
    pub sq_id: u16,
    pub cid: u16,
    pub status: u16,
}

// Compile-time checks for correct NVMe structure sizes
const _: () = assert!(core::mem::size_of::<NvmeCmd>() == 64);
const _: () = assert!(core::mem::size_of::<NvmeCqe>() == 16);

pub struct NvmeQueue {
    pub sq_phys: u64,
    pub cq_phys: u64,
    pub sq_virt: *mut NvmeCmd,
    pub cq_virt: *mut NvmeCqe,
    pub sq_tail: u16,
    pub cq_head: u16,
    pub phase: u16,
    pub size: u16,
    pub db_sq: *mut u32,
    pub db_cq: *mut u32,
}

// SAFETY: NvmeQueue contains raw pointers pointing to dedicated kernel-allocated memory and registers, accessed under sync.
unsafe impl Send for NvmeQueue {}
// SAFETY: NvmeQueue contains raw pointers pointing to dedicated kernel-allocated memory and registers, accessed under sync.
unsafe impl Sync for NvmeQueue {}

impl NvmeQueue {
    pub fn new(
        sq_phys: u64,
        cq_phys: u64,
        sq_virt: *mut NvmeCmd,
        cq_virt: *mut NvmeCqe,
        size: u16,
        db_sq: *mut u32,
        db_cq: *mut u32,
    ) -> Self {
        Self {
            sq_phys,
            cq_phys,
            sq_virt,
            cq_virt,
            sq_tail: 0,
            cq_head: 0,
            phase: 1, // Phase tag starts at 1
            size,
            db_sq,
            db_cq,
        }
    }

    pub fn submit_and_wait(&mut self, mut cmd: NvmeCmd) -> Result<NvmeCqe, DriverError> {
        cmd.cid = self.sq_tail;

        let tail = self.sq_tail as usize;
        // SAFETY: sq_virt points to a valid physical page allocated specifically for this SQ ring buffer.
        unsafe {
            self.sq_virt.add(tail).write_volatile(cmd);
        }

        self.sq_tail = (self.sq_tail + 1) % self.size;

        // Ring SQ Doorbell
        // SAFETY: db_sq points to a mapped MMIO register, and access is serialized.
        unsafe {
            self.db_sq.write_volatile(self.sq_tail as u32);
        }

        let mut timeout = 0;
        let expected_phase = self.phase;
        loop {
            let head = self.cq_head as usize;
            // SAFETY: cq_virt points to a valid physical page allocated specifically for this CQ ring buffer.
            let cqe = unsafe { self.cq_virt.add(head).read_volatile() };

            // Phase Tag is bit 0 of status
            let phase_tag = cqe.status & 1;
            if phase_tag == expected_phase {
                // Command complete
                self.cq_head = (self.cq_head + 1) % self.size;
                if self.cq_head == 0 {
                    self.phase = if self.phase == 1 { 0 } else { 1 };
                }

                // Ring CQ Doorbell
                // SAFETY: db_cq points to a mapped MMIO register, and access is serialized.
                unsafe {
                    self.db_cq.write_volatile(self.cq_head as u32);
                }

                // Check for errors (status code is in status bits 1-15)
                let status_code = (cqe.status >> 1) & 0x7FFF;
                if status_code != 0 {
                    return Err(DriverError::IoError);
                }

                return Ok(cqe);
            }

            timeout += 1;
            if timeout > 10_000_000 {
                return Err(DriverError::Timeout);
            }
            core::hint::spin_loop();
        }
    }
}

pub struct NvmeController {
    pub virt_base: u64,
    pub admin_queue: NvmeQueue,
    pub io_queue: Option<NvmeQueue>,
    pub doorbell_stride: u32,
}

// SAFETY: NvmeController contains raw pointers and queues, accessed via Mutex synchronization.
unsafe impl Send for NvmeController {}
// SAFETY: NvmeController contains raw pointers and queues, accessed via Mutex synchronization.
unsafe impl Sync for NvmeController {}

pub struct NvmeNamespace {
    pub controller: Arc<Mutex<NvmeController>>,
    pub nsid: u32,
    pub block_count: u64,
    pub block_size: u64,
    pub info: DriverInfo,
}

// SAFETY: NvmeNamespace has all raw pointers inside the controller synchronized by Mutex.
unsafe impl Send for NvmeNamespace {}
// SAFETY: NvmeNamespace has all raw pointers inside the controller synchronized by Mutex.
unsafe impl Sync for NvmeNamespace {}

impl BlockDevice for NvmeNamespace {
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let mut ctrl = self.controller.lock();
        let io_queue = ctrl.io_queue.as_mut().ok_or(DriverError::NotReady)?;

        let start_virt = buf.as_ptr() as u64;
        let len = buf.len();

        // Translate the buffer virtual address to physical pages
        let mut pages = Vec::new();
        let mut offset = 0;
        while offset < len {
            let curr_vaddr = start_virt + offset as u64;
            let phys = crate::memory::r#virtual::translate_addr(VirtAddr::new(curr_vaddr))
                .ok_or(DriverError::IoError)?
                .as_u64();

            if offset == 0 {
                pages.push(phys);
            } else {
                pages.push(phys & !0xFFF);
            }

            let bytes_to_next_page = 4096 - (curr_vaddr & 0xFFF);
            offset += bytes_to_next_page as usize;
        }

        if pages.is_empty() {
            return Err(DriverError::InvalidParam);
        }

        let mut prp_list_phys_to_free = None;
        let prp1 = pages[0];
        let prp2 = if pages.len() == 1 {
            0
        } else if pages.len() == 2 {
            pages[1]
        } else {
            let prp_list_phys =
                crate::memory::physical::allocate_frame().ok_or(DriverError::OutOfMemory)?;
            let prp_list_virt =
                (prp_list_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u64;
            // SAFETY: prp_list_virt points to a valid physical frame allocated specifically for the PRP list.
            unsafe {
                core::ptr::write_bytes(prp_list_virt as *mut u8, 0, 4096);
            }
            for (i, &phys) in pages[1..].iter().enumerate() {
                // SAFETY: prp_list_virt points to the valid PRP list page layout.
                unsafe {
                    prp_list_virt.add(i).write_volatile(phys & !0xFFF);
                }
            }
            prp_list_phys_to_free = Some(prp_list_phys);
            prp_list_phys
        };

        // Construct NVMe Read I/O command (opcode 0x02)
        let mut cmd = NvmeCmd::default();
        cmd.opcode = 0x02;
        cmd.nsid = self.nsid;
        cmd.prp1 = prp1;
        cmd.prp2 = prp2;
        cmd.cdw10 = block as u32;
        cmd.cdw11 = (block >> 32) as u32;

        let sectors_count = (len as u64 / self.block_size) as u32;
        cmd.cdw12 = (sectors_count - 1) & 0xFFFF; // 0-based number of blocks

        let result = io_queue.submit_and_wait(cmd);

        if let Some(phys_to_free) = prp_list_phys_to_free {
            crate::memory::physical::deallocate_frame(phys_to_free);
        }

        result.map(|_| ())
    }

    fn write_block(&self, block: u64, data: &[u8]) -> Result<(), DriverError> {
        let mut ctrl = self.controller.lock();
        let io_queue = ctrl.io_queue.as_mut().ok_or(DriverError::NotReady)?;

        let start_virt = data.as_ptr() as u64;
        let len = data.len();

        // Translate the buffer virtual address to physical pages
        let mut pages = Vec::new();
        let mut offset = 0;
        while offset < len {
            let curr_vaddr = start_virt + offset as u64;
            let phys = crate::memory::r#virtual::translate_addr(VirtAddr::new(curr_vaddr))
                .ok_or(DriverError::IoError)?
                .as_u64();

            if offset == 0 {
                pages.push(phys);
            } else {
                pages.push(phys & !0xFFF);
            }

            let bytes_to_next_page = 4096 - (curr_vaddr & 0xFFF);
            offset += bytes_to_next_page as usize;
        }

        if pages.is_empty() {
            return Err(DriverError::InvalidParam);
        }

        let mut prp_list_phys_to_free = None;
        let prp1 = pages[0];
        let prp2 = if pages.len() == 1 {
            0
        } else if pages.len() == 2 {
            pages[1]
        } else {
            let prp_list_phys =
                crate::memory::physical::allocate_frame().ok_or(DriverError::OutOfMemory)?;
            let prp_list_virt =
                (prp_list_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u64;
            // SAFETY: prp_list_virt points to a valid physical frame allocated specifically for the PRP list.
            unsafe {
                core::ptr::write_bytes(prp_list_virt as *mut u8, 0, 4096);
            }
            for (i, &phys) in pages[1..].iter().enumerate() {
                // SAFETY: prp_list_virt points to the valid PRP list page layout.
                unsafe {
                    prp_list_virt.add(i).write_volatile(phys & !0xFFF);
                }
            }
            prp_list_phys_to_free = Some(prp_list_phys);
            prp_list_phys
        };

        // Construct NVMe Write I/O command (opcode 0x01)
        let mut cmd = NvmeCmd::default();
        cmd.opcode = 0x01;
        cmd.nsid = self.nsid;
        cmd.prp1 = prp1;
        cmd.prp2 = prp2;
        cmd.cdw10 = block as u32;
        cmd.cdw11 = (block >> 32) as u32;

        let sectors_count = (len as u64 / self.block_size) as u32;
        cmd.cdw12 = (sectors_count - 1) & 0xFFFF; // 0-based number of blocks

        let result = io_queue.submit_and_wait(cmd);

        if let Some(phys_to_free) = prp_list_phys_to_free {
            crate::memory::physical::deallocate_frame(phys_to_free);
        }

        result.map(|_| ())
    }

    fn block_size(&self) -> u64 {
        self.block_size
    }

    fn block_count(&self) -> u64 {
        self.block_count
    }

    fn flush(&self) -> Result<(), DriverError> {
        let mut ctrl = self.controller.lock();
        let io_queue = ctrl.io_queue.as_mut().ok_or(DriverError::NotReady)?;

        // Construct NVMe Flush command (opcode 0x00)
        let mut cmd = NvmeCmd::default();
        cmd.opcode = 0x00;
        cmd.nsid = self.nsid;

        io_queue.submit_and_wait(cmd).map(|_| ())
    }

    fn info(&self) -> DriverInfo {
        self.info.clone()
    }
}

/// Helper module containing exposed initializers for memory-based mock register validation in unit tests.
pub mod test_helpers {
    

    /// Helper to read registers directly
    pub unsafe fn read_reg32(virt_base: u64, offset: u32) -> u32 {
        let ptr = (virt_base + offset as u64) as *const u32;
        // SAFETY: Accesses registers mapped with caching disabled.
        unsafe { ptr.read_volatile() }
    }

    /// Helper to write registers directly
    pub unsafe fn write_reg32(virt_base: u64, offset: u32, val: u32) {
        let ptr = (virt_base + offset as u64) as *mut u32;
        // SAFETY: Accesses registers mapped with caching disabled.
        unsafe { ptr.write_volatile(val) }
    }

    /// Helper to read 64-bit registers
    pub unsafe fn read_reg64(virt_base: u64, offset: u32) -> u64 {
        let ptr = (virt_base + offset as u64) as *const u64;
        // SAFETY: Accesses registers mapped with caching disabled.
        unsafe { ptr.read_volatile() }
    }

    /// Helper to write 64-bit registers
    pub unsafe fn write_reg64(virt_base: u64, offset: u32, val: u64) {
        let ptr = (virt_base + offset as u64) as *mut u64;
        // SAFETY: Accesses registers mapped with caching disabled.
        unsafe { ptr.write_volatile(val) }
    }
}

/// Detects NVMe controller on the PCI bus, maps registers, configures the controller, and initializes active namespaces.
pub fn init() -> Vec<Arc<dyn BlockDevice>> {
    let devices = crate::drivers::bus::pci::find_by_class(0x01, 0x08);
    if devices.is_empty() {
        kprintln!("[nvme] No NVMe Controller found on PCI bus.");
        return Vec::new();
    }

    let mut drives = Vec::new();

    for dev in &devices {
        kprintln!(
            "[nvme] Found NVMe Controller at [{:02x}:{:02x}.{:01x}]",
            dev.bus,
            dev.device,
            dev.function
        );

        // Enable memory space and bus master in PCI Command register
        let cmd = crate::drivers::bus::pci::read_config(dev.bus, dev.device, dev.function, 0x04);
        crate::drivers::bus::pci::write_config(dev.bus, dev.device, dev.function, 0x04, cmd | 0x06);

        // Get BAR0 (registers base address)
        let bar0 = crate::drivers::bus::pci::read_config(dev.bus, dev.device, dev.function, 0x10);
        let mut base_phys = (bar0 & 0xFFFFFFF0) as u64;

        // Check if BAR is 64-bit
        if (bar0 & 0x06) == 0x04 {
            let bar1 =
                crate::drivers::bus::pci::read_config(dev.bus, dev.device, dev.function, 0x14);
            base_phys |= (bar1 as u64) << 32;
        }

        if base_phys == 0 || base_phys == 0xFFFFFFF0 {
            kprintln!("[nvme] BAR0 is invalid ({:#x})", bar0);
            continue;
        }

        kprintln!("[nvme] BAR0 physical base address: {:#x}", base_phys);

        // Map registers to a dedicated MMIO virtual address range
        // Non-overlapping with AHCI address range (starts at 0xffff_d000_1000_0000)
        let virt_base = 0xffff_d000_1000_0000u64;
        let page_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_CACHE
            | PageTableFlags::NO_EXECUTE;
        let num_pages = 8;
        for i in 0..num_pages {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt_base + i * 4096));
            let frame =
                PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(base_phys + i * 4096));
            // SAFETY: We explicitly map the NVMe MMIO register base address to a dedicated higher-half kernel virtual address space with NO_CACHE.
            unsafe {
                crate::memory::r#virtual::map_page(page, frame, page_flags)
                    .expect("Failed to map physical NVMe register page");
            }
        }

        // Disable controller first to allow setting configurations (CC.EN = 0)
        // SAFETY: MMIO register access is volatile and synchronized since this is early initialization.
        unsafe {
            let mut cc = test_helpers::read_reg32(virt_base, CC);
            cc &= !1; // Clear CC.EN
            test_helpers::write_reg32(virt_base, CC, cc);
        }

        // Wait for controller to report not ready (CSTS.RDY == 0)
        let mut timeout = 0;
        // SAFETY: CSTS is a valid mapped MMIO register.
        while (unsafe { test_helpers::read_reg32(virt_base, CSTS) } & 1) != 0 {
            if timeout > 1_000_000 {
                kprintln!("[nvme] Controller disable timed out!");
                break;
            }
            core::hint::spin_loop();
            timeout += 1;
        }

        // Read Capabilities register (CAP) to find the Doorbell Stride (DSTRD)
        // SAFETY: CAP is a valid mapped MMIO register.
        let cap = unsafe { test_helpers::read_reg64(virt_base, CAP) };
        let dstrd = ((cap >> 32) & 0xF) as u32;
        let doorbell_stride = 4 << dstrd;

        // Allocate physical memory pages for Admin Submission Queue (ASQ) and Admin Completion Queue (ACQ)
        let asq_phys = crate::memory::physical::allocate_frame()
            .expect("NVMe: out of physical frames for ASQ");
        let asq_virt = (asq_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut NvmeCmd;
        // SAFETY: asq_virt points to a valid physical frame allocated specifically for ASQ.
        unsafe {
            core::ptr::write_bytes(asq_virt as *mut u8, 0, 4096);
        }

        let acq_phys = crate::memory::physical::allocate_frame()
            .expect("NVMe: out of physical frames for ACQ");
        let acq_virt = (acq_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut NvmeCqe;
        // SAFETY: acq_virt points to a valid physical frame allocated specifically for ACQ.
        unsafe {
            core::ptr::write_bytes(acq_virt as *mut u8, 0, 4096);
        }

        // Set Admin Queue attributes (AQA)
        // AQS (bits 0-11) and ACQS (bits 16-27). Value is 0-based size (e.g. 63 for 64 entries)
        let aqa = (63 << 16) | 63;
        // SAFETY: AQA, ASQ, and ACQ are mapped MMIO registers.
        unsafe {
            test_helpers::write_reg32(virt_base, AQA, aqa);
            test_helpers::write_reg64(virt_base, ASQ, asq_phys);
            test_helpers::write_reg64(virt_base, ACQ, acq_phys);
        }

        // Configure Controller (CC): Enable (CC.EN = 1), Submission size 64 bytes (IOSQES = 6), Completion size 16 bytes (IOCQES = 4)
        let cc = 1 | (6 << 16) | (4 << 20);
        // SAFETY: CC is a mapped MMIO register.
        unsafe {
            test_helpers::write_reg32(virt_base, CC, cc);
        }

        // Wait for controller to report ready (CSTS.RDY == 1)
        let mut timeout = 0;
        // SAFETY: CSTS is a mapped MMIO register.
        while (unsafe { test_helpers::read_reg32(virt_base, CSTS) } & 1) == 0 {
            if timeout > 1_000_000 {
                kprintln!("[nvme] Controller enable timed out!");
                break;
            }
            core::hint::spin_loop();
            timeout += 1;
        }

        // Initialize Admin Queue
        let db_sq_admin = (virt_base + DOORBELL_BASE as u64) as *mut u32;
        let db_cq_admin = (virt_base + DOORBELL_BASE as u64 + doorbell_stride as u64) as *mut u32;
        let mut admin_queue = NvmeQueue::new(
            asq_phys,
            acq_phys,
            asq_virt,
            acq_virt,
            64,
            db_sq_admin,
            db_cq_admin,
        );

        // Allocate physical memory pages for I/O queues (SQ and CQ)
        let io_sq_phys = crate::memory::physical::allocate_frame()
            .expect("NVMe: out of physical frames for I/O SQ");
        let io_sq_virt = (io_sq_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut NvmeCmd;
        // SAFETY: io_sq_virt points to a valid physical frame allocated specifically for I/O SQ.
        unsafe {
            core::ptr::write_bytes(io_sq_virt as *mut u8, 0, 4096);
        }

        let io_cq_phys = crate::memory::physical::allocate_frame()
            .expect("NVMe: out of physical frames for I/O CQ");
        let io_cq_virt = (io_cq_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut NvmeCqe;
        // SAFETY: io_cq_virt points to a valid physical frame allocated specifically for I/O CQ.
        unsafe {
            core::ptr::write_bytes(io_cq_virt as *mut u8, 0, 4096);
        }

        // Send Create I/O Completion Queue Admin command (opcode 0x05)
        let mut cmd_ccq = NvmeCmd::default();
        cmd_ccq.opcode = 0x05;
        cmd_ccq.prp1 = io_cq_phys;
        cmd_ccq.cdw10 = (63 << 16) | 1; // Size 64, QID 1
        cmd_ccq.cdw11 = 1; // Physically Contiguous = 1
        if let Err(e) = admin_queue.submit_and_wait(cmd_ccq) {
            kprintln!("[nvme] Failed to create I/O Completion Queue: {:?}", e);
            continue;
        }

        // Send Create I/O Submission Queue Admin command (opcode 0x01)
        let mut cmd_csq = NvmeCmd::default();
        cmd_csq.opcode = 0x01;
        cmd_csq.prp1 = io_sq_phys;
        cmd_csq.cdw10 = (63 << 16) | 1; // Size 64, QID 1
        cmd_csq.cdw11 = (1 << 16) | 1; // CQID = 1, Physically Contiguous = 1
        if let Err(e) = admin_queue.submit_and_wait(cmd_csq) {
            kprintln!("[nvme] Failed to create I/O Submission Queue: {:?}", e);
            continue;
        }

        // Set up the I/O Queue doorbells (Queue 1)
        let db_sq_io = (virt_base + DOORBELL_BASE as u64 + 2 * doorbell_stride as u64) as *mut u32;
        let db_cq_io = (virt_base + DOORBELL_BASE as u64 + 3 * doorbell_stride as u64) as *mut u32;
        let io_queue = NvmeQueue::new(
            io_sq_phys, io_cq_phys, io_sq_virt, io_cq_virt, 64, db_sq_io, db_cq_io,
        );

        let controller = Arc::new(Mutex::new(NvmeController {
            virt_base,
            admin_queue,
            io_queue: Some(io_queue),
            doorbell_stride,
        }));

        // Send Identify Namespace Admin command (opcode 0x06, CNS = 1, NSID = 1)
        let identify_phys = crate::memory::physical::allocate_frame()
            .expect("NVMe: out of physical frames for Identify");
        let identify_virt =
            (identify_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
        // SAFETY: identify_virt points to a valid physical frame allocated specifically for Identify data.
        unsafe {
            core::ptr::write_bytes(identify_virt, 0, 4096);
        }

        let mut cmd_ident = NvmeCmd::default();
        cmd_ident.opcode = 0x06;
        cmd_ident.nsid = 1;
        cmd_ident.prp1 = identify_phys;
        cmd_ident.cdw10 = 1; // CNS = 1

        let ident_res = controller.lock().admin_queue.submit_and_wait(cmd_ident);
        if let Err(e) = ident_res {
            kprintln!("[nvme] Identify Namespace command failed: {:?}", e);
            crate::memory::physical::deallocate_frame(identify_phys);
            continue;
        }

        // Parse Namespace capacity (NSZE) and sector size from Identify buffer
        // NSZE is at offset 0 (8 bytes)
        // FLBAS is at offset 27 (1 byte)
        // SAFETY: identify_virt is mapped and populated by the hardware controller.
        let nsze = unsafe { (identify_virt as *const u64).read_volatile() };
        let flbas = unsafe { identify_virt.add(27).read_volatile() };
        let lbaf_idx = (flbas & 0x0F) as usize;

        // LBA Format table starts at offset 128 (4 bytes per entry)
        // SAFETY: identify_virt is mapped and populated.
        let lbaf_ptr = unsafe { identify_virt.add(128) } as *const u32;
        let lbaf_entry = unsafe { lbaf_ptr.add(lbaf_idx).read_volatile() };
        let lbads = ((lbaf_entry >> 16) & 0xFF) as u8;
        let block_size = if lbads >= 9 && lbads <= 16 {
            1u64 << lbads
        } else {
            512
        };

        kprintln!(
            "[nvme] Namespace 1 identified: size = {} sectors, block size = {} bytes",
            nsze,
            block_size
        );

        crate::memory::physical::deallocate_frame(identify_phys);

        let info = DriverInfo {
            name: String::from("nvme0"),
            version: String::from("0.1.0"),
            author: String::from("Antigravity Systems"),
            license: String::from("GPL-3.0-only"),
            description: String::from("PCIe NVMe Block Storage Namespace"),
        };

        crate::drivers::register_driver(info.clone());

        let namespace = Arc::new(NvmeNamespace {
            controller,
            nsid: 1,
            block_count: nsze,
            block_size,
            info,
        });

        drives.push(namespace as Arc<dyn BlockDevice>);
    }

    drives
}
