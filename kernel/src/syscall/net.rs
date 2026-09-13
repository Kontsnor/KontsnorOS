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
        let (end_a, _end_b) = crate::fs::pipe::make_socketpair(nonblock);
        let mut open_flags = OpenFlags(OpenFlags::O_RDWR);
        if nonblock {
            open_flags.0 |= OpenFlags::O_NONBLOCK;
        }
        if cloexec {
            open_flags.0 |= OpenFlags::O_CLOEXEC;
        }
        return match proc_fd::current_task_alloc_fd_with_flags(end_a, open_flags) {
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

    // Read family (first 2 bytes of any sockaddr)
    // SAFETY: addr_ptr is non-null and validated for at least 2 bytes.
    let family = unsafe { core::ptr::read_unaligned(addr_ptr as *const u16) };
    if family == 10
    /* AF_INET6 */
    {
        return Errno::EAFNOSUPPORT.into();
    }
    if family != 2
    /* AF_INET */
    {
        return Errno::EINVAL.into();
    }
    if addrlen < 16 {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(addr_ptr as *const u8, core::mem::size_of::<SockAddrIn>()) {
        return Errno::EFAULT.into();
    }

    // SAFETY: addr_ptr is validated for size_of::<SockAddrIn>() bytes.
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

    0 // Success
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

    // Read address family (first 2 bytes of any sockaddr struct)
    // SAFETY: addr_ptr is non-null and validated for at least 2 bytes.
    let family = unsafe { core::ptr::read_unaligned(addr_ptr as *const u16) };
    if family == 10
    /* AF_INET6 */
    {
        // Fast-fail unroutable IPv6 connection attempts immediately.
        // Returning -ENETUNREACH prompts dual-stack clients (e.g. curl/libcurl)
        // to immediately fall back to IPv4 without hanging.
        return Errno::ENETUNREACH.into();
    }
    if family != 2
    /* AF_INET */
    {
        return Errno::EAFNOSUPPORT.into();
    }

    if addrlen < 16 {
        return Errno::EINVAL.into();
    }
    if !validate_user_ptr(addr_ptr as *const u8, core::mem::size_of::<SockAddrIn>()) {
        return Errno::EFAULT.into();
    }

    // SAFETY: addr_ptr is validated for size_of::<SockAddrIn>() bytes.
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

    // Fast-fail unroutable IPv4 destinations
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

        // For all sockets, ensure local port and IP are bound
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
            // UDP connect is stateless, sets destination & bound local endpoint
            return 0;
        }

        // Transmit TCP SYN
        sock.tcp_state = crate::net::tcp::TcpState::SynSent;
        sock.tcp_snd_nxt = 1000;
        sock.tcp_snd_una = 1000;
        // Mark that this socket has initiated a connect so that poll() can
        // report POLLERR/POLLHUP if it transitions back to Closed.
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
        return -115; // -EINPROGRESS
    }

    crate::kprintln!("[sys_connect] Blocking wait for connection establishment...");
    // Wait until state changes to Established or Closed (error) with timeout
    let start_ticks = crate::arch::x86_64::interrupts::timer_ticks();
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
        if crate::arch::x86_64::interrupts::timer_ticks() - start_ticks > 100 {
            let mut sock = socket.lock();
            sock.tcp_state = crate::net::tcp::TcpState::Closed;
            return -110; // ETIMEDOUT
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
            if (flags & sock_nonblock) != 0 {
                return Errno::EAGAIN.into();
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

    let mut open_flags = OpenFlags::O_RDWR;
    if (flags & sock_cloexec) != 0 {
        open_flags |= OpenFlags::O_CLOEXEC;
    }

    let inode = Arc::new(SocketInode::new(child));
    match proc_fd::current_task_alloc_fd_with_flags(inode, OpenFlags(open_flags)) {
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
            if addrlen < 2 {
                return Errno::EINVAL.into();
            }
            if !validate_user_ptr(dest_addr as *const u8, 2) {
                return Errno::EFAULT.into();
            }
            // SAFETY: dest_addr is non-null and validated for at least 2 bytes.
            let family = unsafe { core::ptr::read_unaligned(dest_addr as *const u16) };
            if family == 10
            /* AF_INET6 */
            {
                return Errno::ENETUNREACH.into();
            }
            if family != 2
            /* AF_INET */
            {
                return Errno::EAFNOSUPPORT.into();
            }
            if addrlen < 16 {
                return Errno::EINVAL.into();
            }
            if !validate_user_ptr(dest_addr as *const u8, core::mem::size_of::<SockAddrIn>()) {
                return Errno::EFAULT.into();
            }
            // SAFETY: dest_addr is validated above.
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
                // IPPROTO_ICMP (RAW or DGRAM ping socket)
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
                // UDP
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
                // SOCK_RAW (other protocols)
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
/// Does NOT perform user pointer validation on `dest`.
pub fn recvfrom_kernel(
    fd: i32,
    dest: &mut [u8],
    flags: i32,
) -> Result<(usize, Option<SockAddrIn>), SyscallResult> {
    if dest.is_empty() {
        return Ok((0, None));
    }

    let file_desc = match proc_fd::current_task_get_file_desc(fd) {
        Some(d) => d,
        None => return Err(Errno::EBADF.into()),
    };

    if let Some(socket) = file_desc.inode.as_socket() {
        let mut sock = socket.lock();
        if sock.sock_type == 2 || sock.sock_type == 3 {
            // UDP or RAW/ICMP
            if sock.udp_recv_queue.is_empty() {
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
                wq.wait();
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
                return Ok((n, Some(sin)));
            }
            {
                let flags_guard = file_desc.flags.lock();
                if sock.nonblocking
                    || (flags_guard.0 & OpenFlags::O_NONBLOCK != 0)
                    || (flags & 0x40 != 0)
                {
                    return Err(-11); // -EAGAIN
                }
            }
            return Ok((0, None));
        }
    }

    // Stream / TCP / Pipe / Socketpair
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
            Ok((n, sin))
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
    let (n, maybe_sin) = match recvfrom_kernel(fd, &mut kernel_buf, flags) {
        Ok(res) => res,
        Err(e) => return e,
    };

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
            || crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err()
        {
            return Errno::EFAULT.into();
        }

        if let Some(sin) = maybe_sin {
            // SAFETY: pointers are validated for write above.
            unsafe {
                src_addr.write(sin);
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
        return 0; // silently ignore as per POSIX
    }
    if crate::syscall::fs::validate_user_ptr_write(optlen as *mut u8, 4).is_err() {
        return Errno::EFAULT.into();
    }
    // SAFETY: optlen pointer validated above.
    let max_len = unsafe { optlen.read() } as usize;
    if max_len < 4 || crate::syscall::fs::validate_user_ptr_write(optval, 4).is_err() {
        return 0;
    }

    // SOL_SOCKET = 1, SOL_TCP = 6
    // SO_ERROR = 4, SO_RCVBUF = 8, SO_SNDBUF = 7, SO_KEEPALIVE = 9
    // TCP_NODELAY = 1 (at SOL_TCP), IP_TOS = 1 (at IPPROTO_IP = 0)
    //
    // Strategy: for SO_ERROR always report the real pending error; for all
    // other options return 0 (acceptable default) to avoid spurious failures.
    let value: i32 = if level == 1 && optname == 4 {
        // SOL_SOCKET / SO_ERROR — return and clear the pending socket error.
        if let Some(socket) = file_desc.inode.as_socket() {
            let mut sock = socket.lock();
            let err = sock.so_error;
            // Clear after reading, matching Linux semantics.
            sock.so_error = 0;
            err
        } else {
            0
        }
    } else {
        // For all other options (TCP_NODELAY, SO_KEEPALIVE, SO_RCVBUF, etc.)
        // return 0 so libcurl / musl do not abort the transfer.
        0
    };

    // SAFETY: optval and optlen validated above.
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
    // AF_INET6 (10) -> return EAFNOSUPPORT if IPv6 is not supported
    if domain == 10 && !crate::net::ipv6_supported() {
        return Errno::EAFNOSUPPORT.into();
    }
    // Support AF_UNIX / AF_LOCAL (1) and AF_INET (2)
    if domain != 1 && domain != 2 {
        return Errno::EAFNOSUPPORT.into();
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

    // SAFETY: Validated user pointer
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

    // SAFETY: Pointer and length validated
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
            // SAFETY: Memory bounds validated above
            let slice =
                unsafe { core::slice::from_raw_parts(iov.iov_base as *const u8, chunk_len) };
            buf.extend_from_slice(slice);
            if buf.len() >= 65536 {
                break;
            }
        }
    }

    let addr_ptr = if !msg.msg_name.is_null() && msg.msg_namelen >= 16 {
        if !validate_user_ptr(
            msg.msg_name as *const u8,
            core::mem::size_of::<SockAddrIn>(),
        ) {
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

    // SAFETY: Validated for write access
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

    // SAFETY: Pointer and length validated
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

    let (bytes_received, maybe_sin) = match recvfrom_kernel(fd, &mut buf, flags) {
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
            // SAFETY: Destination validated with validate_user_ptr_write
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

    if !msg.msg_name.is_null() && msg.msg_namelen >= 16 {
        if let Some(sin) = maybe_sin {
            if crate::syscall::fs::validate_user_ptr_write(
                msg.msg_name,
                core::mem::size_of::<SockAddrIn>(),
            )
            .is_ok()
            {
                // SAFETY: Target validated
                unsafe {
                    core::ptr::write(msg.msg_name as *mut SockAddrIn, sin);
                }
                msg.msg_namelen = 16;
            }
        }
    }

    msg.msg_flags = 0;
    // SAFETY: msg_ptr validated
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

    // SAFETY: msgvec validated above
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

    // SAFETY: msgvec validated above
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
