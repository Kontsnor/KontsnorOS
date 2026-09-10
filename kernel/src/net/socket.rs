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

//! Unified Socket implementation and VFS Inode wrapper.

use super::ipv4::Ipv4Addr;
use super::tcp::TcpState;
use super::udp::UdpDatagram;
use crate::fs::inode::{FileType, Inode, InodeOps};
use crate::sync::wait_queue::WaitQueue;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

/// Representation of a socket.
pub struct Socket {
    pub domain: i32,
    pub sock_type: i32,
    pub protocol: i32,
    pub local_addr: Option<Ipv4Addr>,
    pub local_port: Option<u16>,
    pub remote_addr: Option<Ipv4Addr>,
    pub remote_port: Option<u16>,

    // UDP specific
    pub udp_recv_queue: VecDeque<UdpDatagram>,

    // TCP specific
    pub tcp_state: TcpState,
    pub tcp_snd_una: u32,
    pub tcp_snd_nxt: u32,
    pub tcp_rcv_nxt: u32,
    pub tcp_recv_buf: Vec<u8>,
    pub tcp_send_buf: Vec<u8>,
    pub tcp_backlog: Vec<Arc<Mutex<Socket>>>,
    pub tcp_max_backlog: usize,
    /// Dynamic receive window — shrinks as tcp_recv_buf fills up.
    pub tcp_rcv_wnd: u16,
    /// Out-of-order segment queue: seq_num → payload bytes.
    pub tcp_ooo_queue: BTreeMap<u32, Vec<u8>>,
    /// Pending connection error for getsockopt(SO_ERROR) (0 = no error).
    pub so_error: i32,
    /// True once a remote address has been set (via connect/accept),
    /// used to distinguish a fresh Closed socket from a connect-failed one.
    pub had_remote_addr: bool,

    // Non-blocking mode flag
    pub nonblocking: bool,

    // Wait queue for blocking calls
    pub wait_queue: Arc<WaitQueue>,
}

impl Socket {
    pub fn new(domain: i32, sock_type: i32, protocol: i32) -> Self {
        Self {
            domain,
            sock_type,
            protocol,
            local_addr: None,
            local_port: None,
            remote_addr: None,
            remote_port: None,
            udp_recv_queue: VecDeque::new(),
            tcp_state: TcpState::Closed,
            tcp_snd_una: 0,
            tcp_snd_nxt: 0,
            tcp_rcv_nxt: 0,
            tcp_recv_buf: Vec::new(),
            tcp_send_buf: Vec::new(),
            tcp_backlog: Vec::new(),
            tcp_max_backlog: 0,
            tcp_rcv_wnd: 65535,
            tcp_ooo_queue: BTreeMap::new(),
            so_error: 0,
            had_remote_addr: false,
            nonblocking: false,
            wait_queue: Arc::new(WaitQueue::new()),
        }
    }
}

/// VFS Inode wrapper for a Socket.
pub struct SocketInode {
    pub socket: Arc<Mutex<Socket>>,
    pub inode: Inode,
}

impl SocketInode {
    pub fn new(socket: Arc<Mutex<Socket>>) -> Self {
        Self {
            socket,
            inode: Inode::new(0, FileType::Socket),
        }
    }
}

impl InodeOps for SocketInode {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn as_socket(&self) -> Option<Arc<Mutex<Socket>>> {
        Some(self.socket.clone())
    }

    fn set_nonblocking(&self, nonblocking: bool) {
        self.socket.lock().nonblocking = nonblocking;
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let mut sock = self.socket.lock();
        if sock.sock_type == 1 {
            // SOCK_STREAM (TCP)
            if sock.tcp_state == TcpState::Closed {
                return Err(-104); // ECONNRESET
            }
            if sock.tcp_recv_buf.is_empty() {
                if sock.tcp_state == TcpState::CloseWait {
                    return Ok(0); // EOF
                }
                if sock.nonblocking {
                    return Err(-11); // -EAGAIN
                }
                // Block/Wait
                let wq = sock.wait_queue.clone();
                drop(sock);
                wq.wait();
                sock = self.socket.lock();
            }
            let n = buf.len().min(sock.tcp_recv_buf.len());
            if n > 0 {
                buf[..n].copy_from_slice(&sock.tcp_recv_buf[..n]);
                sock.tcp_recv_buf.drain(..n);
                // NOTE: tcp_rcv_nxt is already advanced by process_segment() as
                // data arrives from the network. We must NOT advance it again here
                // or the ACK numbers sent to the remote will be wrong.
            }
            Ok(n)
        } else if sock.sock_type == 2 || sock.sock_type == 3 {
            // SOCK_DGRAM (UDP) or SOCK_RAW / ICMP
            if sock.udp_recv_queue.is_empty() {
                if sock.nonblocking {
                    return Err(-11); // -EAGAIN
                }
                let wq = sock.wait_queue.clone();
                drop(sock);
                wq.wait();
                sock = self.socket.lock();
            }
            if let Some(dg) = sock.udp_recv_queue.pop_front() {
                let n = buf.len().min(dg.data.len());
                buf[..n].copy_from_slice(&dg.data[..n]);
                Ok(n)
            } else {
                if sock.nonblocking {
                    Err(-11)
                } else {
                    Ok(0)
                }
            }
        } else {
            Err(-22) // EINVAL
        }
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        let sock_type = self.socket.lock().sock_type;
        if sock_type == 1 {
            // SOCK_STREAM (TCP)
            let (local_ip, remote_ip, local_port, remote_port, tcp_snd_nxt, tcp_rcv_nxt) = {
                let sock = self.socket.lock();
                if sock.tcp_state != TcpState::Established {
                    return Err(-32); // EPIPE / ENOTCONN
                }
                (
                    sock.local_addr.unwrap_or(Ipv4Addr::LOCALHOST),
                    sock.remote_addr.ok_or(-107)?, // ENOTCONN
                    sock.local_port.unwrap_or(0),
                    sock.remote_port.unwrap_or(0),
                    sock.tcp_snd_nxt,
                    sock.tcp_rcv_nxt,
                )
            };

            let chunk_len = data.len().min(1460);
            let payload = data[..chunk_len].to_vec();
            let mut tcp_buf = [0u8; 1500];
            let flags = 0x10 | 0x08; // ACK | PSH

            // Advertise the remaining receive window dynamically.
            let window = {
                let sock = self.socket.lock();
                let buf_used = sock.tcp_recv_buf.len();
                (65536usize.saturating_sub(buf_used) as u32).min(65535) as u16
            };

            let tcp_len = super::tcp::build_tcp_packet(
                &mut tcp_buf,
                local_ip,
                remote_ip,
                local_port,
                remote_port,
                tcp_snd_nxt,
                tcp_rcv_nxt,
                flags,
                window,
                &payload,
            )
            .ok_or(-5)?; // EIO

            crate::kprintln!(
                "[SocketInode::write] Sending TCP payload: len={}, seq={}, ack={}",
                payload.len(),
                tcp_snd_nxt,
                tcp_rcv_nxt
            );

            super::ipv4::send_packet(
                local_ip,
                remote_ip,
                super::ipv4::PROTO_TCP,
                &tcp_buf[..tcp_len],
            )
            .map_err(|e| {
                crate::kprintln!("[SocketInode::write] send_packet failed: {}", e);
                -101
            })?; // ENETUNREACH

            {
                let mut sock = self.socket.lock();
                sock.tcp_snd_nxt = sock.tcp_snd_nxt.wrapping_add(payload.len() as u32);
            }
            crate::kprintln!("[SocketInode::write] payload sent successfully");
            Ok(payload.len())
        } else if sock_type == 2 {
            // SOCK_DGRAM (UDP)
            let (remote_ip, remote_port, local_ip, local_port) = {
                let mut sock = self.socket.lock();
                let r_ip = sock.remote_addr.ok_or(-89)?; // EDESTADDRREQ
                let r_port = sock.remote_port.ok_or(-89)?;
                if sock.local_port.is_none() || sock.local_port == Some(0) {
                    sock.local_port = Some(crate::syscall::net::alloc_ephemeral_port());
                }
                if sock.local_addr.is_none() || sock.local_addr == Some(Ipv4Addr::UNSPECIFIED) {
                    let ip = if r_ip.is_loopback() {
                        Ipv4Addr::LOCALHOST
                    } else {
                        crate::net::interface::get_first_ethernet_interface()
                            .map(|(ip, _)| ip)
                            .unwrap_or(Ipv4Addr::new(10, 0, 2, 15))
                    };
                    sock.local_addr = Some(ip);
                }
                (
                    r_ip,
                    r_port,
                    sock.local_addr.unwrap(),
                    sock.local_port.unwrap(),
                )
            };

            let mut udp_buf = [0u8; 2048];
            let udp_len = super::udp::build_datagram(
                &mut udp_buf,
                local_ip,
                remote_ip,
                local_port,
                remote_port,
                data,
            )
            .ok_or(-22)?;

            super::ipv4::send_packet(
                local_ip,
                remote_ip,
                super::ipv4::PROTO_UDP,
                &udp_buf[..udp_len],
            )
            .map_err(|_| -101)?; // ENETUNREACH
            Ok(data.len())
        } else {
            Err(-22) // EINVAL
        }
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        let sock = self.socket.lock();
        if sock.sock_type == 1 {
            // SOCK_STREAM (TCP)
            match sock.tcp_state {
                crate::net::tcp::TcpState::Established => {
                    // Ready to read if there's buffered data or peer closed.
                    if (events & crate::fs::inode::POLLIN) != 0 && !sock.tcp_recv_buf.is_empty() {
                        revents |= crate::fs::inode::POLLIN;
                    }
                    // Always writable when established.
                    if (events & crate::fs::inode::POLLOUT) != 0 {
                        revents |= crate::fs::inode::POLLOUT;
                    }
                }
                crate::net::tcp::TcpState::Listen => {
                    if (events & crate::fs::inode::POLLIN) != 0 && !sock.tcp_backlog.is_empty() {
                        revents |= crate::fs::inode::POLLIN;
                    }
                }
                crate::net::tcp::TcpState::CloseWait => {
                    // Data may remain; reads return EOF after buffer drained.
                    if (events & crate::fs::inode::POLLIN) != 0 {
                        revents |= crate::fs::inode::POLLIN;
                    }
                }
                crate::net::tcp::TcpState::Closed => {
                    // If we previously attempted a connect (had_remote_addr),
                    // a Closed state here means the connection failed or was
                    // reset. Signal POLLERR and POLLHUP so libcurl's
                    // non-blocking connect error path fires correctly.
                    if sock.had_remote_addr {
                        revents |= crate::fs::inode::POLLERR | crate::fs::inode::POLLHUP;
                    }
                }
                _ => {}
            }
        } else if sock.sock_type == 2 {
            // SOCK_DGRAM (UDP)
            if (events & crate::fs::inode::POLLIN) != 0 && !sock.udp_recv_queue.is_empty() {
                revents |= crate::fs::inode::POLLIN;
            }
            // UDP is always ready to write.
            if (events & crate::fs::inode::POLLOUT) != 0 {
                revents |= crate::fs::inode::POLLOUT;
            }
        } else if sock.sock_type == 3 {
            // SOCK_RAW — readable when queue has data, always writable.
            if (events & crate::fs::inode::POLLIN) != 0 && !sock.udp_recv_queue.is_empty() {
                revents |= crate::fs::inode::POLLIN;
            }
            if (events & crate::fs::inode::POLLOUT) != 0 {
                revents |= crate::fs::inode::POLLOUT;
            }
        }
        revents
    }
}

impl Drop for SocketInode {
    fn drop(&mut self) {
        {
            let mut sock = self.socket.lock();
            sock.tcp_state = TcpState::Closed;
        }

        let mut reg = SOCKET_REGISTRY.lock();
        reg.retain(|s| !Arc::ptr_eq(s, &self.socket));
    }
}

// Global registry of all active sockets
pub static SOCKET_REGISTRY: Mutex<Vec<Arc<Mutex<Socket>>>> = Mutex::new(Vec::new());

/// Register a new socket.
pub fn register_socket(sock: Arc<Mutex<Socket>>) {
    SOCKET_REGISTRY.lock().push(sock);
}

/// Find a bound UDP socket.
pub fn find_udp_socket(local_ip: Ipv4Addr, local_port: u16) -> Option<Arc<Mutex<Socket>>> {
    let reg = SOCKET_REGISTRY.lock();
    for s in reg.iter() {
        let s_lock = s.lock();
        if s_lock.sock_type == 2 {
            // SOCK_DGRAM
            if s_lock.local_port == Some(local_port)
                && (s_lock.local_addr == Some(local_ip)
                    || s_lock.local_addr == Some(Ipv4Addr::UNSPECIFIED)
                    || s_lock.local_addr.is_none())
            {
                return Some(s.clone());
            }
        }
    }
    None
}

/// Find a TCP socket matching local port.
pub fn find_tcp_listener(local_ip: Ipv4Addr, local_port: u16) -> Option<Arc<Mutex<Socket>>> {
    let reg = SOCKET_REGISTRY.lock();
    for s in reg.iter() {
        let s_lock = s.lock();
        if s_lock.sock_type == 1 && s_lock.tcp_state == TcpState::Listen {
            // TCP Listen
            if s_lock.local_port == Some(local_port)
                && (s_lock.local_addr == Some(local_ip)
                    || s_lock.local_addr == Some(Ipv4Addr::UNSPECIFIED)
                    || s_lock.local_addr.is_none())
            {
                return Some(s.clone());
            }
        }
    }
    None
}

/// Find an established TCP connection.
pub fn find_tcp_connection(
    local_ip: Ipv4Addr,
    local_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,
) -> Option<Arc<Mutex<Socket>>> {
    let reg = SOCKET_REGISTRY.lock();
    for s in reg.iter() {
        let s_lock = s.lock();
        if s_lock.sock_type == 1 {
            // TCP
            if s_lock.local_port == Some(local_port)
                && s_lock.remote_port == Some(remote_port)
                && (s_lock.local_addr == Some(local_ip)
                    || s_lock.local_addr == Some(Ipv4Addr::UNSPECIFIED)
                    || s_lock.local_addr.is_none())
                && s_lock.remote_addr == Some(remote_ip)
            {
                return Some(s.clone());
            }
        }
    }
    None
}
