//! ICMP (Internet Control Message Protocol) implementation.
//!
//! Handles ping requests/replies and error messages.

use super::ipv4;

/// ICMP header.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct IcmpHeader {
    /// ICMP message type.
    pub icmp_type: u8,
    /// ICMP message code.
    pub code: u8,
    /// Checksum over the ICMP message.
    pub checksum: u16,
}

/// ICMP Echo Request/Reply header extension.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct IcmpEcho {
    /// Base ICMP header.
    pub header: IcmpHeader,
    /// Identifier.
    pub identifier: u16,
    /// Sequence number.
    pub sequence: u16,
}

// ICMP message types.
/// Echo Reply.
pub const ICMP_ECHO_REPLY: u8 = 0;
/// Destination Unreachable.
pub const ICMP_DEST_UNREACHABLE: u8 = 3;
/// Echo Request (ping).
pub const ICMP_ECHO_REQUEST: u8 = 8;
/// Time Exceeded.
pub const ICMP_TIME_EXCEEDED: u8 = 11;

impl IcmpHeader {
    /// Parse an ICMP header from raw bytes.
    pub fn parse(data: &[u8]) -> Option<(&IcmpHeader, &[u8])> {
        if data.len() < 4 {
            return None;
        }

        let header = unsafe { &*(data.as_ptr() as *const IcmpHeader) };
        let payload = &data[4..];
        Some((header, payload))
    }

    /// Verify the ICMP checksum.
    pub fn verify_checksum(data: &[u8]) -> bool {
        ipv4::internet_checksum(data) == 0
    }
}

impl IcmpEcho {
    /// Parse an ICMP Echo Request/Reply.
    pub fn parse(data: &[u8]) -> Option<(&IcmpEcho, &[u8])> {
        if data.len() < core::mem::size_of::<IcmpEcho>() {
            return None;
        }

        let echo = unsafe { &*(data.as_ptr() as *const IcmpEcho) };
        let payload = &data[core::mem::size_of::<IcmpEcho>()..];
        Some((echo, payload))
    }

    /// Get the identifier in host byte order.
    pub fn identifier_host(&self) -> u16 {
        u16::from_be(self.identifier)
    }

    /// Get the sequence number in host byte order.
    pub fn sequence_host(&self) -> u16 {
        u16::from_be(self.sequence)
    }
}

/// Build an ICMP Echo Reply from an incoming Echo Request.
///
/// Returns the ICMP reply packet bytes.
pub fn build_echo_reply(
    buf: &mut [u8],
    identifier: u16,
    sequence: u16,
    echo_data: &[u8],
) -> Option<usize> {
    let total_len = 8 + echo_data.len();
    if buf.len() < total_len {
        return None;
    }

    buf[0] = ICMP_ECHO_REPLY;  // Type
    buf[1] = 0;                 // Code
    buf[2] = 0;                 // Checksum (zeroed for computation)
    buf[3] = 0;
    buf[4..6].copy_from_slice(&identifier.to_be_bytes());
    buf[6..8].copy_from_slice(&sequence.to_be_bytes());
    buf[8..8 + echo_data.len()].copy_from_slice(echo_data);

    // Compute and fill in the checksum
    let checksum = ipv4::internet_checksum(&buf[..total_len]);
    buf[2..4].copy_from_slice(&checksum.to_be_bytes());

    Some(total_len)
}
