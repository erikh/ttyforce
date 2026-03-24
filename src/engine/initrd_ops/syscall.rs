//! Low-level syscall wrappers for initrd network operations.
//!
//! These functions use raw libc calls for ioctl and ICMP because the nix crate
//! does not wrap SIOCSIFFLAGS, SIOCGIFADDR, or ICMP sockets. Unsafe is scoped
//! to this module only — the rest of the crate denies unsafe_code.
#![allow(unsafe_code)]

use std::net::Ipv4Addr;

use nix::libc;

use crate::engine::real_ops::run_cmd;

/// Set or clear IFF_UP on an interface using SIOCSIFFLAGS ioctl.
pub fn set_interface_up(interface: &str, up: bool) -> Result<(), String> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return Err("failed to create socket".into());
    }

    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    let name_bytes = interface.as_bytes();
    if name_bytes.len() >= libc::IFNAMSIZ {
        unsafe { libc::close(sock) };
        return Err("interface name too long".into());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            name_bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            name_bytes.len(),
        );
    }

    // Get current flags
    if unsafe { libc::ioctl(sock, libc::SIOCGIFFLAGS as _, &mut ifr) } < 0 {
        unsafe { libc::close(sock) };
        return Err(format!(
            "SIOCGIFFLAGS failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let flags = unsafe { ifr.ifr_ifru.ifru_flags };
    let new_flags = if up {
        flags | libc::IFF_UP as i16
    } else {
        flags & !(libc::IFF_UP as i16)
    };
    ifr.ifr_ifru.ifru_flags = new_flags;

    if unsafe { libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &ifr) } < 0 {
        unsafe { libc::close(sock) };
        return Err(format!(
            "SIOCSIFFLAGS failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    unsafe { libc::close(sock) };
    Ok(())
}

/// Read the IPv4 address for an interface using ioctl SIOCGIFADDR.
/// Returns None if no address is assigned.
pub fn get_interface_ipv4(interface: &str) -> Option<Ipv4Addr> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return None;
    }

    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    let name_bytes = interface.as_bytes();
    if name_bytes.len() >= libc::IFNAMSIZ {
        unsafe { libc::close(sock) };
        return None;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            name_bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            name_bytes.len(),
        );
    }

    if unsafe { libc::ioctl(sock, libc::SIOCGIFADDR as _, &mut ifr) } < 0 {
        unsafe { libc::close(sock) };
        return None;
    }

    unsafe { libc::close(sock) };

    let addr =
        unsafe { &*(&ifr.ifr_ifru.ifru_addr as *const _ as *const libc::sockaddr_in) };
    let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));

    if ip.is_unspecified() {
        None
    } else {
        Some(ip)
    }
}

/// Send an ICMP echo request and wait for a reply.
/// Tries SOCK_DGRAM first, then SOCK_RAW, then falls back to the ping command.
pub fn icmp_ping(addr: Ipv4Addr, timeout: std::time::Duration) -> Result<(), String> {
    let sock = unsafe {
        libc::socket(
            libc::AF_INET,
            libc::SOCK_DGRAM,
            libc::IPPROTO_ICMP,
        )
    };
    if sock < 0 {
        // SOCK_DGRAM ICMP not available, try raw
        return icmp_ping_raw(addr, timeout);
    }

    icmp_ping_with_sock(sock, addr, timeout)
}

fn icmp_ping_raw(addr: Ipv4Addr, timeout: std::time::Duration) -> Result<(), String> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_ICMP) };
    if sock < 0 {
        // Neither DGRAM nor RAW ICMP available — fall back to ping command
        return match run_cmd("ping", &["-c1", "-W3", &addr.to_string()]) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        };
    }

    icmp_ping_with_sock(sock, addr, timeout)
}

fn icmp_ping_with_sock(
    sock: i32,
    addr: Ipv4Addr,
    timeout: std::time::Duration,
) -> Result<(), String> {
    // Set receive timeout
    let tv = libc::timeval {
        tv_sec: timeout.as_secs() as _,
        tv_usec: 0,
    };
    unsafe {
        libc::setsockopt(
            sock,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const _ as *const _,
            std::mem::size_of::<libc::timeval>() as u32,
        );
    }

    // Build ICMP echo request (type=8, code=0)
    let mut packet = [0u8; 8];
    packet[0] = 8; // type: echo request
    packet[4] = 0x42; // identifier
    packet[5] = 0x42;
    let checksum = icmp_checksum(&packet);
    packet[2] = (checksum >> 8) as u8;
    packet[3] = (checksum & 0xff) as u8;

    let dest = libc::sockaddr_in {
        sin_family: libc::AF_INET as u16,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from(addr).to_be(),
        },
        sin_zero: [0; 8],
    };

    let sent = unsafe {
        libc::sendto(
            sock,
            packet.as_ptr() as *const _,
            packet.len(),
            0,
            &dest as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as u32,
        )
    };
    if sent < 0 {
        unsafe { libc::close(sock) };
        return Err("sendto failed".into());
    }

    let mut buf = [0u8; 256];
    let received = unsafe { libc::recv(sock, buf.as_mut_ptr() as *mut _, buf.len(), 0) };
    unsafe { libc::close(sock) };

    if received > 0 {
        Ok(())
    } else {
        Err("no reply".into())
    }
}

/// Compute ICMP checksum (RFC 1071).
pub fn icmp_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}
