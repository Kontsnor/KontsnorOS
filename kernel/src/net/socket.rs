//! Unified Socket implementation and VFS Inode wrapper.

use super::ipv4::Ipv4Addr;
use super::tcp::TcpState;
use super::udp::UdpDatagram;
use crate::fs::inode::{FileType, Inode, InodeOps};
use crate::sync::wait_queue::WaitQueue;
use alloc::collections::VecDeque;
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
                sock.tcp_rcv_nxt = sock.tcp_rcv_nxt.wrapping_add(n as u32);
            }
            Ok(n)
        } else if sock.sock_type == 2 {
            // SOCK_DGRAM (UDP)
            if sock.udp_recv_queue.is_empty() {
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
                Ok(0)
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

            let payload = data.to_vec();
            let mut tcp_buf = [0u8; 1500];
            let flags = 0x10 | 0x08; // ACK | PSH

            let tcp_len = super::tcp::build_tcp_packet(
                &mut tcp_buf,
                local_port,
                remote_port,
                tcp_snd_nxt,
                tcp_rcv_nxt,
                flags,
                &payload,
            )
            .ok_or(-5)?; // EIO

            super::ipv4::send_packet(
                local_ip,
                remote_ip,
                super::ipv4::PROTO_TCP,
                &tcp_buf[..tcp_len],
            )
            .map_err(|_| -101)?; // ENETUNREACH

            {
                let mut sock = self.socket.lock();
                sock.tcp_snd_nxt = sock.tcp_snd_nxt.wrapping_add(payload.len() as u32);
            }
            Ok(payload.len())
        } else if sock_type == 2 {
            // SOCK_DGRAM (UDP)
            let (remote_ip, remote_port, local_ip, local_port) = {
                let sock = self.socket.lock();
                (
                    sock.remote_addr.ok_or(-89)?, // EDESTADDRREQ
                    sock.remote_port.ok_or(-89)?,
                    sock.local_addr.unwrap_or(Ipv4Addr::LOCALHOST),
                    sock.local_port.unwrap_or(50000),
                )
            };

            let mut udp_buf = [0u8; 2048];
            let udp_len = super::udp::build_datagram(&mut udp_buf, local_port, remote_port, data)
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
        if (events & crate::fs::inode::POLLIN) != 0 {
            if sock.sock_type == 1 {
                // SOCK_STREAM (TCP)
                if sock.tcp_state == crate::net::tcp::TcpState::Listen {
                    if !sock.tcp_backlog.is_empty() {
                        revents |= crate::fs::inode::POLLIN;
                    }
                } else {
                    if !sock.tcp_recv_buf.is_empty()
                        || sock.tcp_state == crate::net::tcp::TcpState::CloseWait
                        || sock.tcp_state == crate::net::tcp::TcpState::Closed
                    {
                        revents |= crate::fs::inode::POLLIN;
                    }
                }
            } else if sock.sock_type == 2 {
                // SOCK_DGRAM (UDP)
                if !sock.udp_recv_queue.is_empty() {
                    revents |= crate::fs::inode::POLLIN;
                }
            }
        }
        if (events & crate::fs::inode::POLLOUT) != 0 {
            if sock.sock_type == 1 {
                // SOCK_STREAM (TCP)
                if sock.tcp_state == crate::net::tcp::TcpState::Established {
                    revents |= crate::fs::inode::POLLOUT;
                }
            } else if sock.sock_type == 2 {
                // SOCK_DGRAM (UDP) - always ready to write
                revents |= crate::fs::inode::POLLOUT;
            }
        }
        revents
    }
}

impl Drop for SocketInode {
    fn drop(&mut self) {
        let mut sock = self.socket.lock();
        sock.tcp_state = TcpState::Closed;

        let target_addr = sock.local_addr;
        let target_port = sock.local_port;
        if let Some(port) = target_port {
            let mut reg = SOCKET_REGISTRY.lock();
            reg.retain(|s| {
                if let Some(s_lock) = s.try_lock() {
                    !(s_lock.local_port == Some(port) && s_lock.local_addr == target_addr)
                } else {
                    true
                }
            });
        }
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
                && s_lock.local_addr == Some(local_ip)
                && s_lock.remote_addr == Some(remote_ip)
            {
                return Some(s.clone());
            }
        }
    }
    None
}
