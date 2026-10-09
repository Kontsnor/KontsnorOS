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

//! Unix domain sockets implementation.
//!
//! Unix domain sockets provide bidirectional IPC between processes
//! on the same machine. Supports stream sockets (`SOCK_STREAM`),
//! datagram sockets (`SOCK_DGRAM`), socket pairs (`socketpair`), and
//! named filesystem-bound sockets (`bind`).

use crate::fs::inode::{FileType, Inode, InodeOps, POLLERR, POLLHUP, POLLIN, POLLOUT};
use crate::sync::wait_queue::WaitQueue;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

/// State of a Unix domain socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixSocketState {
    Unbound,
    Bound,
    Listening,
    Connected,
    Closed,
}

/// A Unix domain datagram message.
#[derive(Debug, Clone)]
pub struct UnixDatagram {
    pub src_path: Option<String>,
    pub data: Vec<u8>,
}

/// Inner representation of a Unix domain socket.
pub struct UnixSocket {
    pub sock_type: i32, // 1 = SOCK_STREAM, 2 = SOCK_DGRAM
    pub state: UnixSocketState,
    pub path: Option<String>,
    pub nonblocking: bool,
    pub wait_queue: Arc<WaitQueue>,

    // SOCK_STREAM fields
    pub peer: Option<Arc<Mutex<UnixSocket>>>,
    pub recv_buf: VecDeque<u8>,
    pub backlog: Vec<Arc<Mutex<UnixSocket>>>,
    pub max_backlog: usize,

    // SOCK_DGRAM fields
    pub dgram_recv_queue: VecDeque<UnixDatagram>,
}

impl UnixSocket {
    pub fn new(sock_type: i32) -> Self {
        Self {
            sock_type,
            state: UnixSocketState::Unbound,
            path: None,
            nonblocking: false,
            wait_queue: Arc::new(WaitQueue::new()),
            peer: None,
            recv_buf: VecDeque::new(),
            backlog: Vec::new(),
            max_backlog: 0,
            dgram_recv_queue: VecDeque::new(),
        }
    }

    /// Create a connected pair of Unix domain sockets.
    pub fn make_pair(
        sock_type: i32,
        nonblock: bool,
    ) -> (Arc<Mutex<UnixSocket>>, Arc<Mutex<UnixSocket>>) {
        let sock_a = Arc::new(Mutex::new(UnixSocket::new(sock_type)));
        let sock_b = Arc::new(Mutex::new(UnixSocket::new(sock_type)));

        {
            let mut a = sock_a.lock();
            let mut b = sock_b.lock();

            a.state = UnixSocketState::Connected;
            b.state = UnixSocketState::Connected;

            a.nonblocking = nonblock;
            b.nonblocking = nonblock;

            a.peer = Some(sock_b.clone());
            b.peer = Some(sock_a.clone());
        }

        (sock_a, sock_b)
    }
}

/// Global registry mapping filesystem paths to bound Unix domain sockets.
static UNIX_SOCKET_REGISTRY: Mutex<BTreeMap<String, Arc<Mutex<UnixSocket>>>> =
    Mutex::new(BTreeMap::new());

/// Register a Unix domain socket at the given filesystem path.
pub fn register_unix_socket(path: String, socket: Arc<Mutex<UnixSocket>>) -> Result<(), i32> {
    let mut reg = UNIX_SOCKET_REGISTRY.lock();
    if reg.contains_key(&path) {
        return Err(-98); // EADDRINUSE
    }
    reg.insert(path, socket);
    Ok(())
}

/// Look up a bound Unix domain socket by filesystem path.
pub fn find_unix_socket(path: &str) -> Option<Arc<Mutex<UnixSocket>>> {
    UNIX_SOCKET_REGISTRY.lock().get(path).cloned()
}

/// Unregister a Unix domain socket path.
pub fn unregister_unix_socket(path: &str) {
    UNIX_SOCKET_REGISTRY.lock().remove(path);
}

/// VFS Inode wrapper for a Unix domain socket.
pub struct UnixSocketInode {
    pub socket: Arc<Mutex<UnixSocket>>,
    pub inode: Inode,
}

impl UnixSocketInode {
    pub fn new(socket: Arc<Mutex<UnixSocket>>) -> Self {
        Self {
            socket,
            inode: Inode::new(0, FileType::Socket),
        }
    }
}

impl InodeOps for UnixSocketInode {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn wait_queue(&self) -> Option<Arc<WaitQueue>> {
        Some(self.socket.lock().wait_queue.clone())
    }

    fn as_unix_socket(&self) -> Option<Arc<Mutex<UnixSocket>>> {
        Some(self.socket.clone())
    }

    fn set_nonblocking(&self, nonblocking: bool) {
        self.socket.lock().nonblocking = nonblocking;
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let mut sock = self.socket.lock();
        if sock.sock_type == 1 {
            // SOCK_STREAM
            if sock.state != UnixSocketState::Connected && sock.state != UnixSocketState::Listening
            {
                return Err(-107); // ENOTCONN
            }

            let mut wq_opt: Option<Arc<WaitQueue>> = None;
            while sock.recv_buf.is_empty() {
                // If peer is closed or disconnected, return EOF (0)
                let peer_opt = sock.peer.clone();
                let peer_closed = match peer_opt {
                    Some(ref p) => {
                        drop(sock);
                        let closed = p.lock().state == UnixSocketState::Closed;
                        sock = self.socket.lock();
                        closed
                    }
                    None => true,
                };
                if peer_closed {
                    return Ok(0);
                }

                if sock.nonblocking {
                    return Err(-11); // EAGAIN
                }

                let tok = sock.wait_queue.token();
                let wq = wq_opt.get_or_insert_with(|| sock.wait_queue.clone());
                drop(sock);
                wq.wait_since(tok);
                sock = self.socket.lock();
            }

            let n = buf.len().min(sock.recv_buf.len());
            for i in 0..n {
                buf[i] = sock.recv_buf.pop_front().unwrap();
            }
            Ok(n)
        } else if sock.sock_type == 2 {
            // SOCK_DGRAM
            let mut wq_opt: Option<Arc<WaitQueue>> = None;
            while sock.dgram_recv_queue.is_empty() {
                if sock.nonblocking {
                    return Err(-11); // EAGAIN
                }
                let tok = sock.wait_queue.token();
                let wq = wq_opt.get_or_insert_with(|| sock.wait_queue.clone());
                drop(sock);
                wq.wait_since(tok);
                sock = self.socket.lock();
            }

            if let Some(dg) = sock.dgram_recv_queue.pop_front() {
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
        let sock = self.socket.lock();
        if sock.sock_type == 1 {
            // SOCK_STREAM
            if sock.state != UnixSocketState::Connected {
                return Err(-107); // ENOTCONN
            }

            let peer = match sock.peer {
                Some(ref p) => p.clone(),
                None => return Err(-32), // EPIPE
            };
            drop(sock);

            let mut peer_sock = peer.lock();
            if peer_sock.state == UnixSocketState::Closed {
                return Err(-32); // EPIPE
            }

            peer_sock.recv_buf.extend(data.iter().copied());
            peer_sock.wait_queue.wake_all();
            Ok(data.len())
        } else if sock.sock_type == 2 {
            // SOCK_DGRAM (connected or default destination)
            let peer = match sock.peer {
                Some(ref p) => p.clone(),
                None => return Err(-89), // EDESTADDRREQ
            };
            let src_path = sock.path.clone();
            drop(sock);

            let mut peer_sock = peer.lock();
            peer_sock.dgram_recv_queue.push_back(UnixDatagram {
                src_path,
                data: data.to_vec(),
            });
            peer_sock.wait_queue.wake_all();
            Ok(data.len())
        } else {
            Err(-22) // EINVAL
        }
    }

    fn poll(&self, events: u32) -> u32 {
        let mut revents = 0;
        let sock = self.socket.lock();

        if sock.sock_type == 1 {
            // SOCK_STREAM
            match sock.state {
                UnixSocketState::Connected => {
                    if (events & POLLIN) != 0 && !sock.recv_buf.is_empty() {
                        revents |= POLLIN;
                    }
                    // Ready to write if peer is connected and active
                    if (events & POLLOUT) != 0 {
                        if let Some(ref peer) = sock.peer {
                            if peer.lock().state != UnixSocketState::Closed {
                                revents |= POLLOUT;
                            } else {
                                revents |= POLLERR | POLLHUP;
                            }
                        } else {
                            revents |= POLLERR | POLLHUP;
                        }
                    }
                    let peer_closed = match sock.peer {
                        Some(ref p) => p.lock().state == UnixSocketState::Closed,
                        None => true,
                    };
                    if peer_closed {
                        revents |= POLLIN | POLLHUP;
                    }
                }
                UnixSocketState::Listening => {
                    if (events & POLLIN) != 0 && !sock.backlog.is_empty() {
                        revents |= POLLIN;
                    }
                }
                UnixSocketState::Closed => {
                    revents |= POLLERR | POLLHUP;
                }
                _ => {}
            }
        } else if sock.sock_type == 2 {
            // SOCK_DGRAM
            if (events & POLLIN) != 0 && !sock.dgram_recv_queue.is_empty() {
                revents |= POLLIN;
            }
            if (events & POLLOUT) != 0 {
                revents |= POLLOUT;
            }
        }

        revents
    }
}

impl Drop for UnixSocketInode {
    fn drop(&mut self) {
        let (path, peer_opt) = {
            let mut sock = self.socket.lock();
            sock.state = UnixSocketState::Closed;
            sock.wait_queue.wake_all();
            (sock.path.take(), sock.peer.take())
        };

        if let Some(ref peer) = peer_opt {
            peer.lock().wait_queue.wake_all();
        }

        if let Some(p) = path {
            unregister_unix_socket(&p);
            crate::fs::vfs::invalidate_dentry(&p);
        }
    }
}
