//! Network socket system calls.

use alloc::sync::Arc;
use spin::Mutex;
use crate::syscall::{Errno, SyscallResult};
use crate::net::ipv4::Ipv4Addr;
use crate::net::socket::{Socket, SocketInode};
use crate::fs::file::OpenFlags;
use crate::process::fd as proc_fd;
use crate::syscall::fs::validate_user_ptr;

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
    if domain != 2 { // Only support AF_INET (2)
        return Errno::EINVAL.into();
    }
    if sock_type != 1 && sock_type != 2 { // Support SOCK_STREAM (1) and SOCK_DGRAM (2)
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

    let addr = unsafe { &*addr_ptr };
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

    let addr = unsafe { &*addr_ptr };
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

        if sock_type == 2 { // UDP connect is stateless, just sets destination
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
        
        local_port = sock.local_port.unwrap();
        local_ip = sock.local_addr.unwrap();
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
    sock.tcp_max_backlog = backlog.max(1) as usize;
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
        if crate::syscall::fs::validate_user_ptr_write(addr_ptr as *mut u8, core::mem::size_of::<SockAddrIn>()).is_err() ||
           crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err() {
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

    let socket = match get_socket(fd) {
        Some(s) => s,
        None => return Errno::EBADF.into(),
    };

    let slice = unsafe { core::slice::from_raw_parts(buf, len) };

    if !dest_addr.is_null() {
        if !validate_user_ptr(dest_addr as *const u8, core::mem::size_of::<SockAddrIn>()) {
            return Errno::EFAULT.into();
        }
        if addrlen < 16 {
            return Errno::EINVAL.into();
        }
        let addr = unsafe { &*dest_addr };
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
        if sock_type == 2 { // UDP
            let mut udp_buf = [0u8; 2048];
            let udp_len = match crate::net::udp::build_datagram(&mut udp_buf, local_port, remote_port, slice) {
                Some(l) => l,
                None => return Errno::EINVAL.into(),
            };

            if let Err(_) = crate::net::ipv4::send_packet(local_ip, remote_ip, crate::net::ipv4::PROTO_UDP, &udp_buf[..udp_len]) {
                return Errno::ENETUNREACH.into();
            }
            return len as SyscallResult;
        } else {
            return Errno::EINVAL.into();
        }
    }

    let inode = match proc_fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };
    match inode.write(0, slice) {
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

    let socket = match get_socket(fd) {
        Some(s) => s,
        None => return Errno::EBADF.into(),
    };

    let mut sock = socket.lock();
    if sock.sock_type == 2 { // UDP
        if sock.udp_recv_queue.is_empty() {
            let wq = sock.wait_queue.clone();
            drop(sock);
            wq.wait();
            sock = socket.lock();
        }

        if let Some(dg) = sock.udp_recv_queue.pop_front() {
            let n = len.min(dg.data.len());
            unsafe {
                core::slice::from_raw_parts_mut(buf, n).copy_from_slice(&dg.data[..n]);
            }

            if !src_addr.is_null() && !addrlen_ptr.is_null() {
                if crate::syscall::fs::validate_user_ptr_write(src_addr as *mut u8, core::mem::size_of::<SockAddrIn>()).is_err() ||
                   crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err() {
                    return Errno::EFAULT.into();
                }

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

    // TCP
    drop(sock);
    let inode = match proc_fd::current_task_read_fd(fd) {
        Some(i) => i,
        None => return Errno::EBADF.into(),
    };
    let slice = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    match inode.read(0, slice) {
        Ok(n) => {
            if !src_addr.is_null() && !addrlen_ptr.is_null() {
                if crate::syscall::fs::validate_user_ptr_write(src_addr as *mut u8, core::mem::size_of::<SockAddrIn>()).is_err() ||
                   crate::syscall::fs::validate_user_ptr_write(addrlen_ptr as *mut u8, 4).is_err() {
                    return Errno::EFAULT.into();
                }

                let child_sock = socket.lock();
                let remote_ip = child_sock.remote_addr.unwrap_or(Ipv4Addr::LOCALHOST);
                let remote_port = child_sock.remote_port.unwrap_or(0);

                unsafe {
                    src_addr.write(SockAddrIn {
                        sin_family: 2,
                        sin_port: remote_port.to_be(),
                        sin_addr: remote_ip.octets,
                        sin_zero: [0; 8],
                    });
                    addrlen_ptr.write(16);
                }
            }
            n as SyscallResult
        }
        Err(e) => e as SyscallResult,
    }
}
