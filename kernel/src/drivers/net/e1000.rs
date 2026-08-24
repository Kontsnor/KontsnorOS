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

//! Intel e1000 Gigabit Ethernet Driver (82540EM).

use crate::drivers::traits::{DriverError, DriverInfo, LinkStatus, NetDevice};
use alloc::sync::Arc;
use spin::Mutex;

// Register Offsets
const REG_CTRL: u32 = 0x0000;
const REG_STATUS: u32 = 0x0008;
const REG_IMS: u32 = 0x00D8;
const REG_ICR: u32 = 0x00C0;
const REG_RCTL: u32 = 0x0100;
const REG_TCTL: u32 = 0x0400;
const REG_TIPG: u32 = 0x0410;
const REG_RDBAL: u32 = 0x2800;
const REG_RDBAH: u32 = 0x2804;
const REG_RDLEN: u32 = 0x2808;
const REG_RDH: u32 = 0x2810;
const REG_RDT: u32 = 0x2818;
const REG_TDBAL: u32 = 0x3800;
const REG_TDBAH: u32 = 0x3804;
const REG_TDLEN: u32 = 0x3808;
const REG_TDH: u32 = 0x3810;
const REG_TDT: u32 = 0x3818;
const REG_MTA: u32 = 0x5200;
const REG_RAL: u32 = 0x5400;
const REG_RAH: u32 = 0x5404;

const NUM_RX_DESC: usize = 128;
const NUM_TX_DESC: usize = 128;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct RxDesc {
    pub addr: u64,
    pub length: u16,
    pub checksum: u16,
    pub status: u8,
    pub errors: u8,
    pub special: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct TxDesc {
    pub addr: u64,
    pub length: u16,
    pub cso: u8,
    pub cmd: u8,
    pub status: u8,
    pub css: u8,
    pub special: u16,
}

pub struct E1000 {
    bar0_virt: u64,
    rx_ring_phys: u64,
    rx_ring: *mut RxDesc,
    rx_bufs_phys: [u64; NUM_RX_DESC],
    rx_bufs_virt: [u64; NUM_RX_DESC],
    rx_idx: usize,

    tx_ring_phys: u64,
    tx_ring: *mut TxDesc,
    tx_bufs_phys: [u64; NUM_TX_DESC],
    tx_bufs_virt: [u64; NUM_TX_DESC],
    tx_idx: usize,
}

unsafe impl Send for E1000 {}
unsafe impl Sync for E1000 {}

impl E1000 {
    fn write_reg(&self, offset: u32, val: u32) {
        let ptr = (self.bar0_virt + offset as u64) as *mut u32;
        unsafe {
            ptr.write_volatile(val);
        }
    }

    fn read_reg(&self, offset: u32) -> u32 {
        let ptr = (self.bar0_virt + offset as u64) as *const u32;
        unsafe { ptr.read_volatile() }
    }

    fn recv_packet(&mut self, buf: &mut [u8]) -> Result<usize, DriverError> {
        let desc_ptr = unsafe { self.rx_ring.add(self.rx_idx) };
        let status = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*desc_ptr).status)) };
        if status & 0x01 == 0 {
            return Err(DriverError::NotReady);
        }

        let len = unsafe { (*desc_ptr).length as usize };
        if len > buf.len() {
            return Err(DriverError::IoError);
        }

        let src = self.rx_bufs_virt[self.rx_idx] as *const u8;
        unsafe {
            buf[..len].copy_from_slice(core::slice::from_raw_parts(src, len));
        }

        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*desc_ptr).status), 0);
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*desc_ptr).errors), 0);
        }

        self.write_reg(REG_RDT, self.rx_idx as u32);

        self.rx_idx = (self.rx_idx + 1) % NUM_RX_DESC;
        Ok(len)
    }

    fn send_packet(&mut self, data: &[u8]) -> Result<(), DriverError> {
        let desc_ptr = unsafe { self.tx_ring.add(self.tx_idx) };

        let mut timeout = 0;
        while unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*desc_ptr).status)) } & 0x01
            == 0
        {
            timeout += 1;
            if timeout > 1000000 {
                return Err(DriverError::Timeout);
            }
            core::hint::spin_loop();
        }

        let len = data.len().min(4096);
        let dest = self.tx_bufs_virt[self.tx_idx] as *mut u8;
        unsafe {
            core::slice::from_raw_parts_mut(dest, len).copy_from_slice(&data[..len]);
        }

        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*desc_ptr).length), len as u16);
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*desc_ptr).status), 0);
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*desc_ptr).cmd), 0x0B);
            // EOP | IFCS | RS
        }

        self.write_reg(REG_TDT, ((self.tx_idx + 1) % NUM_TX_DESC) as u32);

        self.tx_idx = (self.tx_idx + 1) % NUM_TX_DESC;
        Ok(())
    }
}

pub struct E1000Device {
    inner: Mutex<E1000>,
    mac_addr: [u8; 6],
}

impl NetDevice for E1000Device {
    fn send(&self, data: &[u8]) -> Result<(), DriverError> {
        self.inner.lock().send_packet(data)
    }

    fn recv(&self, buf: &mut [u8]) -> Result<usize, DriverError> {
        self.inner.lock().recv_packet(buf)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac_addr
    }

    fn link_status(&self) -> LinkStatus {
        LinkStatus::Up
    }

    fn up(&self) -> Result<(), DriverError> {
        Ok(())
    }

    fn down(&self) -> Result<(), DriverError> {
        Ok(())
    }

    fn info(&self) -> DriverInfo {
        DriverInfo {
            name: alloc::string::String::from("e1000"),
            version: alloc::string::String::from("0.1.0"),
            author: alloc::string::String::from("KontsnorOS Core Devs"),
            license: alloc::string::String::from("GPL-3.0-only"),
            description: alloc::string::String::from("Intel e1000 Gigabit Ethernet Driver"),
        }
    }
}

// Global instances
static E1000_INSTANCE: Mutex<Option<Arc<E1000Device>>> = Mutex::new(None);

/// Initialize the e1000 driver.
///
/// # Safety
/// Caller must ensure that parameters point to a valid e1000 card.
pub unsafe fn init(bus: u8, device: u8, function: u8) {
    let cmd = crate::drivers::bus::pci::read_config(bus, device, function, 0x04);
    crate::drivers::bus::pci::write_config(bus, device, function, 0x04, cmd | 0x06); // memory space + bus master

    let bar0 = crate::drivers::bus::pci::read_config(bus, device, function, 0x10);
    let base_phys = (bar0 & 0xFFFFFFF0) as u64;
    let base_virt = base_phys + crate::memory::r#virtual::phys_mem_offset();

    crate::kprintln!(
        "[e1000] Initializing controller at phys: {:#x}, virt: {:#x}",
        base_phys,
        base_virt
    );

    let rx_ring_phys =
        crate::memory::physical::allocate_frame().expect("e1000: out of memory for Rx ring");
    let rx_ring_virt = (rx_ring_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut RxDesc;
    unsafe {
        core::ptr::write_bytes(rx_ring_virt, 0, NUM_RX_DESC);
    }

    let tx_ring_phys =
        crate::memory::physical::allocate_frame().expect("e1000: out of memory for Tx ring");
    let tx_ring_virt = (tx_ring_phys + crate::memory::r#virtual::phys_mem_offset()) as *mut TxDesc;
    unsafe {
        core::ptr::write_bytes(tx_ring_virt, 0, NUM_TX_DESC);
    }

    let mut rx_bufs_phys = [0u64; NUM_RX_DESC];
    let mut rx_bufs_virt = [0u64; NUM_RX_DESC];
    for i in 0..NUM_RX_DESC {
        let p =
            crate::memory::physical::allocate_frame().expect("e1000: out of memory for Rx buffer");
        rx_bufs_phys[i] = p;
        rx_bufs_virt[i] = p + crate::memory::r#virtual::phys_mem_offset();
        unsafe {
            let desc = rx_ring_virt.add(i);
            (*desc).addr = p;
            (*desc).status = 0;
        }
    }

    let mut tx_bufs_phys = [0u64; NUM_TX_DESC];
    let mut tx_bufs_virt = [0u64; NUM_TX_DESC];
    for i in 0..NUM_TX_DESC {
        let p =
            crate::memory::physical::allocate_frame().expect("e1000: out of memory for Tx buffer");
        tx_bufs_phys[i] = p;
        tx_bufs_virt[i] = p + crate::memory::r#virtual::phys_mem_offset();
        unsafe {
            let desc = tx_ring_virt.add(i);
            (*desc).addr = p;
            (*desc).status = 0x01; // Mark done initially
        }
    }

    let e1000 = E1000 {
        bar0_virt: base_virt,
        rx_ring_phys,
        rx_ring: rx_ring_virt,
        rx_bufs_phys,
        rx_bufs_virt,
        rx_idx: 0,
        tx_ring_phys,
        tx_ring: tx_ring_virt,
        tx_bufs_phys,
        tx_bufs_virt,
        tx_idx: 0,
    };

    e1000.write_reg(REG_CTRL, e1000.read_reg(REG_CTRL) | (1 << 26)); // RST
    let mut timeout = 0;
    while e1000.read_reg(REG_CTRL) & (1 << 26) != 0 {
        timeout += 1;
        if timeout > 1000000 {
            crate::kprintln!("[e1000] Device reset timed out!");
            break;
        }
    }

    e1000.write_reg(REG_CTRL, e1000.read_reg(REG_CTRL) | (1 << 6)); // SLU

    let ral = e1000.read_reg(REG_RAL);
    let rah = e1000.read_reg(REG_RAH);
    let mut mac = [0u8; 6];
    mac[0] = (ral & 0xFF) as u8;
    mac[1] = ((ral >> 8) & 0xFF) as u8;
    mac[2] = ((ral >> 16) & 0xFF) as u8;
    mac[3] = ((ral >> 24) & 0xFF) as u8;
    mac[4] = (rah & 0xFF) as u8;
    mac[5] = ((rah >> 8) & 0xFF) as u8;

    crate::kprintln!(
        "[e1000] MAC address: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );

    for i in 0..128 {
        e1000.write_reg(REG_MTA + (i * 4), 0);
    }

    e1000.write_reg(REG_RDBAL, rx_ring_phys as u32);
    e1000.write_reg(REG_RDBAH, (rx_ring_phys >> 32) as u32);
    e1000.write_reg(REG_RDLEN, (NUM_RX_DESC * 16) as u32);
    e1000.write_reg(REG_RDH, 0);
    e1000.write_reg(REG_RDT, (NUM_RX_DESC - 1) as u32);

    let rctl = (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 15) | (1 << 26);
    e1000.write_reg(REG_RCTL, rctl);

    e1000.write_reg(REG_TDBAL, tx_ring_phys as u32);
    e1000.write_reg(REG_TDBAH, (tx_ring_phys >> 32) as u32);
    e1000.write_reg(REG_TDLEN, (NUM_TX_DESC * 16) as u32);
    e1000.write_reg(REG_TDH, 0);
    e1000.write_reg(REG_TDT, 0);

    e1000.write_reg(REG_TIPG, 0x0060200A);

    let tctl = (1 << 1) | (1 << 3) | (15 << 4) | (64 << 12);
    e1000.write_reg(REG_TCTL, tctl);

    e1000.write_reg(REG_IMS, 0x80 | 0x40);

    let device = Arc::new(E1000Device {
        inner: Mutex::new(e1000),
        mac_addr: mac,
    });

    let net_interface = crate::net::interface::NetworkInterface {
        name: alloc::string::String::from("eth0"),
        index: 1,
        mac_addr: mac,
        ipv4_addr: crate::net::ipv4::Ipv4Addr::new(10, 0, 2, 15),
        netmask: crate::net::ipv4::Ipv4Addr::new(255, 255, 255, 0),
        gateway: crate::net::ipv4::Ipv4Addr::new(10, 0, 2, 2),
        mtu: 1500,
        is_up: true,
        rx_packets: 0,
        tx_packets: 0,
        rx_bytes: 0,
        tx_bytes: 0,
        rx_errors: 0,
        tx_errors: 0,
    };
    crate::net::interface::register_interface(net_interface);

    *E1000_INSTANCE.lock() = Some(device);
    crate::kprintln!("[e1000] Driver initialized and registered.");
}

/// Transmit a packet directly.
pub fn send_packet(data: &[u8]) -> Result<(), DriverError> {
    if let Some(ref dev) = *E1000_INSTANCE.lock() {
        dev.send(data)
    } else {
        Err(DriverError::NotFound)
    }
}

/// Handle interrupt triggered by the e1000 controller.
pub fn handle_interrupt() {
    let dev_lock = E1000_INSTANCE.lock();
    if let Some(ref dev) = *dev_lock {
        let mut inner = dev.inner.lock();
        let cause = inner.read_reg(REG_ICR);
        if cause & (0x80 | 0x40) != 0 {
            let mut buf = [0u8; 2048];
            while let Ok(len) = inner.recv_packet(&mut buf) {
                if len > 0 {
                    // Pass to the network stack!
                    crate::net::ethernet::handle_packet(&buf[..len]);
                } else {
                    break;
                }
            }
        }
    }
}
