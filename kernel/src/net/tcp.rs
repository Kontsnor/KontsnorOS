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
use spin::Mutex;

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
    *TCP_CONNECTIONS.lock() = Some(BTreeMap::new());
}
