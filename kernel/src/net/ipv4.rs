//! IPv4 protocol implementation.
//!
//! Handles parsing, building, and routing IPv4 packets.

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
            10 => true,                                    // 10.0.0.0/8
            172 => (16..=31).contains(&self.octets[1]),    // 172.16.0.0/12
            192 => self.octets[1] == 168,                  // 192.168.0.0/16
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

        let payload = &data[ihl..];
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
        let data = unsafe {
            core::slice::from_raw_parts(self as *const _ as *const u8, ihl)
        };
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
            b.netmask.to_u32().count_ones().cmp(&a.netmask.to_u32().count_ones())
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
