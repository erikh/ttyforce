//! Network operations for initrd mode using command-line tools and safe abstractions.
//!
//! Uses `ip` and `ping` commands instead of raw libc calls, keeping the entire
//! crate free of unsafe code.

use std::net::Ipv4Addr;

use crate::engine::real_ops::run_cmd;

/// Set or clear IFF_UP on an interface using `ip link set`.
pub fn set_interface_up(interface: &str, up: bool) -> Result<(), String> {
    let state = if up { "up" } else { "down" };
    run_cmd("ip", &["link", "set", interface, state])?;
    Ok(())
}

/// Read the IPv4 address for an interface using `ip -4 -o addr show`.
/// Returns None if no address is assigned.
pub fn get_interface_ipv4(interface: &str) -> Option<Ipv4Addr> {
    let output = run_cmd("ip", &["-4", "-o", "addr", "show", interface]).ok()?;
    // Output format: "2: eth0    inet 192.168.1.100/24 brd 192.168.1.255 scope global eth0"
    for line in output.lines() {
        if let Some(inet_pos) = line.find("inet ") {
            let after_inet = &line[inet_pos + 5..];
            let addr_str = after_inet.split('/').next()?;
            return addr_str.trim().parse().ok();
        }
    }
    None
}

/// Send an ICMP echo request and wait for a reply using the `ping` command.
pub fn icmp_ping(addr: Ipv4Addr, timeout: std::time::Duration) -> Result<(), String> {
    let timeout_secs = timeout.as_secs().max(1);
    run_cmd(
        "ping",
        &["-c1", &format!("-W{}", timeout_secs), &addr.to_string()],
    )?;
    Ok(())
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
