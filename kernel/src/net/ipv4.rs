//! IPv4 protocol implementation.
//!
//! Handles parsing, building, and routing IPv4 packets.

use spin::Mutex;

/// IPv4 header (20 bytes minimum, up to 60 with options).
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Ipv4Header {
    /// Version (4 bits) + IHL (4 bits).
    pub version_ihl: u8,
    /// Type of Service / DSCP + ECN.
    pub tos: u8,
    /// Total length of the packet.
    pub total_length: u16,
    /// Identification (for fragmentation).
    pub identification: u16,
    /// Flags (3 bits) + Fragment offset (13 bits).
    pub flags_fragment: u16,
    /// Time to Live.
    pub ttl: u8,
    /// Protocol (TCP=6, UDP=17, ICMP=1).
    pub protocol: u8,
    /// Header checksum.
    pub checksum: u16,
    /// Source IP address.
    pub src_addr: Ipv4Addr,
    /// Destination IP address.
    pub dst_addr: Ipv4Addr,
}

/// An IPv4 address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Ipv4Addr {
    /// The four octets.
    pub octets: [u8; 4],
}

impl Ipv4Addr {
    /// Create a new IPv4 address.
    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Self {
            octets: [a, b, c, d],
        }
    }

    /// The unspecified address (0.0.0.0).
    pub const UNSPECIFIED: Ipv4Addr = Ipv4Addr::new(0, 0, 0, 0);
    /// The loopback address (127.0.0.1).
    pub const LOCALHOST: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
    /// The broadcast address (255.255.255.255).
    pub const BROADCAST: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 255);

    /// Convert to a 32-bit integer (network byte order).
    pub fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.octets)
    }

    /// Create from a 32-bit integer (network byte order).
    pub fn from_u32(addr: u32) -> Self {
        Self {
            octets: addr.to_be_bytes(),
        }
    }

    /// Check if this is a loopback address (127.x.x.x).
    pub fn is_loopback(&self) -> bool {
        self.octets[0] == 127
    }

    /// Check if this is a broadcast address.
    pub fn is_broadcast(&self) -> bool {
        *self == Self::BROADCAST
    }

    /// Check if this is a multicast address (224.0.0.0/4).
    pub fn is_multicast(&self) -> bool {
        self.octets[0] >= 224 && self.octets[0] <= 239
    }

    /// Check if this is a private address (RFC 1918).
    pub fn is_private(&self) -> bool {
        match self.octets[0] {
            10 => true,                                 // 10.0.0.0/8
            172 => (16..=31).contains(&self.octets[1]), // 172.16.0.0/12
            192 => self.octets[1] == 168,               // 192.168.0.0/16
            _ => false,
        }
    }
}

impl core::fmt::Display for Ipv4Addr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.octets[0], self.octets[1], self.octets[2], self.octets[3]
        )
    }
}

/// IP protocol numbers.
pub const PROTO_ICMP: u8 = 1;
/// TCP protocol number.
pub const PROTO_TCP: u8 = 6;
/// UDP protocol number.
pub const PROTO_UDP: u8 = 17;

impl Ipv4Header {
    /// Parse an IPv4 header from raw bytes.
    pub fn parse(data: &[u8]) -> Option<(&Ipv4Header, &[u8])> {
        if data.len() < 20 {
            return None;
        }

        // SAFETY: We verified the buffer is large enough.
        let header = unsafe { &*(data.as_ptr() as *const Ipv4Header) };

        // Verify version is 4
        let version = header.version_ihl >> 4;
        if version != 4 {
            return None;
        }

        let ihl = (header.version_ihl & 0x0F) as usize * 4;
        if ihl < 20 || data.len() < ihl {
            return None;
        }

        let total_len = u16::from_be(header.total_length) as usize;
        if total_len < ihl || data.len() < total_len {
            return None;
        }

        let payload = &data[ihl..total_len];
        Some((header, payload))
    }

    /// Get the header length in bytes.
    pub fn header_length(&self) -> usize {
        (self.version_ihl & 0x0F) as usize * 4
    }

    /// Get total packet length in host byte order.
    pub fn total_length_host(&self) -> u16 {
        u16::from_be(self.total_length)
    }

    /// Get the protocol number.
    pub fn protocol(&self) -> u8 {
        self.protocol
    }

    /// Verify the header checksum.
    pub fn verify_checksum(&self) -> bool {
        let ihl = self.header_length();
        let data = unsafe { core::slice::from_raw_parts(self as *const _ as *const u8, ihl) };
        internet_checksum(data) == 0
    }
}

/// Compute the Internet checksum (RFC 1071).
///
/// Used for IPv4 headers, ICMP, TCP, and UDP.
pub fn internet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;

    // Sum 16-bit words
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }

    // Handle odd byte
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }

    // Fold 32-bit sum to 16 bits
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    !(sum as u16)
}

/// A routing table entry.
#[derive(Debug, Clone)]
pub struct RouteEntry {
    /// Destination network.
    pub destination: Ipv4Addr,
    /// Subnet mask.
    pub netmask: Ipv4Addr,
    /// Next-hop gateway (0.0.0.0 for directly connected).
    pub gateway: Ipv4Addr,
    /// Network interface index.
    pub interface_idx: u32,
    /// Route metric (lower = preferred).
    pub metric: u32,
}

/// The kernel routing table.
pub struct RoutingTable {
    /// Routes sorted by specificity (most specific first).
    pub routes: alloc::vec::Vec<RouteEntry>,
}

impl RoutingTable {
    /// Create a new, empty routing table.
    pub fn new() -> Self {
        Self {
            routes: alloc::vec::Vec::new(),
        }
    }

    /// Add a route to the table.
    pub fn add_route(&mut self, route: RouteEntry) {
        self.routes.push(route);
        // Sort by netmask specificity (most specific first)
        self.routes.sort_by(|a, b| {
            b.netmask
                .to_u32()
                .count_ones()
                .cmp(&a.netmask.to_u32().count_ones())
        });
    }

    /// Look up the route for a destination address.
    pub fn lookup(&self, dst: Ipv4Addr) -> Option<&RouteEntry> {
        let dst_u32 = dst.to_u32();

        for route in &self.routes {
            let net_u32 = route.destination.to_u32();
            let mask_u32 = route.netmask.to_u32();

            if dst_u32 & mask_u32 == net_u32 & mask_u32 {
                return Some(route);
            }
        }

        None
    }
}

pub static ROUTING_TABLE: Mutex<Option<RoutingTable>> = Mutex::new(None);

/// Initialize the IPv4 routing table.
pub fn init_routing() {
    let mut table = RoutingTable::new();
    // Default loopback route
    table.add_route(RouteEntry {
        destination: Ipv4Addr::new(127, 0, 0, 0),
        netmask: Ipv4Addr::new(255, 0, 0, 0),
        gateway: Ipv4Addr::UNSPECIFIED,
        interface_idx: 0,
        metric: 0,
    });
    // Default eth0 subnet route
    table.add_route(RouteEntry {
        destination: Ipv4Addr::new(10, 0, 2, 0),
        netmask: Ipv4Addr::new(255, 255, 255, 0),
        gateway: Ipv4Addr::UNSPECIFIED,
        interface_idx: 1,
        metric: 0,
    });
    // Default gateway route
    table.add_route(RouteEntry {
        destination: Ipv4Addr::UNSPECIFIED,
        netmask: Ipv4Addr::UNSPECIFIED,
        gateway: Ipv4Addr::new(10, 0, 2, 2),
        interface_idx: 1,
        metric: 10,
    });
    *ROUTING_TABLE.lock() = Some(table);
}

/// Handle an incoming IPv4 packet.
pub fn handle_packet(src_mac: [u8; 6], payload: &[u8]) {
    if let Some((header, ip_payload)) = Ipv4Header::parse(payload) {
        if !header.verify_checksum() {
            return;
        }

        let src_ip = header.src_addr;
        let dst_ip = header.dst_addr;

        // Auto-update ARP cache
        super::arp::update(src_ip, src_mac);

        let is_us = super::interface::find_interface_by_ip(dst_ip).is_some()
            || dst_ip.is_broadcast()
            || dst_ip.is_loopback();

        if is_us {
            match header.protocol {
                PROTO_ICMP => {
                    super::icmp::handle_packet(src_ip, ip_payload);
                }
                PROTO_UDP => {
                    super::udp::handle_packet(src_ip, dst_ip, ip_payload);
                }
                PROTO_TCP => {
                    super::tcp::handle_packet(src_ip, dst_ip, ip_payload);
                }
                _ => {}
            }
        }
    }
}

static IP_IDENT: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(1);

/// Build an IPv4 packet into a buffer.
pub fn build_ipv4_packet(
    buf: &mut [u8],
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    protocol: u8,
    payload: &[u8],
) -> Option<usize> {
    let header_len = 20;
    let total_len = header_len + payload.len();
    if buf.len() < total_len || total_len > 65535 {
        return None;
    }

    let ident = IP_IDENT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    let mut header = Ipv4Header {
        version_ihl: 0x45,
        tos: 0,
        total_length: (total_len as u16).to_be(),
        identification: ident.to_be(),
        flags_fragment: 0,
        ttl: 64,
        protocol,
        checksum: 0,
        src_addr: src_ip,
        dst_addr: dst_ip,
    };

    let header_bytes =
        unsafe { core::slice::from_raw_parts(&header as *const Ipv4Header as *const u8, 20) };
    header.checksum = internet_checksum(header_bytes).to_be();

    let header_bytes_updated =
        unsafe { core::slice::from_raw_parts(&header as *const Ipv4Header as *const u8, 20) };
    buf[0..20].copy_from_slice(header_bytes_updated);
    buf[20..total_len].copy_from_slice(payload);

    Some(total_len)
}

/// Send an IPv4 packet, performing routing and ARP resolution as needed.
pub fn send_packet(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    protocol: u8,
    payload: &[u8],
) -> Result<(), &'static str> {
    if dst_ip.is_loopback() {
        let mut ip_buf = [0u8; 2048];
        if let Some(ip_len) = build_ipv4_packet(&mut ip_buf, src_ip, dst_ip, protocol, payload) {
            handle_packet([0; 6], &ip_buf[..ip_len]);
            return Ok(());
        }
        return Err("Failed to build loopback IP packet");
    }

    let next_hop = {
        let table_lock = ROUTING_TABLE.lock();
        let table = table_lock.as_ref().ok_or("Routing table not initialized")?;
        let route = table.lookup(dst_ip).ok_or("No route to host")?;
        if route.gateway == Ipv4Addr::UNSPECIFIED {
            dst_ip
        } else {
            route.gateway
        }
    };

    let mut dst_mac = super::arp::lookup(next_hop);
    if dst_mac.is_none() {
        super::arp::send_request(next_hop);
        let start_ticks = crate::arch::x86_64::interrupts::timer_ticks();
        while dst_mac.is_none() && crate::arch::x86_64::interrupts::timer_ticks() - start_ticks < 5
        {
            core::hint::spin_loop();
            dst_mac = super::arp::lookup(next_hop);
        }
    }

    let dst_mac = dst_mac.ok_or("ARP resolution failed")?;
    let (_, local_mac) =
        super::interface::get_first_ethernet_interface().ok_or("No ethernet interface up")?;

    let mut ip_buf = [0u8; 1600];
    let ip_len = build_ipv4_packet(&mut ip_buf, src_ip, dst_ip, protocol, payload)
        .ok_or("IP packet too large")?;

    let mut eth_buf = [0u8; 1620];
    let eth_len = super::ethernet::build_frame(
        &mut eth_buf,
        dst_mac,
        local_mac,
        super::ethernet::ETHERTYPE_IPV4,
        &ip_buf[..ip_len],
    )
    .ok_or("Ethernet frame too large")?;

    let _ = crate::drivers::net::e1000::send_packet(&eth_buf[..eth_len]);
    Ok(())
}
