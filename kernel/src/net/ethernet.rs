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

//! Ethernet frame processing.
//!
//! Handles parsing and constructing Ethernet II frames.

/// Ethernet frame header (14 bytes).
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct EthernetHeader {
    /// Destination MAC address.
    pub dst_mac: [u8; 6],
    /// Source MAC address.
    pub src_mac: [u8; 6],
    /// EtherType (protocol identifier).
    pub ethertype: u16,
}

/// Common EtherType values.
pub const ETHERTYPE_IPV4: u16 = 0x0800;
/// EtherType for ARP.
pub const ETHERTYPE_ARP: u16 = 0x0806;
/// EtherType for IPv6.
pub const ETHERTYPE_IPV6: u16 = 0x86DD;
/// EtherType for VLAN tagging.
pub const ETHERTYPE_VLAN: u16 = 0x8100;

/// Broadcast MAC address.
pub const BROADCAST_MAC: [u8; 6] = [0xFF; 6];

/// Minimum Ethernet frame size (excluding FCS).
pub const MIN_FRAME_SIZE: usize = 60;

/// Maximum Ethernet frame payload (MTU).
pub const MAX_MTU: usize = 1500;

/// Maximum Ethernet frame size (header + MTU).
pub const MAX_FRAME_SIZE: usize = 14 + MAX_MTU;

impl EthernetHeader {
    /// Parse an Ethernet header from raw bytes.
    ///
    /// Returns the header and the remaining payload.
    pub fn parse(data: &[u8]) -> Option<(&EthernetHeader, &[u8])> {
        if data.len() < 14 {
            return None;
        }

        // SAFETY: We verified the buffer is large enough.
        let header = unsafe { &*(data.as_ptr() as *const EthernetHeader) };
        let payload = &data[14..];

        Some((header, payload))
    }

    /// Get the EtherType in host byte order (big-endian → little-endian).
    pub fn ethertype_host(&self) -> u16 {
        u16::from_be(self.ethertype)
    }

    /// Check if this frame is a broadcast frame.
    pub fn is_broadcast(&self) -> bool {
        self.dst_mac == BROADCAST_MAC
    }

    /// Format a MAC address as a string.
    pub fn format_mac(mac: &[u8; 6]) -> alloc::string::String {
        alloc::format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5]
        )
    }
}

/// Build an Ethernet frame into a buffer.
///
/// Returns the total frame length.
pub fn build_frame(
    buf: &mut [u8],
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    ethertype: u16,
    payload: &[u8],
) -> Option<usize> {
    let total_len = 14 + payload.len();
    if buf.len() < total_len {
        return None;
    }

    buf[0..6].copy_from_slice(&dst_mac);
    buf[6..12].copy_from_slice(&src_mac);
    buf[12..14].copy_from_slice(&ethertype.to_be_bytes());
    buf[14..14 + payload.len()].copy_from_slice(payload);

    Some(total_len)
}

/// Handle an incoming Ethernet frame.
pub fn handle_packet(data: &[u8]) {
    if let Some((header, payload)) = EthernetHeader::parse(data) {
        let ethertype = header.ethertype_host();
        match ethertype {
            ETHERTYPE_ARP => {
                super::arp::handle_packet(header.src_mac, payload);
            }
            ETHERTYPE_IPV4 => {
                super::ipv4::handle_packet(header.src_mac, payload);
            }
            _ => {}
        }
    }
}
