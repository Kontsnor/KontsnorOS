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

//! Network socket system calls.

use crate::fs::file::OpenFlags;
use crate::net::ipv4::Ipv4Addr;
use crate::net::socket::{Socket, SocketInode};
use crate::process::fd as proc_fd;
use crate::syscall::fs::validate_user_ptr;
use crate::syscall::{Errno, SyscallResult};
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

/// Helper to get the inner socket from a file descriptor.
fn get_socket(fd: i32) -> Option<Arc<Mutex<Socket>>> {
    let file_desc = proc_fd::current_task_get_file_desc(fd)?;
    file_desc.inode.as_socket()
}

/// `socket(domain, type, protocol)` — create an endpoint for communication.
pub fn sys_socket(domain: i32, sock_type: i32, protocol: i32) -> SyscallResult {
    if domain != 2 {
        // Only support AF_INET (2)
        return Errno::EINVAL.into();
    }
    if sock_type != 1 && sock_type != 2 {
        // Support SOCK_STREAM (1) and SOCK_DGRAM (2)
        return Errno::EINVAL.into();
    }

    let socket = Arc::new(Mutex::new(Socket::new(domain, sock_type, protocol)));
    crate::net::socket::register_socket(socket.clone());

    let inode = Arc::new(SocketInode::new(socket));
    match proc_fd::current_task_alloc_fd_with_flags(inode, OpenFlags(OpenFlags::O_RDWR)) {
        Some(fd) => fd as SyscallResult,
        None => Errno::EMFILE.into(),
    }
}

/// `bind(fd, addr_ptr, addrlen)` — bind a name to a socket.
pub fn sys_bind(fd: i32, addr_ptr: *const SockAddrIn, addrlen: u32) -> SyscallResult {
    if addr_ptr.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(addr_ptr as *const u8, core::mem::size_of::<SockAddrIn>()) {
        return Errno::EFAULT.into();
    }
    if addrlen < 16 {
        return Errno::EINVAL.into();
    }

    let addr = unsafe { core::ptr::read_volatile(addr_ptr) };
    if addr.sin_family != 2 {
        return Errno::EINVAL.into();
    }

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
    let local_port = u16::from_be(addr.sin_port);

    let mut sock = socket.lock();
    sock.local_addr = Some(local_ip);
    sock.local_port = Some(local_port);

    0 // Success
}

/// `connect(fd, addr_ptr, addrlen)` — initiate a connection on a socket.
pub fn sys_connect(fd: i32, addr_ptr: *const SockAddrIn, addrlen: u32) -> SyscallResult {
    if addr_ptr.is_null() {
        return Errno::EFAULT.into();
    }
    if !validate_user_ptr(addr_ptr as *const u8, core::mem::size_of::<SockAddrIn>()) {
        return Errno::EFAULT.into();
    }
    if addrlen < 16 {
        return Errno::EINVAL.into();
    }

    let addr = unsafe { core::ptr::read_volatile(addr_ptr) };
    if addr.sin_family != 2 {
        return Errno::EINVAL.into();
    }

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

        if sock_type == 2 {
            // UDP connect is stateless, just sets destination
            return 0;
        }

        // For TCP, choose local bindings if not yet set
        if sock.local_port.is_none() {
            sock.local_port = Some((50000 + fd) as u16);
        }
        if sock.local_addr.is_none() {
            sock.local_addr = Some(Ipv4Addr::LOCALHOST);
        }

        // Transmit TCP SYN
        sock.tcp_state = crate::net::tcp::TcpState::SynSent;
        sock.tcp_snd_nxt = 1000;
        sock.tcp_snd_una = 1000;

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
        local_port,
        remote_port,
        tcp_snd_nxt,
        0,
        crate::net::tcp::TCP_SYN,
        &[],
    ) {
        let _ = crate::net::ipv4::send_packet(
            local_ip,
            remote_ip,
            crate::net::ipv4::PROTO_TCP,
            &tcp_buf[..tcp_len],
        );
    }

    // Wait until state changes to Established or Closed (error)
    loop {
        {
            let sock = socket.lock();
            tcp_state = sock.tcp_state;
        }
        if tcp_state == crate::net::tcp::TcpState::Established {
            break;
        }
        if tcp_state == crate::net::tcp::TcpState::Closed {
            return -111; // ECONNREFUSED
        }
        let wq = {
            let sock = socket.lock();
            sock.wait_queue.clone()
        };
        wq.wait();
    }

    0
}

/// `listen(fd, backlog)` — listen for connections on a socket.
pub fn sys_listen(fd: i32, backlog: i32) -> SyscallResult {
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
    let socket = match get_socket(fd) {
        Some(s) => s,
        None => return Errno::EBADF.into(),
    };

    let child = loop {
        let wq;
        {
            let mut sock = socket.lock();
            if sock.sock_type != 1 || sock.tcp_state != crate::net::tcp::TcpState::Listen {
                return Errno::EINVAL.into();
            }

            if !sock.tcp_backlog.is_empty() {
                break sock.tcp_backlog.remove(0);
            }
            wq = sock.wait_queue.clone();
        }
        wq.wait();
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

    let inode = Arc::new(SocketInode::new(child));
    match proc_fd::current_task_alloc_fd_with_flags(inode, OpenFlags(OpenFlags::O_RDWR)) {
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
    // SAFETY: buf is validated user pointer with length len.
    unsafe {
        core::ptr::copy_nonoverlapping(buf, kernel_buf.as_mut_ptr(), len);
    }

    if let Some(socket) = file_desc.inode.as_socket() {
        if !dest_addr.is_null() {
            if !validate_user_ptr(dest_addr as *const u8, core::mem::size_of::<SockAddrIn>()) {
                return Errno::EFAULT.into();
            }
            if addrlen < 16 {
                return Errno::EINVAL.into();
            }
            // SAFETY: dest_addr is validated above.
            let addr = unsafe { core::ptr::read_volatile(dest_addr) };
            if addr.sin_family != 2 {
                return Errno::EINVAL.into();
            }

            let remote_ip = Ipv4Addr::new(
                addr.sin_addr[0],
                addr.sin_addr[1],
                addr.sin_addr[2],
                addr.sin_addr[3],
            );
            let remote_port = u16::from_be(addr.sin_port);

            let (sock_type, local_ip, local_port) = {
                let sock = socket.lock();
                (
                    sock.sock_type,
                    sock.local_addr.unwrap_or(Ipv4Addr::LOCALHOST),
                    sock.local_port.unwrap_or(50000),
                )
            };
            if sock_type == 2 {
                // UDP
                let mut udp_buf = [0u8; 2048];
                let udp_len = match crate::net::udp::build_datagram(
                    &mut udp_buf,
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

/// `recvfrom(fd, buf, len, flags, src_addr, addrlen_ptr)` — receive a message from a socket.
pub fn sys_recvfrom(
    fd: i32,
    buf: *mut u8,
    len: usize,
    _flags: i32,
    src_addr: *mut SockAddrIn,
    addrlen_ptr: *mut u32,
) -> SyscallResult {
    if buf.is_null() || len == 0 {
        return 0;
    }
    if crate::syscall::fs::validate_user_ptr_write(buf, len).is_err() {
        return Errno::EFAULT.into();
    }

    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Errno::EBADF.into(),
    };

    if let Some(socket) = file_desc.inode.as_socket() {
        let mut sock = socket.lock();
        if sock.sock_type == 2 {
            // UDP
            if sock.udp_recv_queue.is_empty() {
                let wq = sock.wait_queue.clone();
                drop(sock);
                wq.wait();
                sock = socket.lock();
            }

            if let Some(dg) = sock.udp_recv_queue.pop_front() {
                let n = len.min(dg.data.len());
                // SAFETY: buf is validated user pointer for write access with length len.
                unsafe {
                    core::ptr::copy_nonoverlapping(dg.data.as_ptr(), buf, n);
                }

                if !src_addr.is_null() && !addrlen_ptr.is_null() {
                    if crate::syscall::fs::validate_user_ptr_write(
                        src_addr as *mut u8,
                        core::mem::size_of::<SockAddrIn>(),
                    )
                    .is_err()
                        || crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4)
                            .is_err()
                    {
                        return Errno::EFAULT.into();
                    }

                    // SAFETY: pointers are validated for write above.
                    unsafe {
                        src_addr.write(SockAddrIn {
                            sin_family: 2,
                            sin_port: dg.src_port.to_be(),
                            sin_addr: dg.src_addr.octets,
                            sin_zero: [0; 8],
                        });
                        addrlen_ptr.write(16);
                    }
                }

                return n as SyscallResult;
            }
            return 0;
        }
    }

    // Stream / TCP / Pipe / Socketpair
    let mut kernel_buf = alloc::vec![0u8; len];
    match file_desc.read(&mut kernel_buf) {
        Ok(n) => {
            // SAFETY: buf is validated user pointer for write access.
            unsafe {
                core::ptr::copy_nonoverlapping(kernel_buf.as_ptr(), buf, n);
            }
            if !src_addr.is_null() && !addrlen_ptr.is_null() {
                if crate::syscall::fs::validate_user_ptr_write(
                    src_addr as *mut u8,
                    core::mem::size_of::<SockAddrIn>(),
                )
                .is_err()
                    || crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4)
                        .is_err()
                {
                    return Errno::EFAULT.into();
                }

                if let Some(socket) = file_desc.inode.as_socket() {
                    let child_sock = socket.lock();
                    let remote_ip = child_sock.remote_addr.unwrap_or(Ipv4Addr::LOCALHOST);
                    let remote_port = child_sock.remote_port.unwrap_or(0);

                    // SAFETY: pointers are validated for write above.
                    unsafe {
                        src_addr.write(SockAddrIn {
                            sin_family: 2,
                            sin_port: remote_port.to_be(),
                            sin_addr: remote_ip.octets,
                            sin_zero: [0; 8],
                        });
                        addrlen_ptr.write(16);
                    }
                } else {
                    // SAFETY: addrlen_ptr is validated above.
                    unsafe {
                        addrlen_ptr.write(0);
                    }
                }
            }
            n as SyscallResult
        }
        Err(e) => e as SyscallResult,
    }
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

        // SAFETY: pointers are validated for write above.
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
        // AF_UNIX / socketpair
        // SAFETY: pointers are validated for write above.
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

        // SAFETY: pointers are validated for write above.
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
        // AF_UNIX / socketpair
        // SAFETY: pointers are validated for write above.
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
    _level: i32,
    optname: i32,
    optval: *mut u8,
    optlen: *mut u32,
) -> SyscallResult {
    if proc_fd::current_task_get_file_desc(fd).is_none() {
        return Errno::EBADF.into();
    }
    if !optval.is_null() && !optlen.is_null() {
        if crate::syscall::fs::validate_user_ptr_write(optlen as *mut u8, 4).is_ok() {
            // SAFETY: optlen pointer validated above.
            let max_len = unsafe { optlen.read() } as usize;
            if max_len >= 4 && crate::syscall::fs::validate_user_ptr_write(optval, 4).is_ok() {
                // Return 0 (no error) for SO_ERROR (optname 4)
                if optname == 4 {
                    // SAFETY: optval and optlen validated above.
                    unsafe {
                        (optval as *mut i32).write(0);
                        optlen.write(4);
                    }
                }
            }
        }
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
    // Support AF_UNIX / AF_LOCAL (1) and AF_INET (2)
    if domain != 1 && domain != 2 {
        return Errno::EINVAL.into();
    }

    let nonblock = (sock_type & 0x800) != 0;
    let cloexec = (sock_type & 0x80000) != 0;

    let mut open_flags = OpenFlags(OpenFlags::O_RDWR);
    if nonblock {
        open_flags.0 |= OpenFlags::O_NONBLOCK;
    }
    if cloexec {
        open_flags.0 |= OpenFlags::O_CLOEXEC;
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

    // SAFETY: sv pointer was validated for write access above.
    unsafe {
        sv.write(fd0);
        sv.add(1).write(fd1);
    }

    0
}
