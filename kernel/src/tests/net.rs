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

//! Network packet parsing unit tests.

use crate::kprintln;
use crate::net::arp::{ArpPacket, ARP_HW_ETHERNET, ARP_OP_REPLY, ARP_OP_REQUEST};
use crate::net::ipv4::{
    compute_transport_checksum, internet_checksum, Ipv4Addr, Ipv4Header, PROTO_TCP, PROTO_UDP,
};
use crate::net::tcp::{TcpHeader, TCP_ACK, TCP_SYN};
use crate::net::udp::UdpHeader;

#[test_case]
fn test_network_packet_parsing() {
    kprintln!("[test] Starting Network packet parsing unit tests...");

    // ── 1. ARP Header Parsing & Validation ───────────────────────────────────
    let valid_arp = ArpPacket {
        hw_type: ARP_HW_ETHERNET.to_be(),
        proto_type: (0x0800u16).to_be(),
        hw_len: 6,
        proto_len: 4,
        operation: ARP_OP_REQUEST.to_be(),
        sender_mac: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        sender_ip: [192, 168, 1, 100],
        target_mac: [0; 6],
        target_ip: [192, 168, 1, 1],
    };

    let arp_bytes = unsafe {
        core::slice::from_raw_parts(
            &valid_arp as *const ArpPacket as *const u8,
            core::mem::size_of::<ArpPacket>(),
        )
    };

    let parsed_arp = ArpPacket::parse(arp_bytes).expect("Valid ARP packet parse failed");
    assert_eq!(parsed_arp.operation_host(), ARP_OP_REQUEST);
    assert_eq!(parsed_arp.sender_ip_addr(), Ipv4Addr::new(192, 168, 1, 100));
    assert_eq!(parsed_arp.target_ip_addr(), Ipv4Addr::new(192, 168, 1, 1));

    // Truncated ARP packet
    assert!(ArpPacket::parse(&arp_bytes[..20]).is_none());

    // Corrupted hardware type in ARP
    let mut corrupt_arp_bytes = arp_bytes.to_vec();
    corrupt_arp_bytes[0] = 0x99;
    assert!(ArpPacket::parse(&corrupt_arp_bytes).is_none());

    // ── 2. IPv4 Header Parsing & Checksum Verification ────────────────────────
    let src_ip = Ipv4Addr::new(10, 0, 2, 15);
    let dst_ip = Ipv4Addr::new(10, 0, 2, 2);

    let mut raw_ip = [0u8; 28]; // 20 bytes IP header + 8 bytes payload
    let mut ip_hdr = Ipv4Header {
        version_ihl: 0x45, // Version 4, IHL 5 (20 bytes)
        tos: 0,
        total_length: (28u16).to_be(),
        identification: (0x1234u16).to_be(),
        flags_fragment: 0,
        ttl: 64,
        protocol: PROTO_UDP,
        checksum: 0,
        src_addr: src_ip,
        dst_addr: dst_ip,
    };

    let hdr_bytes =
        unsafe { core::slice::from_raw_parts(&ip_hdr as *const Ipv4Header as *const u8, 20) };
    ip_hdr.checksum = internet_checksum(hdr_bytes).to_be();

    let updated_hdr_bytes =
        unsafe { core::slice::from_raw_parts(&ip_hdr as *const Ipv4Header as *const u8, 20) };
    raw_ip[0..20].copy_from_slice(updated_hdr_bytes);
    raw_ip[20..28].copy_from_slice(b"TESTDATA");

    let (parsed_ip_hdr, payload) = Ipv4Header::parse(&raw_ip).expect("Valid IPv4 parse failed");
    assert!(parsed_ip_hdr.verify_checksum());
    assert_eq!(payload, b"TESTDATA");
    assert_eq!(parsed_ip_hdr.protocol(), PROTO_UDP);
    assert_eq!(parsed_ip_hdr.total_length_host(), 28);

    // Corrupt IPv4 Checksum
    raw_ip[10] ^= 0xFF; // Mutate checksum byte
    let (corrupt_ip_hdr, _) = Ipv4Header::parse(&raw_ip).unwrap();
    assert!(!corrupt_ip_hdr.verify_checksum());

    // Truncated IPv4 packet
    assert!(Ipv4Header::parse(&raw_ip[..15]).is_none());

    // ── 3. TCP Header Parsing ───────────────────────────────────────────────
    let mut raw_tcp = [0u8; 24]; // 20 bytes TCP header + 4 bytes payload
    let tcp_hdr = TcpHeader {
        src_port: (8080u16).to_be(),
        dst_port: (80u16).to_be(),
        seq_num: (1000u32).to_be(),
        ack_num: (500u32).to_be(),
        data_offset_flags: ((5u16 << 12) | (TCP_SYN | TCP_ACK)).to_be(),
        window: (65535u16).to_be(),
        checksum: 0,
        urgent_ptr: 0,
    };

    let tcp_hdr_bytes =
        unsafe { core::slice::from_raw_parts(&tcp_hdr as *const TcpHeader as *const u8, 20) };
    raw_tcp[0..20].copy_from_slice(tcp_hdr_bytes);
    raw_tcp[20..24].copy_from_slice(b"DATA");

    let (parsed_tcp, tcp_payload) = TcpHeader::parse(&raw_tcp).expect("Valid TCP parse failed");
    assert_eq!(parsed_tcp.src_port_host(), 8080);
    assert_eq!(parsed_tcp.dst_port_host(), 80);
    assert_eq!(parsed_tcp.seq_num_host(), 1000);
    assert_eq!(parsed_tcp.ack_num_host(), 500);
    assert!(parsed_tcp.is_syn());
    assert!(parsed_tcp.is_ack());
    assert!(!parsed_tcp.is_fin());
    assert_eq!(tcp_payload, b"DATA");

    // Truncated TCP header
    assert!(TcpHeader::parse(&raw_tcp[..16]).is_none());

    // ── 4. UDP Header Parsing ───────────────────────────────────────────────
    let mut raw_udp = [0u8; 12]; // 8 bytes UDP header + 4 bytes payload
    let udp_hdr = UdpHeader {
        src_port: (53u16).to_be(),
        dst_port: (12345u16).to_be(),
        length: (12u16).to_be(),
        checksum: 0,
    };

    let udp_hdr_bytes =
        unsafe { core::slice::from_raw_parts(&udp_hdr as *const UdpHeader as *const u8, 8) };
    raw_udp[0..8].copy_from_slice(udp_hdr_bytes);
    raw_udp[8..12].copy_from_slice(b"PING");

    let (parsed_udp, udp_payload) = UdpHeader::parse(&raw_udp).expect("Valid UDP parse failed");
    assert_eq!(parsed_udp.src_port_host(), 53);
    assert_eq!(parsed_udp.dst_port_host(), 12345);
    assert_eq!(parsed_udp.length_host(), 12);
    assert_eq!(udp_payload, b"PING");

    // Truncated UDP header
    assert!(UdpHeader::parse(&raw_udp[..6]).is_none());

    // ── 5. Transport Pseudo-Header Checksum Test ─────────────────────────────
    let csum_tcp = compute_transport_checksum(src_ip, dst_ip, PROTO_TCP, &raw_tcp);
    assert_ne!(csum_tcp, 0);

    let csum_udp = compute_transport_checksum(src_ip, dst_ip, PROTO_UDP, &raw_udp);
    assert_ne!(csum_udp, 0);

    kprintln!("[test] Network packet parsing unit tests PASSED!");
}
