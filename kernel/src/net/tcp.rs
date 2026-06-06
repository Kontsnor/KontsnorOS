//! TCP (Transmission Control Protocol) implementation.
//!
//! Provides reliable, ordered, connection-oriented byte streams.
//!
//! ## TCP State Machine
//!
//! ```text
//!                              ┌──────────┐
//!                    ┌────────>│  CLOSED  │<────────┐
//!                    │         └──────────┘         │
//!            passive │                              │ timeout
//!             open   │                              │
//!                    ▼                              │
//!              ┌──────────┐                   ┌──────────┐
//!              │  LISTEN  │                   │ TIME_WAIT│
//!              └──────────┘                   └──────────┘
//!                    │ rcv SYN                      ↑
//!                    ▼                              │
//!              ┌──────────┐                   ┌──────────┐
//!              │ SYN_RCVD │                   │ LAST_ACK │
//!              └──────────┘                   └──────────┘
//!                    │ rcv ACK                      ↑
//!                    ▼                              │
//!              ┌──────────┐     close         ┌──────────┐
//!              │  ESTAB   │ ───────────────→  │ CLOSE_WT │
//!              └──────────┘                   └──────────┘
//! ```

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use spin::Mutex;
use super::ipv4::Ipv4Addr;

/// TCP header (20 bytes minimum).
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct TcpHeader {
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// Sequence number.
    pub seq_num: u32,
    /// Acknowledgment number.
    pub ack_num: u32,
    /// Data offset (4 bits) + reserved (3 bits) + flags (9 bits).
    pub data_offset_flags: u16,
    /// Window size.
    pub window: u16,
    /// Checksum.
    pub checksum: u16,
    /// Urgent pointer.
    pub urgent_ptr: u16,
}

/// TCP flags.
pub const TCP_FIN: u16 = 0x001;
/// SYN flag.
pub const TCP_SYN: u16 = 0x002;
/// RST flag.
pub const TCP_RST: u16 = 0x004;
/// PSH flag.
pub const TCP_PSH: u16 = 0x008;
/// ACK flag.
pub const TCP_ACK: u16 = 0x010;
/// URG flag.
pub const TCP_URG: u16 = 0x020;

/// TCP connection states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    /// Waiting for a connection.
    Closed,
    /// Waiting for connection requests.
    Listen,
    /// SYN sent; waiting for SYN-ACK.
    SynSent,
    /// SYN received; sent SYN-ACK.
    SynReceived,
    /// Connection established.
    Established,
    /// FIN sent by local; waiting for ACK.
    FinWait1,
    /// FIN acknowledged; waiting for remote FIN.
    FinWait2,
    /// Remote FIN received; waiting for local close.
    CloseWait,
    /// Both sides closing.
    Closing,
    /// Waiting for remote FIN ACK.
    LastAck,
    /// Waiting for segment timeout.
    TimeWait,
}

/// A TCP connection endpoint (socket pair).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TcpEndpoint {
    /// Local IP address.
    pub local_addr: u32,
    /// Local port.
    pub local_port: u16,
    /// Remote IP address.
    pub remote_addr: u32,
    /// Remote port.
    pub remote_port: u16,
}

/// A TCP connection control block.
pub struct TcpConnection {
    /// Current state.
    pub state: TcpState,
    /// Send sequence variables.
    pub snd_una: u32,     // Oldest unacknowledged sequence number
    /// Next sequence number to send.
    pub snd_nxt: u32,     // Next sequence number to send
    /// Send window size.
    pub snd_wnd: u16,
    /// Receive sequence variables.
    pub rcv_nxt: u32,     // Next expected sequence number
    /// Receive window size.
    pub rcv_wnd: u16,
    /// Initial send sequence number.
    pub iss: u32,
    /// Initial receive sequence number.
    pub irs: u32,
    /// Send buffer.
    pub send_buf: alloc::vec::Vec<u8>,
    /// Receive buffer.
    pub recv_buf: alloc::vec::Vec<u8>,
}

impl TcpConnection {
    /// Create a new TCP connection in CLOSED state.
    pub fn new() -> Self {
        Self {
            state: TcpState::Closed,
            snd_una: 0,
            snd_nxt: 0,
            snd_wnd: 0,
            rcv_nxt: 0,
            rcv_wnd: 65535,
            iss: 0,
            irs: 0,
            send_buf: alloc::vec::Vec::new(),
            recv_buf: alloc::vec::Vec::new(),
        }
    }

    /// Get the number of bytes available to read.
    pub fn available(&self) -> usize {
        self.recv_buf.len()
    }

    /// Read data from the receive buffer.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.recv_buf.len());
        buf[..n].copy_from_slice(&self.recv_buf[..n]);
        self.recv_buf.drain(..n);
        n
    }

    /// Queue data for sending.
    pub fn write(&mut self, data: &[u8]) -> usize {
        self.send_buf.extend_from_slice(data);
        data.len()
    }
}

impl TcpHeader {
    /// Parse a TCP header from raw bytes.
    pub fn parse(data: &[u8]) -> Option<(&TcpHeader, &[u8])> {
        if data.len() < 20 {
            return None;
        }

        let header = unsafe { &*(data.as_ptr() as *const TcpHeader) };
        let data_offset = ((u16::from_be(header.data_offset_flags) >> 12) & 0xF) as usize * 4;

        if data_offset < 20 || data.len() < data_offset {
            return None;
        }

        let payload = &data[data_offset..];
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

    /// Get the sequence number in host byte order.
    pub fn seq_num_host(&self) -> u32 {
        u32::from_be(self.seq_num)
    }

    /// Get the ack number in host byte order.
    pub fn ack_num_host(&self) -> u32 {
        u32::from_be(self.ack_num)
    }

    /// Get the TCP flags.
    pub fn flags(&self) -> u16 {
        u16::from_be(self.data_offset_flags) & 0x1FF
    }

    /// Check if the SYN flag is set.
    pub fn is_syn(&self) -> bool {
        self.flags() & TCP_SYN != 0
    }

    /// Check if the ACK flag is set.
    pub fn is_ack(&self) -> bool {
        self.flags() & TCP_ACK != 0
    }

    /// Check if the FIN flag is set.
    pub fn is_fin(&self) -> bool {
        self.flags() & TCP_FIN != 0
    }

    /// Check if the RST flag is set.
    pub fn is_rst(&self) -> bool {
        self.flags() & TCP_RST != 0
    }
}

/// Global TCP connection table.
static TCP_CONNECTIONS: Mutex<Option<BTreeMap<TcpEndpoint, TcpConnection>>> =
    Mutex::new(None);

/// Initialize the TCP subsystem.
pub fn init() {
    // Keep it minimal as we use the unified SOCKET_REGISTRY
}

/// Build a TCP packet into a buffer.
pub fn build_tcp_packet(
    buf: &mut [u8],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u16,
    payload: &[u8],
) -> Option<usize> {
    let header_len = 20;
    let total_len = header_len + payload.len();
    if buf.len() < total_len {
        return None;
    }

    let data_offset_flags = (5u16 << 12) | (flags & 0x1FF);

    let header = TcpHeader {
        src_port: src_port.to_be(),
        dst_port: dst_port.to_be(),
        seq_num: seq.to_be(),
        ack_num: ack.to_be(),
        data_offset_flags: data_offset_flags.to_be(),
        window: (65535u16).to_be(),
        checksum: 0,
        urgent_ptr: 0,
    };

    let header_bytes = unsafe {
        core::slice::from_raw_parts(&header as *const TcpHeader as *const u8, 20)
    };
    buf[0..20].copy_from_slice(header_bytes);
    buf[20..total_len].copy_from_slice(payload);

    let checksum = super::ipv4::internet_checksum(&buf[..total_len]);
    buf[16..18].copy_from_slice(&checksum.to_be_bytes());

    Some(total_len)
}

/// Process an incoming TCP segment.
pub fn handle_packet(src_ip: Ipv4Addr, dst_ip: Ipv4Addr, payload: &[u8]) {
    if let Some((header, tcp_payload)) = TcpHeader::parse(payload) {
        let src_port = header.src_port_host();
        let dst_port = header.dst_port_host();
        let seq = header.seq_num_host();
        let ack = header.ack_num_host();
        let flags = header.flags();

        if let Some(sock_arc) = super::socket::find_tcp_connection(dst_ip, dst_port, src_ip, src_port) {
            let mut sock = sock_arc.lock();
            process_segment(&mut sock, src_ip, dst_ip, src_port, dst_port, seq, ack, flags, tcp_payload);
        } else if let Some(listener_arc) = super::socket::find_tcp_listener(dst_ip, dst_port) {
            if flags & TCP_SYN != 0 {
                let mut listener = listener_arc.lock();
                if listener.tcp_backlog.len() < listener.tcp_max_backlog {
                    let child = Arc::new(Mutex::new(super::socket::Socket::new(listener.domain, listener.sock_type, listener.protocol)));
                    {
                        let mut child_sock = child.lock();
                        child_sock.local_addr = Some(dst_ip);
                        child_sock.local_port = Some(dst_port);
                        child_sock.remote_addr = Some(src_ip);
                        child_sock.remote_port = Some(src_port);
                        child_sock.tcp_state = TcpState::SynReceived;
                        child_sock.tcp_rcv_nxt = seq.wrapping_add(1);
                        child_sock.tcp_snd_nxt = 1000;
                        child_sock.tcp_snd_una = 1000;
                        
                        let mut tcp_buf = [0u8; 128];
                        if let Some(tcp_len) = build_tcp_packet(
                            &mut tcp_buf,
                            dst_port,
                            src_port,
                            child_sock.tcp_snd_nxt,
                            child_sock.tcp_rcv_nxt,
                            TCP_SYN | TCP_ACK,
                            &[],
                        ) {
                            let _ = super::ipv4::send_packet(
                                dst_ip,
                                src_ip,
                                super::ipv4::PROTO_TCP,
                                &tcp_buf[..tcp_len],
                            );
                            child_sock.tcp_snd_nxt = child_sock.tcp_snd_nxt.wrapping_add(1);
                        }
                    }
                    listener.tcp_backlog.push(child.clone());
                    super::socket::register_socket(child);
                    listener.wait_queue.wake_all();
                }
            }
        }
    }
}

fn process_segment(
    sock: &mut super::socket::Socket,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u16,
    payload: &[u8],
) {
    match sock.tcp_state {
        TcpState::SynSent => {
            if (flags & TCP_SYN != 0) && (flags & TCP_ACK != 0) {
                sock.tcp_rcv_nxt = seq.wrapping_add(1);
                sock.tcp_snd_una = ack;
                sock.tcp_state = TcpState::Established;

                let mut tcp_buf = [0u8; 128];
                if let Some(tcp_len) = build_tcp_packet(
                    &mut tcp_buf,
                    dst_port,
                    src_port,
                    sock.tcp_snd_nxt,
                    sock.tcp_rcv_nxt,
                    TCP_ACK,
                    &[],
                ) {
                    let _ = super::ipv4::send_packet(
                        dst_ip,
                        src_ip,
                        super::ipv4::PROTO_TCP,
                        &tcp_buf[..tcp_len],
                    );
                }
                sock.wait_queue.wake_all();
            }
        }
        TcpState::SynReceived => {
            if flags & TCP_ACK != 0 {
                sock.tcp_snd_una = ack;
                sock.tcp_state = TcpState::Established;
                sock.wait_queue.wake_all();
            }
        }
        TcpState::Established => {
            if flags & TCP_ACK != 0 {
                sock.tcp_snd_una = ack;
            }

            if !payload.is_empty() {
                if seq == sock.tcp_rcv_nxt {
                    sock.tcp_recv_buf.extend_from_slice(payload);
                    sock.tcp_rcv_nxt = seq.wrapping_add(payload.len() as u32);
                    
                    let mut tcp_buf = [0u8; 128];
                    if let Some(tcp_len) = build_tcp_packet(
                        &mut tcp_buf,
                        dst_port,
                        src_port,
                        sock.tcp_snd_nxt,
                        sock.tcp_rcv_nxt,
                        TCP_ACK,
                        &[],
                    ) {
                        let _ = super::ipv4::send_packet(
                            dst_ip,
                            src_ip,
                            super::ipv4::PROTO_TCP,
                            &tcp_buf[..tcp_len],
                        );
                    }
                    sock.wait_queue.wake_all();
                }
            }

            if flags & TCP_FIN != 0 {
                sock.tcp_rcv_nxt = seq.wrapping_add(1);
                sock.tcp_state = TcpState::CloseWait;
                
                let mut tcp_buf = [0u8; 128];
                if let Some(tcp_len) = build_tcp_packet(
                    &mut tcp_buf,
                    dst_port,
                    src_port,
                    sock.tcp_snd_nxt,
                    sock.tcp_rcv_nxt,
                    TCP_ACK,
                    &[],
                ) {
                    let _ = super::ipv4::send_packet(
                        dst_ip,
                        src_ip,
                        super::ipv4::PROTO_TCP,
                        &tcp_buf[..tcp_len],
                    );
                }
                sock.wait_queue.wake_all();
            }
        }
        TcpState::FinWait1 => {
            if flags & TCP_ACK != 0 {
                sock.tcp_snd_una = ack;
                sock.tcp_state = TcpState::FinWait2;
            }
            if flags & TCP_FIN != 0 {
                sock.tcp_rcv_nxt = seq.wrapping_add(1);
                let mut tcp_buf = [0u8; 128];
                if let Some(tcp_len) = build_tcp_packet(
                    &mut tcp_buf,
                    dst_port,
                    src_port,
                    sock.tcp_snd_nxt,
                    sock.tcp_rcv_nxt,
                    TCP_ACK,
                    &[],
                ) {
                    let _ = super::ipv4::send_packet(
                        dst_ip,
                        src_ip,
                        super::ipv4::PROTO_TCP,
                        &tcp_buf[..tcp_len],
                    );
                }
                sock.tcp_state = TcpState::Closing;
            }
        }
        TcpState::FinWait2 => {
            if flags & TCP_FIN != 0 {
                sock.tcp_rcv_nxt = seq.wrapping_add(1);
                let mut tcp_buf = [0u8; 128];
                if let Some(tcp_len) = build_tcp_packet(
                    &mut tcp_buf,
                    dst_port,
                    src_port,
                    sock.tcp_snd_nxt,
                    sock.tcp_rcv_nxt,
                    TCP_ACK,
                    &[],
                ) {
                    let _ = super::ipv4::send_packet(
                        dst_ip,
                        src_ip,
                        super::ipv4::PROTO_TCP,
                        &tcp_buf[..tcp_len],
                    );
                }
                sock.tcp_state = TcpState::Closed;
                sock.wait_queue.wake_all();
            }
        }
        _ => {}
    }
}

