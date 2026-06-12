//! UDP (User Datagram Protocol) implementation.
//!
//! Provides connectionless, unreliable datagram delivery.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;

use super::ipv4::Ipv4Addr;

/// UDP header (8 bytes).
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct UdpHeader {
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// Length of the UDP datagram (header + data).
    pub length: u16,
    /// Checksum.
    pub checksum: u16,
}

impl UdpHeader {
    /// Parse a UDP header from raw bytes.
    pub fn parse(data: &[u8]) -> Option<(&UdpHeader, &[u8])> {
        if data.len() < 8 {
            return None;
        }

        let header = unsafe { &*(data.as_ptr() as *const UdpHeader) };
        let length = u16::from_be(header.length) as usize;

        if length < 8 || data.len() < length {
            return None;
        }

        let payload = &data[8..length];
        Some((header, payload))
    }

    /// Get the source port in host byte order.
    pub fn src_port_host(&self) -> u16 {
        u16::from_be(self.src_port)
    }

    /// Get the destination port in host byte order.
    pub fn dst_port_host(&self) -> u16 {
        u16::from_be(self.dst_port)
    }

    /// Get the length in host byte order.
    pub fn length_host(&self) -> u16 {
        u16::from_be(self.length)
    }
}

/// A received UDP datagram.
#[derive(Debug, Clone)]
pub struct UdpDatagram {
    /// Source IP address.
    pub src_addr: Ipv4Addr,
    /// Source port.
    pub src_port: u16,
    /// Payload data.
    pub data: Vec<u8>,
}

/// A bound UDP socket endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UdpBinding {
    /// Local IP address (0.0.0.0 = any).
    pub local_addr: u32,
    /// Local port.
    pub local_port: u16,
}

/// A UDP socket with a receive queue.
pub struct UdpSocket {
    /// Binding info.
    pub binding: UdpBinding,
    /// Receive queue.
    pub recv_queue: Vec<UdpDatagram>,
    /// Maximum number of datagrams to queue.
    pub max_queue: usize,
}

impl UdpSocket {
    /// Create a new UDP socket bound to the given port.
    pub fn new(local_addr: Ipv4Addr, local_port: u16) -> Self {
        Self {
            binding: UdpBinding {
                local_addr: local_addr.to_u32(),
                local_port,
            },
            recv_queue: Vec::new(),
            max_queue: 128,
        }
    }

    /// Enqueue a received datagram.
    pub fn enqueue(&mut self, datagram: UdpDatagram) -> bool {
        if self.recv_queue.len() >= self.max_queue {
            return false; // Drop — queue full
        }
        self.recv_queue.push(datagram);
        true
    }

    /// Dequeue the next received datagram.
    pub fn dequeue(&mut self) -> Option<UdpDatagram> {
        if self.recv_queue.is_empty() {
            None
        } else {
            Some(self.recv_queue.remove(0))
        }
    }
}

/// Global UDP socket table.
static UDP_SOCKETS: Mutex<Option<BTreeMap<UdpBinding, UdpSocket>>> = Mutex::new(None);

/// Initialize the UDP subsystem.
pub fn init() {
    // Keep it minimal as we use the unified SOCKET_REGISTRY
}

/// Handle an incoming UDP packet.
pub fn handle_packet(src_ip: Ipv4Addr, dst_ip: Ipv4Addr, payload: &[u8]) {
    if let Some((header, udp_payload)) = UdpHeader::parse(payload) {
        let src_port = header.src_port_host();
        let dst_port = header.dst_port_host();

        if let Some(sock_arc) = super::socket::find_udp_socket(dst_ip, dst_port) {
            let mut sock = sock_arc.lock();
            if sock.udp_recv_queue.len() < 128 {
                let datagram = UdpDatagram {
                    src_addr: src_ip,
                    src_port,
                    data: udp_payload.to_vec(),
                };
                sock.udp_recv_queue.push_back(datagram);
                sock.wait_queue.wake_all();
            }
        }
    }
}

/// Well-known port numbers.
pub const PORT_DNS: u16 = 53;
/// DHCP server port.
pub const PORT_DHCP_SERVER: u16 = 67;
/// DHCP client port.
pub const PORT_DHCP_CLIENT: u16 = 68;
/// NTP port.
pub const PORT_NTP: u16 = 123;

/// Build a UDP datagram.
pub fn build_datagram(
    buf: &mut [u8],
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Option<usize> {
    let total_len = 8 + payload.len();
    if buf.len() < total_len || total_len > 65535 {
        return None;
    }

    buf[0..2].copy_from_slice(&src_port.to_be_bytes());
    buf[2..4].copy_from_slice(&dst_port.to_be_bytes());
    buf[4..6].copy_from_slice(&(total_len as u16).to_be_bytes());
    buf[6..8].copy_from_slice(&[0, 0]); // Checksum (optional for IPv4)
    buf[8..8 + payload.len()].copy_from_slice(payload);

    Some(total_len)
}
