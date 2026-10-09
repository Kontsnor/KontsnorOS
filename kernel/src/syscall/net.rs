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

//! Network and Unix domain socket system calls.

use crate::fs::file::OpenFlags;
use crate::ipc::socket::{
    find_unix_socket, register_unix_socket, UnixDatagram, UnixSocket, UnixSocketInode,
    UnixSocketState,
};
use crate::net::ipv4::Ipv4Addr;
use crate::net::socket::{Socket, SocketInode};
use crate::process::fd as proc_fd;
use crate::syscall::fs::validate_user_ptr;
use crate::syscall::validation::copy_string_from_user;
use crate::syscall::{Errno, SyscallResult};
use alloc::string::String;
use alloc::sync::Arc;
use spin::Mutex;

/// Standard POSIX sockaddr_in structure for IPv4 addresses.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SockAddrIn {
    pub sin_family: u16,
    pub sin_port: u16,
    pub sin_addr: [u8; 4],
    pub sin_zero: [u8; 8],
}

/// Standard POSIX sockaddr_un structure for Unix domain sockets.
#[repr(C)]
pub struct SockAddrUn {
    pub sun_family: u16,
    pub sun_path: [u8; 108],
}

/// Helper to get the inner network socket from a file descriptor.
fn get_socket(fd: i32) -> Option<Arc<Mutex<Socket>>> {
    let file_desc = proc_fd::current_task_get_file_desc(fd)?;
    file_desc.inode.as_socket()
}

/// Helper to get the inner Unix domain socket from a file descriptor.
fn get_unix_socket(fd: i32) -> Option<Arc<Mutex<UnixSocket>>> {
    let file_desc = proc_fd::current_task_get_file_desc(fd)?;
    file_desc.inode.as_unix_socket()
}

/// Helper to parse a Unix domain socket path from a user-space sockaddr_un pointer.
fn parse_sockaddr_un(addr_ptr: *const u8, addrlen: u32) -> Result<String, Errno> {
    if addr_ptr.is_null() {
        return Err(Errno::EFAULT);
    }
    if addrlen < 3 {
        return Err(Errno::EINVAL);
    }
    if !validate_user_ptr(addr_ptr, addrlen as usize) {
        return Err(Errno::EFAULT);
    }

    let family = unsafe { core::ptr::read_unaligned(addr_ptr as *const u16) };
    if family != 1 {
        return Err(Errno::EAFNOSUPPORT);
    }

    let path_bytes_len = (addrlen as usize).saturating_sub(2).min(108);
    let path_ptr = unsafe { addr_ptr.add(2) };
    let raw_slice = unsafe { core::slice::from_raw_parts(path_ptr, path_bytes_len) };

    // Find null terminator or end of slice
    let end = raw_slice
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(raw_slice.len());
    let path_slice = &raw_slice[..end];

    if path_slice.is_empty() {
        return Err(Errno::EINVAL);
    }

    let path_str = core::str::from_utf8(path_slice).map_err(|_| Errno::EINVAL)?;
    Ok(crate::fs::vfs::resolve_relative_path(path_str))
}

static NEXT_EPHEMERAL_PORT: core::sync::atomic::AtomicU16 =
    core::sync::atomic::AtomicU16::new(49152);

/// Allocate a dynamic ephemeral port in range [49152, 65000].
pub fn alloc_ephemeral_port() -> u16 {
    let port = NEXT_EPHEMERAL_PORT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if !(49152..=65000).contains(&port) {
        NEXT_EPHEMERAL_PORT.store(49152, core::sync::atomic::Ordering::Relaxed);
        49152
    } else {
        port
    }
}

/// `socket(domain, type, protocol)` — create an endpoint for communication.
pub fn sys_socket(domain: i32, sock_type: i32, protocol: i32) -> SyscallResult {
    let nonblock = (sock_type & 0x800) != 0;
    let cloexec = (sock_type & 0x80000) != 0;
    let base_type = sock_type & !(0x800 | 0x80000);

    // Support AF_UNIX / AF_LOCAL (1)
    if domain == 1 {
        if base_type != 1 && base_type != 2 {
            return Errno::EINVAL.into();
        }

        let unix_sock = Arc::new(Mutex::new(UnixSocket::new(base_type)));
        unix_sock.lock().nonblocking = nonblock;

        let mut open_flags = OpenFlags(OpenFlags::O_RDWR);
        if nonblock {
            open_flags.0 |= OpenFlags::O_NONBLOCK;
        }
        if cloexec {
            open_flags.0 |= OpenFlags::O_CLOEXEC;
        }

        let inode = Arc::new(UnixSocketInode::new(unix_sock));
        return match proc_fd::current_task_alloc_fd_with_flags(inode, open_flags) {
            Some(fd) => fd as SyscallResult,
            None => Errno::EMFILE.into(),
        };
    }

    // AF_NETLINK (16) -> return EAFNOSUPPORT so glibc gracefully falls back
    if domain == 16 {
        return Errno::EAFNOSUPPORT.into();
    }

    // AF_INET6 (10) -> return EAFNOSUPPORT if IPv6 is not supported
    if domain == 10 && !crate::net::ipv6_supported() {
        return Errno::EAFNOSUPPORT.into();
    }

    if domain != 2 {
        // Only support AF_INET (2)
        return Errno::EAFNOSUPPORT.into();
    }

    if base_type != 1 && base_type != 2 && base_type != 3 {
        // Support SOCK_STREAM (1), SOCK_DGRAM (2), SOCK_RAW (3)
        return Errno::EINVAL.into();
    }

    let socket = Arc::new(Mutex::new(Socket::new(domain, base_type, protocol)));
    socket.lock().nonblocking = nonblock;
    crate::net::socket::register_socket(socket.clone());

    let mut open_flags = OpenFlags(OpenFlags::O_RDWR);
    if nonblock {
        open_flags.0 |= OpenFlags::O_NONBLOCK;
    }
    if cloexec {
        open_flags.0 |= OpenFlags::O_CLOEXEC;
    }

    let inode = Arc::new(SocketInode::new(socket));
    match proc_fd::current_task_alloc_fd_with_flags(inode, open_flags) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}

/// `bind(fd, addr_ptr, addrlen)` — bind a name to a socket.
pub fn sys_bind(fd: i32, addr_ptr: *const SockAddrIn, addrlen: u32) -> SyscallResult {
    if addr_ptr.is_null() {
        return Errno::EFAULT.into();
    }
    if addrlen < 2 {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(addr_ptr as *const u8, 2) {
        return Errno::EFAULT.into();
    }

    let family = unsafe { core::ptr::read_unaligned(addr_ptr as *const u16) };

    if family == 1 {
        // AF_UNIX / AF_LOCAL
        let unix_sock = match get_unix_socket(fd) {
            Some(s) => s,
            None => return Errno::EBADF.into(),
        };

        let abs_path = match parse_sockaddr_un(addr_ptr as *const u8, addrlen) {
            Ok(p) => p,
            Err(e) => return e.into(),
        };

        let mut sock = unix_sock.lock();
        if sock.state != UnixSocketState::Unbound {
            return Errno::EINVAL.into();
        }

        if let Err(e) = register_unix_socket(abs_path.clone(), unix_sock.clone()) {
            return e as SyscallResult;
        }

        // Create VFS node for the bound socket
        if let Some((parent_dir, filename)) = crate::syscall::fs::meta::resolve_parent(&abs_path) {
            let _ = parent_dir.create(&filename, crate::fs::inode::FileType::Socket);
        }

        sock.path = Some(abs_path);
        sock.state = UnixSocketState::Bound;
        return 0;
    }

    if family == 10 {
        return Errno::EAFNOSUPPORT.into();
    }
    if family != 2 {
        return Errno::EINVAL.into();
    }
    if addrlen < 16 {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(addr_ptr as *const u8, core::mem::size_of::<SockAddrIn>()) {
        return Errno::EFAULT.into();
    }

    let addr = unsafe { core::ptr::read_unaligned(addr_ptr) };

    let socket = match get_socket(fd) {
        Some(s) => s,
        None => return Errno::EBADF.into(),
    };

    let local_ip = Ipv4Addr::new(
        addr.sin_addr[0],
        addr.sin_addr[1],
        addr.sin_addr[2],
        addr.sin_addr[3],
    );
    let raw_port = u16::from_be(addr.sin_port);
    let local_port = if raw_port == 0 {
        alloc_ephemeral_port()
    } else {
        raw_port
    };

    let mut sock = socket.lock();
    sock.local_addr = if local_ip == Ipv4Addr::UNSPECIFIED {
        None
    } else {
        Some(local_ip)
    };
    sock.local_port = Some(local_port);

    0
}

/// `connect(fd, addr_ptr, addrlen)` — initiate a connection on a socket.
pub fn sys_connect(fd: i32, addr_ptr: *const SockAddrIn, addrlen: u32) -> SyscallResult {
    if addr_ptr.is_null() {
        return Errno::EFAULT.into();
    }
    if addrlen < 2 {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(addr_ptr as *const u8, 2) {
        return Errno::EFAULT.into();
    }

    let family = unsafe { core::ptr::read_unaligned(addr_ptr as *const u16) };

    if family == 1 {
        // AF_UNIX / AF_LOCAL
        let unix_sock = match get_unix_socket(fd) {
            Some(s) => s,
            None => return Errno::EBADF.into(),
        };

        let abs_path = match parse_sockaddr_un(addr_ptr as *const u8, addrlen) {
            Ok(p) => p,
            Err(e) => return e.into(),
        };

        let target_sock = match find_unix_socket(&abs_path) {
            Some(s) => s,
            None => return Errno::ECONNREFUSED.into(),
        };

        let sock_type = unix_sock.lock().sock_type;
        if sock_type == 1 {
            // SOCK_STREAM
            let mut target = target_sock.lock();
            if target.state != UnixSocketState::Listening {
                return Errno::ECONNREFUSED.into();
            }

            if target.backlog.len() >= target.max_backlog {
                return Errno::EAGAIN.into();
            }

            // Create client-side peer socket
            let peer_sock = Arc::new(Mutex::new(UnixSocket::new(1)));

            {
                let mut peer = peer_sock.lock();
                peer.state = UnixSocketState::Connected;
                peer.peer = Some(unix_sock.clone());
            }

            {
                let mut sock = unix_sock.lock();
                sock.state = UnixSocketState::Connected;
                sock.peer = Some(peer_sock.clone());
            }

            target.backlog.push(peer_sock);
            target.wait_queue.wake_all();
            return 0;
        } else if sock_type == 2 {
            // SOCK_DGRAM
            let mut sock = unix_sock.lock();
            sock.peer = Some(target_sock);
            sock.state = UnixSocketState::Connected;
            return 0;
        } else {
            return Errno::EINVAL.into();
        }
    }

    if family == 10 {
        return Errno::ENETUNREACH.into();
    }
    if family != 2 {
        return Errno::EAFNOSUPPORT.into();
    }

    if addrlen < 16 {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(addr_ptr as *const u8, core::mem::size_of::<SockAddrIn>()) {
        return Errno::EFAULT.into();
    }

    let addr = unsafe { core::ptr::read_unaligned(addr_ptr) };

    let socket = match get_socket(fd) {
        Some(s) => s,
        None => return Errno::EBADF.into(),
    };

    let remote_ip = Ipv4Addr::new(
        addr.sin_addr[0],
        addr.sin_addr[1],
        addr.sin_addr[2],
        addr.sin_addr[3],
    );
    let remote_port = u16::from_be(addr.sin_port);

    if remote_ip == Ipv4Addr::UNSPECIFIED {
        return Errno::ENETUNREACH.into();
    }
    if !remote_ip.is_loopback() && crate::net::interface::get_first_ethernet_interface().is_none() {
        return Errno::ENETUNREACH.into();
    }

    let nonblocking = {
        let fd_desc = proc_fd::current_task_get_file_desc(fd);
        let nonblock_fd = fd_desc
            .map(|d| d.flags.lock().0 & OpenFlags::O_NONBLOCK != 0)
            .unwrap_or(false);
        let nonblock_sock = socket.lock().nonblocking;
        nonblock_fd || nonblock_sock
    };

    let sock_type;
    let mut tcp_state;
    let local_port;
    let local_ip;
    let tcp_snd_nxt;

    {
        let mut sock = socket.lock();
        sock_type = sock.sock_type;
        sock.remote_addr = Some(remote_ip);
        sock.remote_port = Some(remote_port);

        if sock.local_port.is_none() || sock.local_port == Some(0) {
            sock.local_port = Some(alloc_ephemeral_port());
        }
        if sock.local_addr.is_none() || sock.local_addr == Some(Ipv4Addr::UNSPECIFIED) {
            let ip = if remote_ip.is_loopback() {
                Ipv4Addr::LOCALHOST
            } else {
                crate::net::interface::get_first_ethernet_interface()
                    .map(|(ip, _)| ip)
                    .unwrap_or(Ipv4Addr::new(10, 0, 2, 15))
            };
            sock.local_addr = Some(ip);
        }

        if sock_type == 2 {
            return 0;
        }

        sock.tcp_state = crate::net::tcp::TcpState::SynSent;
        sock.tcp_snd_nxt = 1000;
        sock.tcp_snd_una = 1000;
        sock.had_remote_addr = true;

        local_port = match sock.local_port {
            Some(p) => p,
            None => return Errno::EINVAL.into(),
        };
        local_ip = match sock.local_addr {
            Some(ip) => ip,
            None => return Errno::EINVAL.into(),
        };
        tcp_snd_nxt = sock.tcp_snd_nxt;
        sock.tcp_snd_nxt = sock.tcp_snd_nxt.wrapping_add(1);
    }

    let mut tcp_buf = [0u8; 128];
    if let Some(tcp_len) = crate::net::tcp::build_tcp_packet(
        &mut tcp_buf,
        local_ip,
        remote_ip,
        local_port,
        remote_port,
        tcp_snd_nxt,
        0,
        crate::net::tcp::TCP_SYN,
        65535,
        &[],
    ) {
        let _ = crate::net::ipv4::send_packet(
            local_ip,
            remote_ip,
            crate::net::ipv4::PROTO_TCP,
            &tcp_buf[..tcp_len],
        );
    }

    if nonblocking {
        return -115;
    }

    let start_ticks = crate::arch::x86_64::interrupts::timer_ticks();
    let wq = socket.lock().wait_queue.clone();
    loop {
        let tok = wq.token();
        {
            let sock = socket.lock();
            tcp_state = sock.tcp_state;
        }
        if tcp_state == crate::net::tcp::TcpState::Established {
            break;
        }
        if tcp_state == crate::net::tcp::TcpState::Closed {
            return -111;
        }
        if crate::arch::x86_64::interrupts::timer_ticks() - start_ticks > 100 {
            let mut sock = socket.lock();
            sock.tcp_state = crate::net::tcp::TcpState::Closed;
            return -110;
        }
        wq.wait_since(tok);
    }

    0
}

/// `listen(fd, backlog)` — listen for connections on a socket.
pub fn sys_listen(fd: i32, backlog: i32) -> SyscallResult {
    if let Some(unix_sock) = get_unix_socket(fd) {
        let mut sock = unix_sock.lock();
        if sock.sock_type != 1 {
            return Errno::EINVAL.into();
        }
        sock.state = UnixSocketState::Listening;
        sock.max_backlog = (backlog.max(1) as usize).min(128);
        return 0;
    }

    let socket = match get_socket(fd) {
        Some(s) => s,
        None => return Errno::EBADF.into(),
    };

    let mut sock = socket.lock();
    if sock.sock_type != 1 {
        return Errno::EINVAL.into();
    }

    sock.tcp_state = crate::net::tcp::TcpState::Listen;
    sock.tcp_max_backlog = (backlog.max(1) as usize).min(128);
    0
}

/// `accept(fd, addr_ptr, addrlen_ptr)` — accept a connection on a socket.
pub fn sys_accept(fd: i32, addr_ptr: *mut SockAddrIn, addrlen_ptr: *mut u32) -> SyscallResult {
    sys_accept4(fd, addr_ptr, addrlen_ptr, 0)
}

/// `accept4(fd, addr_ptr, addrlen_ptr, flags)` — accept a connection on a socket with flags.
pub fn sys_accept4(
    fd: i32,
    addr_ptr: *mut SockAddrIn,
    addrlen_ptr: *mut u32,
    flags: i32,
) -> SyscallResult {
    let sock_nonblock = 0x800;
    let sock_cloexec = 0x80000;
    if (flags & !(sock_nonblock | sock_cloexec)) != 0 {
        return Errno::EINVAL.into();
    }

    if let Some(unix_sock) = get_unix_socket(fd) {
        let child_sock = loop {
            let wq;
            let tok;
            {
                let mut sock = unix_sock.lock();
                if sock.sock_type != 1 || sock.state != UnixSocketState::Listening {
                    return Errno::EINVAL.into();
                }

                if !sock.backlog.is_empty() {
                    break sock.backlog.remove(0);
                }
                if (flags & sock_nonblock) != 0 || sock.nonblocking {
                    return Errno::EAGAIN.into();
                }
                tok = sock.wait_queue.token();
                wq = sock.wait_queue.clone();
            }
            wq.wait_since(tok);
        };

        if !addr_ptr.is_null() && !addrlen_ptr.is_null() {
            if crate::syscall::fs::validate_user_ptr_write(addr_ptr as *mut u8, 2).is_ok()
                && crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_ok()
            {
                unsafe {
                    (addr_ptr as *mut u16).write(1); // AF_UNIX
                    addrlen_ptr.write(2);
                }
            }
        }

        let mut open_flags = OpenFlags(OpenFlags::O_RDWR);
        if (flags & sock_nonblock) != 0 {
            open_flags.0 |= OpenFlags::O_NONBLOCK;
            child_sock.lock().nonblocking = true;
        }
        if (flags & sock_cloexec) != 0 {
            open_flags.0 |= OpenFlags::O_CLOEXEC;
        }

        let inode = Arc::new(UnixSocketInode::new(child_sock));
        return match proc_fd::current_task_alloc_fd_with_flags(inode, open_flags) {
            Some(new_fd) => new_fd as SyscallResult,
            None => Errno::EMFILE.into(),
        };
    }

    let socket = match get_socket(fd) {
        Some(s) => s,
        None => return Errno::EBADF.into(),
    };

    let child = loop {
        let wq;
        let tok;
        {
            let mut sock = socket.lock();
            if sock.sock_type != 1 || sock.tcp_state != crate::net::tcp::TcpState::Listen {
                return Errno::EINVAL.into();
            }

            if !sock.tcp_backlog.is_empty() {
                break sock.tcp_backlog.remove(0);
            }
            if (flags & sock_nonblock) != 0 {
                return Errno::EAGAIN.into();
            }
            tok = sock.wait_queue.token();
            wq = sock.wait_queue.clone();
        }
        wq.wait_since(tok);
    };

    let child_sock = child.lock();

    if !addr_ptr.is_null() && !addrlen_ptr.is_null() {
        if crate::syscall::fs::validate_user_ptr_write(
            addr_ptr as *mut u8,
            core::mem::size_of::<SockAddrIn>(),
        )
        .is_err()
            || crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err()
        {
            return Errno::EFAULT.into();
        }

        let remote_ip = child_sock.remote_addr.unwrap_or(Ipv4Addr::LOCALHOST);
        let remote_port = child_sock.remote_port.unwrap_or(0);

        unsafe {
            addr_ptr.write(SockAddrIn {
                sin_family: 2,
                sin_port: remote_port.to_be(),
                sin_addr: remote_ip.octets,
                sin_zero: [0; 8],
            });
            addrlen_ptr.write(16);
        }
    }

    drop(child_sock);

    let mut open_flags = OpenFlags(OpenFlags::O_RDWR);
    if (flags & sock_cloexec) != 0 {
        open_flags.0 |= OpenFlags::O_CLOEXEC;
    }

    let inode = Arc::new(SocketInode::new(child));
    match proc_fd::current_task_alloc_fd_with_flags(inode, open_flags) {
        Some(new_fd) => new_fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}

/// `sendto(fd, buf, len, flags, dest_addr, addrlen)` — send a message on a socket.
pub fn sys_sendto(
    fd: i32,
    buf: *const u8,
    len: usize,
    _flags: i32,
    dest_addr: *const SockAddrIn,
    addrlen: u32,
) -> SyscallResult {
    if buf.is_null() || len == 0 {
        return 0;
    }
    if !validate_user_ptr(buf, len) {
        return Errno::EFAULT.into();
    }

    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    let mut kernel_buf = alloc::vec![0u8; len];
    unsafe {
        core::ptr::copy_nonoverlapping(buf, kernel_buf.as_mut_ptr(), len);
    }

    if let Some(unix_sock) = file_desc.inode.as_unix_socket() {
        if !dest_addr.is_null() {
            let target_path = match parse_sockaddr_un(dest_addr as *const u8, addrlen) {
                Ok(p) => p,
                Err(e) => return e.into(),
            };

            let target_sock = match find_unix_socket(&target_path) {
                Some(s) => s,
                None => return Errno::ECONNREFUSED.into(),
            };

            let src_path = unix_sock.lock().path.clone();

            let mut target = target_sock.lock();
            target.dgram_recv_queue.push_back(UnixDatagram {
                src_path,
                data: kernel_buf,
            });
            target.wait_queue.wake_all();
            return len as SyscallResult;
        }
    }

    if let Some(socket) = file_desc.inode.as_socket() {
        if !dest_addr.is_null() {
            if addrlen < 2 {
                return Errno::EINVAL.into();
            }
            if !validate_user_ptr(dest_addr as *const u8, 2) {
                return Errno::EFAULT.into();
            }
            let family = unsafe { core::ptr::read_unaligned(dest_addr as *const u16) };
            if family == 10 {
                return Errno::ENETUNREACH.into();
            }
            if family != 2 {
                return Errno::EAFNOSUPPORT.into();
            }
            if addrlen < 16 {
                return Errno::EINVAL.into();
            }
            if !validate_user_ptr(dest_addr as *const u8, core::mem::size_of::<SockAddrIn>()) {
                return Errno::EFAULT.into();
            }
            let addr = unsafe { core::ptr::read_unaligned(dest_addr) };

            let remote_ip = Ipv4Addr::new(
                addr.sin_addr[0],
                addr.sin_addr[1],
                addr.sin_addr[2],
                addr.sin_addr[3],
            );
            let remote_port = u16::from_be(addr.sin_port);

            let (sock_type, local_ip, local_port) = {
                let mut sock = socket.lock();
                if sock.local_port.is_none() || sock.local_port == Some(0) {
                    sock.local_port = Some(alloc_ephemeral_port());
                }
                if sock.local_addr.is_none() || sock.local_addr == Some(Ipv4Addr::UNSPECIFIED) {
                    let ip = if remote_ip.is_loopback() {
                        Ipv4Addr::LOCALHOST
                    } else {
                        crate::net::interface::get_first_ethernet_interface()
                            .map(|(ip, _)| ip)
                            .unwrap_or(Ipv4Addr::new(10, 0, 2, 15))
                    };
                    sock.local_addr = Some(ip);
                }
                (
                    sock.sock_type,
                    sock.local_addr.unwrap(),
                    sock.local_port.unwrap(),
                )
            };
            let sock_proto = socket.lock().protocol;
            if sock_proto == 1 {
                if let Err(_) = crate::net::ipv4::send_packet(
                    local_ip,
                    remote_ip,
                    crate::net::ipv4::PROTO_ICMP,
                    &kernel_buf,
                ) {
                    return Errno::ENETUNREACH.into();
                }
                return len as SyscallResult;
            } else if sock_type == 2 {
                let mut udp_buf = [0u8; 2048];
                let udp_len = match crate::net::udp::build_datagram(
                    &mut udp_buf,
                    local_ip,
                    remote_ip,
                    local_port,
                    remote_port,
                    &kernel_buf,
                ) {
                    Some(l) => l,
                    None => return Errno::EINVAL.into(),
                };

                if let Err(_) = crate::net::ipv4::send_packet(
                    local_ip,
                    remote_ip,
                    crate::net::ipv4::PROTO_UDP,
                    &udp_buf[..udp_len],
                ) {
                    return Errno::ENETUNREACH.into();
                }
                return len as SyscallResult;
            } else if sock_type == 3 {
                if let Err(_) = crate::net::ipv4::send_packet(
                    local_ip,
                    remote_ip,
                    sock_proto as u8,
                    &kernel_buf,
                ) {
                    return Errno::ENETUNREACH.into();
                }
                return len as SyscallResult;
            } else {
                return Errno::EINVAL.into();
            }
        }
    }

    match file_desc.write(&kernel_buf) {
        Ok(n) => n as SyscallResult,
        Err(e) => e as SyscallResult,
    }
}

/// Internal helper to read datagram or stream data directly into a kernel buffer.
pub fn recvfrom_kernel(
    fd: i32,
    dest: &mut [u8],
    flags: i32,
) -> Result<(usize, Option<SockAddrIn>, Option<String>), SyscallResult> {
    if dest.is_empty() {
        return Ok((0, None, None));
    }

    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Err(Errno::EBADF.into()),
    };

    if let Some(unix_sock) = file_desc.inode.as_unix_socket() {
        let mut sock = unix_sock.lock();
        if sock.sock_type == 2 {
            let tok = sock.wait_queue.token();
            if sock.dgram_recv_queue.is_empty() {
                let flags_guard = file_desc.flags.lock();
                if sock.nonblocking
                    || (flags_guard.0 & OpenFlags::O_NONBLOCK != 0)
                    || (flags & 0x40 != 0)
                {
                    return Err(-11); // -EAGAIN
                }
                drop(flags_guard);
                let wq = sock.wait_queue.clone();
                drop(sock);
                wq.wait_since(tok);
                sock = unix_sock.lock();
            }

            if let Some(dg) = sock.dgram_recv_queue.pop_front() {
                let n = dest.len().min(dg.data.len());
                dest[..n].copy_from_slice(&dg.data[..n]);
                return Ok((n, None, dg.src_path));
            }
            return Ok((0, None, None));
        }
    }

    if let Some(socket) = file_desc.inode.as_socket() {
        let mut sock = socket.lock();
        if sock.sock_type == 2 || sock.sock_type == 3 {
            let tok = sock.wait_queue.token();
            if sock.udp_recv_queue.is_empty() {
                let flags_guard = file_desc.flags.lock();
                if sock.nonblocking
                    || (flags_guard.0 & OpenFlags::O_NONBLOCK != 0)
                    || (flags & 0x40 != 0)
                {
                    return Err(-11);
                }
                drop(flags_guard);
                let wq = sock.wait_queue.clone();
                drop(sock);
                wq.wait_since(tok);
                sock = socket.lock();
            }

            if let Some(dg) = sock.udp_recv_queue.pop_front() {
                let n = dest.len().min(dg.data.len());
                dest[..n].copy_from_slice(&dg.data[..n]);

                let sin = SockAddrIn {
                    sin_family: 2,
                    sin_port: dg.src_port.to_be(),
                    sin_addr: dg.src_addr.octets,
                    sin_zero: [0; 8],
                };
                return Ok((n, Some(sin), None));
            }
            return Ok((0, None, None));
        }
    }

    match file_desc.read(dest) {
        Ok(n) => {
            let sin = if let Some(socket) = file_desc.inode.as_socket() {
                let child_sock = socket.lock();
                let remote_ip = child_sock.remote_addr.unwrap_or(Ipv4Addr::LOCALHOST);
                let remote_port = child_sock.remote_port.unwrap_or(0);
                Some(SockAddrIn {
                    sin_family: 2,
                    sin_port: remote_port.to_be(),
                    sin_addr: remote_ip.octets,
                    sin_zero: [0; 8],
                })
            } else {
                None
            };
            Ok((n, sin, None))
        }
        Err(e) => Err(e as SyscallResult),
    }
}

/// `recvfrom(fd, buf, len, flags, src_addr, addrlen_ptr)` — receive a message from a socket.
pub fn sys_recvfrom(
    fd: i32,
    buf: *mut u8,
    len: usize,
    flags: i32,
    src_addr: *mut SockAddrIn,
    addrlen_ptr: *mut u32,
) -> SyscallResult {
    if buf.is_null() || len == 0 {
        return 0;
    }
    if crate::syscall::fs::validate_user_ptr_write(buf, len).is_err() {
        return Errno::EFAULT.into();
    }

    let mut kernel_buf = alloc::vec![0u8; len.min(65536)];
    let (n, maybe_sin, maybe_unix_path) = match recvfrom_kernel(fd, &mut kernel_buf, flags) {
        Ok(res) => res,
        Err(e) => return e,
    };

    unsafe {
        core::ptr::copy_nonoverlapping(kernel_buf.as_ptr(), buf, n);
    }

    if !src_addr.is_null() && !addrlen_ptr.is_null() {
        if let Some(sin) = maybe_sin {
            if crate::syscall::fs::validate_user_ptr_write(
                src_addr as *mut u8,
                core::mem::size_of::<SockAddrIn>(),
            )
            .is_ok()
                && crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_ok()
            {
                unsafe {
                    src_addr.write(sin);
                    addrlen_ptr.write(16);
                }
            }
        } else if let Some(path) = maybe_unix_path {
            let path_bytes = path.as_bytes();
            let copy_len = path_bytes.len().min(107);
            let needed_len = (2 + copy_len + 1) as u32;

            if crate::syscall::fs::validate_user_ptr_write(src_addr as *mut u8, needed_len as usize)
                .is_ok()
                && crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_ok()
            {
                unsafe {
                    let un_ptr = src_addr as *mut SockAddrUn;
                    (*un_ptr).sun_family = 1; // AF_UNIX
                    core::ptr::copy_nonoverlapping(
                        path_bytes.as_ptr(),
                        (*un_ptr).sun_path.as_mut_ptr(),
                        copy_len,
                    );
                    (*un_ptr).sun_path[copy_len] = 0;
                    addrlen_ptr.write(needed_len);
                }
            }
        } else {
            if crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_ok() {
                unsafe {
                    addrlen_ptr.write(0);
                }
            }
        }
    }

    n as SyscallResult
}

/// `shutdown(fd, how)` — shut down part of a full-duplex connection.
pub fn sys_shutdown(fd: i32, _how: i32) -> SyscallResult {
    if proc_fd::current_task_get_file_desc(fd).is_none() {
        return Errno::EBADF.into();
    }
    0
}

/// `getsockname(fd, addr_ptr, addrlen_ptr)` — get socket name.
pub fn sys_getsockname(fd: i32, addr_ptr: *mut SockAddrIn, addrlen_ptr: *mut u32) -> SyscallResult {
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    if addr_ptr.is_null() || addrlen_ptr.is_null() {
        return Errno::EFAULT.into();
    }

    if let Some(unix_sock) = file_desc.inode.as_unix_socket() {
        let sock = unix_sock.lock();
        if let Some(ref path) = sock.path {
            let path_bytes = path.as_bytes();
            let copy_len = path_bytes.len().min(107);
            let needed_len = (2 + copy_len + 1) as u32;

            if crate::syscall::fs::validate_user_ptr_write(addr_ptr as *mut u8, needed_len as usize)
                .is_err()
                || crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err()
            {
                return Errno::EFAULT.into();
            }

            unsafe {
                let un_ptr = addr_ptr as *mut SockAddrUn;
                (*un_ptr).sun_family = 1; // AF_UNIX
                core::ptr::copy_nonoverlapping(
                    path_bytes.as_ptr(),
                    (*un_ptr).sun_path.as_mut_ptr(),
                    copy_len,
                );
                (*un_ptr).sun_path[copy_len] = 0;
                addrlen_ptr.write(needed_len);
            }
            return 0;
        } else {
            if crate::syscall::fs::validate_user_ptr_write(addr_ptr as *mut u8, 2).is_err()
                || crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err()
            {
                return Errno::EFAULT.into();
            }
            unsafe {
                (addr_ptr as *mut u16).write(1); // AF_UNIX
                addrlen_ptr.write(2);
            }
            return 0;
        }
    }

    if crate::syscall::fs::validate_user_ptr_write(
        addr_ptr as *mut u8,
        core::mem::size_of::<SockAddrIn>(),
    )
    .is_err()
        || crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err()
    {
        return Errno::EFAULT.into();
    }

    if let Some(socket) = file_desc.inode.as_socket() {
        let sock = socket.lock();
        let local_ip = sock.local_addr.unwrap_or(Ipv4Addr::LOCALHOST);
        let local_port = sock.local_port.unwrap_or(0);

        unsafe {
            addr_ptr.write(SockAddrIn {
                sin_family: 2,
                sin_port: local_port.to_be(),
                sin_addr: local_ip.octets,
                sin_zero: [0; 8],
            });
            addrlen_ptr.write(16);
        }
    } else {
        unsafe {
            addr_ptr.write(SockAddrIn {
                sin_family: 1, // AF_UNIX
                sin_port: 0,
                sin_addr: [0; 4],
                sin_zero: [0; 8],
            });
            addrlen_ptr.write(2);
        }
    }

    0
}

/// `getpeername(fd, addr_ptr, addrlen_ptr)` — get name of connected peer socket.
pub fn sys_getpeername(fd: i32, addr_ptr: *mut SockAddrIn, addrlen_ptr: *mut u32) -> SyscallResult {
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    if addr_ptr.is_null() || addrlen_ptr.is_null() {
        return Errno::EFAULT.into();
    }

    if let Some(unix_sock) = file_desc.inode.as_unix_socket() {
        let sock = unix_sock.lock();
        let peer_path = match sock.peer {
            Some(ref p) => p.lock().path.clone(),
            None => None,
        };

        if let Some(ref path) = peer_path {
            let path_bytes = path.as_bytes();
            let copy_len = path_bytes.len().min(107);
            let needed_len = (2 + copy_len + 1) as u32;

            if crate::syscall::fs::validate_user_ptr_write(addr_ptr as *mut u8, needed_len as usize)
                .is_err()
                || crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err()
            {
                return Errno::EFAULT.into();
            }

            unsafe {
                let un_ptr = addr_ptr as *mut SockAddrUn;
                (*un_ptr).sun_family = 1; // AF_UNIX
                core::ptr::copy_nonoverlapping(
                    path_bytes.as_ptr(),
                    (*un_ptr).sun_path.as_mut_ptr(),
                    copy_len,
                );
                (*un_ptr).sun_path[copy_len] = 0;
                addrlen_ptr.write(needed_len);
            }
            return 0;
        } else {
            if crate::syscall::fs::validate_user_ptr_write(addr_ptr as *mut u8, 2).is_err()
                || crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err()
            {
                return Errno::EFAULT.into();
            }
            unsafe {
                (addr_ptr as *mut u16).write(1); // AF_UNIX
                addrlen_ptr.write(2);
            }
            return 0;
        }
    }

    if crate::syscall::fs::validate_user_ptr_write(
        addr_ptr as *mut u8,
        core::mem::size_of::<SockAddrIn>(),
    )
    .is_err()
        || crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err()
    {
        return Errno::EFAULT.into();
    }

    if let Some(socket) = file_desc.inode.as_socket() {
        let sock = socket.lock();
        let remote_ip = sock.remote_addr.unwrap_or(Ipv4Addr::LOCALHOST);
        let remote_port = sock.remote_port.unwrap_or(0);

        unsafe {
            addr_ptr.write(SockAddrIn {
                sin_family: 2,
                sin_port: remote_port.to_be(),
                sin_addr: remote_ip.octets,
                sin_zero: [0; 8],
            });
            addrlen_ptr.write(16);
        }
    } else {
        unsafe {
            addr_ptr.write(SockAddrIn {
                sin_family: 1, // AF_UNIX
                sin_port: 0,
                sin_addr: [0; 4],
                sin_zero: [0; 8],
            });
            addrlen_ptr.write(2);
        }
    }

    0
}

/// `setsockopt(fd, level, optname, optval, optlen)` — set options on sockets.
pub fn sys_setsockopt(
    fd: i32,
    _level: i32,
    _optname: i32,
    _optval: *const u8,
    _optlen: u32,
) -> SyscallResult {
    if proc_fd::current_task_get_file_desc(fd).is_none() {
        return Errno::EBADF.into();
    }
    0
}

/// `getsockopt(fd, level, optname, optval, optlen)` — get options on sockets.
pub fn sys_getsockopt(
    fd: i32,
    level: i32,
    optname: i32,
    optval: *mut u8,
    optlen: *mut u32,
) -> SyscallResult {
    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    if optval.is_null() || optlen.is_null() {
        return 0;
    }
    if crate::syscall::fs::validate_user_ptr_write(optlen as *mut u8, 4).is_err() {
        return Errno::EFAULT.into();
    }
    let max_len = unsafe { optlen.read() } as usize;
    if max_len < 4 || crate::syscall::fs::validate_user_ptr_write(optval, 4).is_err() {
        return 0;
    }

    let value: i32 = if level == 1 && optname == 4 {
        if let Some(socket) = file_desc.inode.as_socket() {
            let mut sock = socket.lock();
            let err = sock.so_error;
            sock.so_error = 0;
            err
        } else {
            0
        }
    } else {
        0
    };

    unsafe {
        (optval as *mut i32).write(value);
        optlen.write(4);
    }
    0
}

/// `socketpair(domain, type, protocol, sv)` — create a pair of connected sockets.
pub fn sys_socketpair(domain: i32, sock_type: i32, _protocol: i32, sv: *mut i32) -> SyscallResult {
    if sv.is_null() {
        return Errno::EFAULT.into();
    }
    if crate::syscall::fs::validate_user_ptr_write(sv as *mut u8, 8).is_err() {
        return Errno::EFAULT.into();
    }
    if domain == 10 && !crate::net::ipv6_supported() {
        return Errno::EAFNOSUPPORT.into();
    }
    if domain != 1 && domain != 2 {
        return Errno::EAFNOSUPPORT.into();
    }

    let nonblock = (sock_type & 0x800) != 0;
    let cloexec = (sock_type & 0x80000) != 0;
    let base_type = sock_type & !(0x800 | 0x80000);

    let mut open_flags = OpenFlags(OpenFlags::O_RDWR);
    if nonblock {
        open_flags.0 |= OpenFlags::O_NONBLOCK;
    }
    if cloexec {
        open_flags.0 |= OpenFlags::O_CLOEXEC;
    }

    if domain == 1 {
        // AF_UNIX / AF_LOCAL
        let (unix_a, unix_b) = UnixSocket::make_pair(base_type, nonblock);
        let inode_a = Arc::new(UnixSocketInode::new(unix_a));
        let inode_b = Arc::new(UnixSocketInode::new(unix_b));

        let fd0 = match proc_fd::current_task_alloc_fd_with_flags_and_path(
            inode_a,
            open_flags,
            Some(alloc::string::String::from("unix_socketpair:[0]")),
        ) {
            Some(fd) => fd,
            None => return Errno::EMFILE.into(),
        };

        let fd1 = match proc_fd::current_task_alloc_fd_with_flags_and_path(
            inode_b,
            open_flags,
            Some(alloc::string::String::from("unix_socketpair:[1]")),
        ) {
            Some(fd) => fd,
            None => {
                proc_fd::current_task_close_fd(fd0);
                return Errno::EMFILE.into();
            }
        };

        unsafe {
            sv.write(fd0);
            sv.add(1).write(fd1);
        }

        return 0;
    }

    let (sock_a, sock_b) = crate::fs::pipe::make_socketpair(nonblock);

    let fd0 = match proc_fd::current_task_alloc_fd_with_flags_and_path(
        sock_a,
        open_flags,
        Some(alloc::string::String::from("socketpair:[0]")),
    ) {
        Some(fd) => fd,
        None => return Errno::EMFILE.into(),
    };

    let fd1 = match proc_fd::current_task_alloc_fd_with_flags_and_path(
        sock_b,
        open_flags,
        Some(alloc::string::String::from("socketpair:[1]")),
    ) {
        Some(fd) => fd,
        None => {
            proc_fd::current_task_close_fd(fd0);
            return Errno::EMFILE.into();
        }
    };

    unsafe {
        sv.write(fd0);
        sv.add(1).write(fd1);
    }

    0
}

/// POSIX `iovec` structure.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IoVec {
    pub iov_base: *mut u8,
    pub iov_len: usize,
}

/// POSIX `msghdr` structure.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Msghdr {
    pub msg_name: *mut u8,
    pub msg_namelen: u32,
    pub msg_iov: *mut IoVec,
    pub msg_iovlen: usize,
    pub msg_control: *mut u8,
    pub msg_controllen: usize,
    pub msg_flags: i32,
}

/// Linux `mmsghdr` structure.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MMsghdr {
    pub msg_hdr: Msghdr,
    pub msg_len: u32,
}

/// `sendmsg(fd, msg, flags)` — send a message on a socket.
pub fn sys_sendmsg(fd: i32, msg_ptr: *const Msghdr, flags: i32) -> SyscallResult {
    if msg_ptr.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(msg_ptr as *const u8, core::mem::size_of::<Msghdr>()) {
        return Errno::EFAULT.into();
    }

    let msg = unsafe { core::ptr::read_volatile(msg_ptr) };

    if msg.msg_iovlen == 0 || msg.msg_iovlen > 1024 {
        return Errno::EINVAL.into();
    }
    let iov_total_size = match msg.msg_iovlen.checked_mul(core::mem::size_of::<IoVec>()) {
        Some(sz) => sz,
        None => return Errno::EINVAL.into(),
    };
    if !validate_user_ptr(msg.msg_iov as *const u8, iov_total_size) {
        return Errno::EFAULT.into();
    }

    let iovecs = unsafe { core::slice::from_raw_parts(msg.msg_iov, msg.msg_iovlen) };

    let mut total_len = 0usize;
    for iov in iovecs {
        total_len = match total_len.checked_add(iov.iov_len) {
            Some(l) => l,
            None => return Errno::EINVAL.into(),
        };
        if iov.iov_len > 0 && !validate_user_ptr(iov.iov_base as *const u8, iov.iov_len) {
            return Errno::EFAULT.into();
        }
    }

    if total_len == 0 {
        return 0;
    }

    let mut buf = alloc::vec::Vec::with_capacity(total_len.min(65536));
    for iov in iovecs {
        if iov.iov_len > 0 {
            let chunk_len = iov.iov_len.min(65536 - buf.len());
            let slice =
                unsafe { core::slice::from_raw_parts(iov.iov_base as *const u8, chunk_len) };
            buf.extend_from_slice(slice);
            if buf.len() >= 65536 {
                break;
            }
        }
    }

    let addr_ptr = if !msg.msg_name.is_null() && msg.msg_namelen >= 2 {
        if !validate_user_ptr(msg.msg_name as *const u8, msg.msg_namelen as usize) {
            return Errno::EFAULT.into();
        }
        msg.msg_name as *const SockAddrIn
    } else {
        core::ptr::null()
    };

    sys_sendto(
        fd,
        buf.as_ptr(),
        buf.len(),
        flags,
        addr_ptr,
        msg.msg_namelen,
    )
}

/// `recvmsg(fd, msg, flags)` — receive a message from a socket.
pub fn sys_recvmsg(fd: i32, msg_ptr: *mut Msghdr, flags: i32) -> SyscallResult {
    if msg_ptr.is_null() {
        return Errno::EFAULT.into();
    }
    if crate::syscall::fs::validate_user_ptr_write(
        msg_ptr as *mut u8,
        core::mem::size_of::<Msghdr>(),
    )
    .is_err()
    {
        return Errno::EFAULT.into();
    }

    let mut msg = unsafe { core::ptr::read_volatile(msg_ptr) };

    if msg.msg_iovlen == 0 || msg.msg_iovlen > 1024 {
        return Errno::EINVAL.into();
    }
    let iov_total_size = match msg.msg_iovlen.checked_mul(core::mem::size_of::<IoVec>()) {
        Some(sz) => sz,
        None => return Errno::EINVAL.into(),
    };
    if !crate::syscall::fs::validate_user_ptr(msg.msg_iov as *const u8, iov_total_size) {
        return Errno::EFAULT.into();
    }

    let iovecs = unsafe { core::slice::from_raw_parts(msg.msg_iov, msg.msg_iovlen) };

    let mut total_cap = 0usize;
    for iov in iovecs.iter() {
        total_cap = match total_cap.checked_add(iov.iov_len) {
            Some(c) => c,
            None => return Errno::EINVAL.into(),
        };
        if iov.iov_len > 0
            && crate::syscall::fs::validate_user_ptr_write(iov.iov_base, iov.iov_len).is_err()
        {
            return Errno::EFAULT.into();
        }
    }

    if total_cap == 0 {
        return 0;
    }

    let mut buf = alloc::vec![0u8; total_cap.min(65536)];

    let (bytes_received, maybe_sin, maybe_unix_path) = match recvfrom_kernel(fd, &mut buf, flags) {
        Ok(res) => res,
        Err(e) => return e,
    };

    let mut bytes_copied = 0usize;
    for iov in iovecs.iter() {
        if bytes_copied >= bytes_received {
            break;
        }
        let to_copy = (bytes_received - bytes_copied).min(iov.iov_len);
        if to_copy > 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    buf.as_ptr().add(bytes_copied),
                    iov.iov_base,
                    to_copy,
                );
            }
            bytes_copied += to_copy;
        }
    }

    if !msg.msg_name.is_null() && msg.msg_namelen >= 2 {
        if let Some(sin) = maybe_sin {
            if crate::syscall::fs::validate_user_ptr_write(
                msg.msg_name,
                core::mem::size_of::<SockAddrIn>(),
            )
            .is_ok()
            {
                unsafe {
                    core::ptr::write(msg.msg_name as *mut SockAddrIn, sin);
                }
                msg.msg_namelen = 16;
            }
        } else if let Some(path) = maybe_unix_path {
            let path_bytes = path.as_bytes();
            let copy_len = path_bytes.len().min(107);
            let needed_len = (2 + copy_len + 1) as u32;

            if crate::syscall::fs::validate_user_ptr_write(msg.msg_name, needed_len as usize)
                .is_ok()
            {
                unsafe {
                    let un_ptr = msg.msg_name as *mut SockAddrUn;
                    (*un_ptr).sun_family = 1;
                    core::ptr::copy_nonoverlapping(
                        path_bytes.as_ptr(),
                        (*un_ptr).sun_path.as_mut_ptr(),
                        copy_len,
                    );
                    (*un_ptr).sun_path[copy_len] = 0;
                    msg.msg_namelen = needed_len;
                }
            }
        }
    }

    msg.msg_flags = 0;
    unsafe {
        core::ptr::write(msg_ptr, msg);
    }

    bytes_received as SyscallResult
}

/// `sendmmsg(fd, msgvec, vlen, flags)` — send multiple messages on a socket.
pub fn sys_sendmmsg(fd: i32, msgvec: *mut MMsghdr, vlen: u32, flags: i32) -> SyscallResult {
    if vlen == 0 {
        return 0;
    }
    if msgvec.is_null() {
        return Errno::EFAULT.into();
    }
    let total_size = match (vlen as usize).checked_mul(core::mem::size_of::<MMsghdr>()) {
        Some(sz) => sz,
        None => return Errno::EINVAL.into(),
    };
    if crate::syscall::fs::validate_user_ptr_write(msgvec as *mut u8, total_size).is_err() {
        return Errno::EFAULT.into();
    }

    let mmsgs = unsafe { core::slice::from_raw_parts_mut(msgvec, vlen as usize) };
    let mut count = 0u32;

    for m in mmsgs.iter_mut() {
        let res = sys_sendmsg(fd, &m.msg_hdr as *const Msghdr, flags);
        if res < 0 {
            if count == 0 {
                return res;
            }
            break;
        }
        m.msg_len = res as u32;
        count += 1;
    }

    count as SyscallResult
}

/// `recvmmsg(fd, msgvec, vlen, flags, timeout)` — receive multiple messages on a socket.
pub fn sys_recvmmsg(
    fd: i32,
    msgvec: *mut MMsghdr,
    vlen: u32,
    flags: i32,
    _timeout: *const crate::syscall::fs::TimeSpec,
) -> SyscallResult {
    if vlen == 0 {
        return 0;
    }
    if msgvec.is_null() {
        return Errno::EFAULT.into();
    }
    let total_size = match (vlen as usize).checked_mul(core::mem::size_of::<MMsghdr>()) {
        Some(sz) => sz,
        None => return Errno::EINVAL.into(),
    };
    if crate::syscall::fs::validate_user_ptr_write(msgvec as *mut u8, total_size).is_err() {
        return Errno::EFAULT.into();
    }

    let mmsgs = unsafe { core::slice::from_raw_parts_mut(msgvec, vlen as usize) };
    let mut count = 0u32;

    for m in mmsgs.iter_mut() {
        let res = sys_recvmsg(fd, &mut m.msg_hdr as *mut Msghdr, flags);
        if res < 0 {
            if count == 0 {
                return res;
            }
            break;
        }
        m.msg_len = res as u32;
        count += 1;
    }

    count as SyscallResult
}
