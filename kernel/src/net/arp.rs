//! ARP (Address Resolution Protocol) implementation.
//!
//! Maps IPv4 addresses to MAC addresses on local networks.

use alloc::collections::BTreeMap;
use spin::Mutex;

use super::ipv4::Ipv4Addr;

/// ARP hardware type: Ethernet.
pub const ARP_HW_ETHERNET: u16 = 1;

/// ARP operation: Request.
pub const ARP_OP_REQUEST: u16 = 1;
/// ARP operation: Reply.
pub const ARP_OP_REPLY: u16 = 2;

/// ARP header for Ethernet + IPv4.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct ArpPacket {
    /// Hardware type (1 = Ethernet).
    pub hw_type: u16,
    /// Protocol type (0x0800 = IPv4).
    pub proto_type: u16,
    /// Hardware address length (6 for Ethernet).
    pub hw_len: u8,
    /// Protocol address length (4 for IPv4).
    pub proto_len: u8,
    /// Operation (1 = request, 2 = reply).
    pub operation: u16,
    /// Sender hardware (MAC) address.
    pub sender_mac: [u8; 6],
    /// Sender protocol (IP) address.
    pub sender_ip: [u8; 4],
    /// Target hardware (MAC) address.
    pub target_mac: [u8; 6],
    /// Target protocol (IP) address.
    pub target_ip: [u8; 4],
}

impl ArpPacket {
    /// Parse an ARP packet from raw bytes.
    pub fn parse(data: &[u8]) -> Option<&ArpPacket> {
        if data.len() < core::mem::size_of::<ArpPacket>() {
            return None;
        }

        let packet = unsafe { &*(data.as_ptr() as *const ArpPacket) };

        // Verify it's Ethernet + IPv4
        if u16::from_be(packet.hw_type) != ARP_HW_ETHERNET {
            return None;
        }
        if packet.hw_len != 6 || packet.proto_len != 4 {
            return None;
        }

        Some(packet)
    }

    /// Get the operation in host byte order.
    pub fn operation_host(&self) -> u16 {
        u16::from_be(self.operation)
    }

    /// Get the sender IP as an Ipv4Addr.
    pub fn sender_ip_addr(&self) -> Ipv4Addr {
        Ipv4Addr::new(
            self.sender_ip[0],
            self.sender_ip[1],
            self.sender_ip[2],
            self.sender_ip[3],
        )
    }

    /// Get the target IP as an Ipv4Addr.
    pub fn target_ip_addr(&self) -> Ipv4Addr {
        Ipv4Addr::new(
            self.target_ip[0],
            self.target_ip[1],
            self.target_ip[2],
            self.target_ip[3],
        )
    }
}

/// An entry in the ARP cache.
#[derive(Debug, Clone)]
pub struct ArpEntry {
    /// MAC address for this IP.
    pub mac: [u8; 6],
    /// Timestamp when this entry was created/updated (in ticks).
    pub timestamp: u64,
}

/// Global ARP cache.
static ARP_CACHE: Mutex<Option<BTreeMap<u32, ArpEntry>>> = Mutex::new(None);

/// Initialize the ARP cache.
pub fn init() {
    *ARP_CACHE.lock() = Some(BTreeMap::new());
}

/// Look up a MAC address for an IP address.
pub fn lookup(ip: Ipv4Addr) -> Option<[u8; 6]> {
    let cache = ARP_CACHE.lock();
    cache
        .as_ref()?
        .get(&ip.to_u32())
        .map(|entry| entry.mac)
}

/// Insert or update an ARP cache entry.
pub fn update(ip: Ipv4Addr, mac: [u8; 6]) {
    if let Some(ref mut cache) = *ARP_CACHE.lock() {
        let ticks = crate::arch::x86_64::interrupts::timer_ticks();
        cache.insert(
            ip.to_u32(),
            ArpEntry {
                mac,
                timestamp: ticks,
            },
        );
    }
}
