//! Network operations for initrd mode using command-line tools and safe abstractions.
//!
//! Uses `ip` and `ping` commands instead of raw libc calls, keeping the entire
//! crate free of unsafe code.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

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

/// Read the first global IPv6 address for an interface using
/// `ip -6 -o addr show <iface> scope global`. Link-local (`fe80::/10`) and
/// loopback addresses are never returned. Returns None if no global address
/// is assigned.
pub fn get_interface_ipv6(interface: &str) -> Option<Ipv6Addr> {
    let output = run_cmd(
        "ip",
        &["-6", "-o", "addr", "show", interface, "scope", "global"],
    )
    .ok()?;
    parse_ipv6_addr_output(&output)
}

/// Extract the first usable global IPv6 address from `ip -6 -o addr show`
/// output. Link-local and loopback addresses are skipped even if present, so
/// the parser is correct regardless of whether the caller filtered by scope.
pub fn parse_ipv6_addr_output(output: &str) -> Option<Ipv6Addr> {
    for line in output.lines() {
        if let Some(inet_pos) = line.find("inet6 ") {
            let after_inet = &line[inet_pos + 6..];
            let addr_str = after_inet.split('/').next()?.trim();
            let addr: Ipv6Addr = match addr_str.parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            // fe80::/10 is link-local; ::1 is loopback. Neither routes off-link.
            if addr.is_loopback() || (addr.segments()[0] & 0xffc0) == 0xfe80 {
                continue;
            }
            return Some(addr);
        }
    }
    None
}

/// True if `interface` has a globally-routable IPv6 unicast address (2000::/3).
///
/// Stricter than [`get_interface_ipv6`], which also accepts unique-local
/// addresses (fc00::/7). A ULA is global *scope* but NOT reachable on the
/// global IPv6 internet — e.g. the dev VM's SLAAC ULA from libvirt's NAT
/// bridge — so it must not gate an internet-routability probe, otherwise the
/// probe pings a public anycast resolver that the ULA-only stack can never
/// reach. Only an address in 2000::/3 (global unicast) means real IPv6 is in
/// the stack.
pub fn interface_has_global_unicast_ipv6(interface: &str) -> bool {
    run_cmd(
        "ip",
        &["-6", "-o", "addr", "show", interface, "scope", "global"],
    )
    .ok()
    .map(|out| parse_has_global_unicast_ipv6(&out))
    .unwrap_or(false)
}

/// Scan `ip -6 -o addr show` output for any global-unicast (2000::/3) address.
pub fn parse_has_global_unicast_ipv6(output: &str) -> bool {
    for line in output.lines() {
        if let Some(inet_pos) = line.find("inet6 ") {
            let after_inet = &line[inet_pos + 6..];
            let addr_str = match after_inet.split('/').next() {
                Some(s) => s.trim(),
                None => continue,
            };
            if let Ok(addr) = addr_str.parse::<Ipv6Addr>() {
                // 2000::/3 is the global-unicast range; everything else
                // (link-local, ULA fc00::/7, loopback, multicast) is excluded.
                if (addr.segments()[0] & 0xe000) == 0x2000 {
                    return true;
                }
            }
        }
    }
    false
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

/// Send an ICMPv6 echo request and wait for a reply using `ping -6`.
pub fn icmp_ping6(addr: Ipv6Addr, timeout: std::time::Duration) -> Result<(), String> {
    let timeout_secs = timeout.as_secs().max(1);
    run_cmd(
        "ping",
        &[
            "-6",
            "-c1",
            &format!("-W{}", timeout_secs),
            &addr.to_string(),
        ],
    )?;
    Ok(())
}

/// Attempt a TCP connection to `addr:port`, succeeding only if the three-way
/// handshake completes within `timeout`.
///
/// This is the connectivity probe that survives the networks ICMP/`:53` probes
/// don't: captive/guest WiFi (e.g. the "berkeley" network) commonly filter ICMP
/// echo and outbound plaintext DNS while still allowing outbound `:443` — the
/// exact path DoH uses. A completed handshake to a public anycast host on `:443`
/// therefore proves routable internet even when every ping is dropped and the
/// advertised IPv6 is a black hole.
pub fn tcp_connect_probe(
    addr: IpAddr,
    port: u16,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let sockaddr = SocketAddr::new(addr, port);
    std::net::TcpStream::connect_timeout(&sockaddr, timeout)
        .map(|_| ())
        .map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ipv6_addr_output_global() {
        let out = "2: eth0    inet6 2001:db8::5/64 scope global dynamic mngtmpaddr \\       valid_lft 86000sec preferred_lft 14000sec";
        assert_eq!(
            parse_ipv6_addr_output(out),
            Some("2001:db8::5".parse().unwrap())
        );
    }

    #[test]
    fn test_parse_ipv6_addr_output_skips_link_local() {
        // Link-local appears first but must be skipped in favor of the global.
        let out = "\
2: eth0    inet6 fe80::1234:5678/64 scope link \\       valid_lft forever preferred_lft forever
2: eth0    inet6 2001:db8::42/64 scope global \\       valid_lft forever preferred_lft forever";
        assert_eq!(
            parse_ipv6_addr_output(out),
            Some("2001:db8::42".parse().unwrap())
        );
    }

    #[test]
    fn test_parse_ipv6_addr_output_unique_local() {
        // fc00::/7 (ULA) is a valid global-scope address and should be accepted.
        let out = "3: wlan0    inet6 fd00:abcd::7/64 scope global";
        assert_eq!(
            parse_ipv6_addr_output(out),
            Some("fd00:abcd::7".parse().unwrap())
        );
    }

    #[test]
    fn test_parse_ipv6_addr_output_none_when_only_link_local() {
        let out = "2: eth0    inet6 fe80::1/64 scope link";
        assert_eq!(parse_ipv6_addr_output(out), None);
    }

    #[test]
    fn test_parse_ipv6_addr_output_empty() {
        assert_eq!(parse_ipv6_addr_output(""), None);
        assert_eq!(
            parse_ipv6_addr_output("2: eth0    inet 192.168.1.5/24"),
            None
        );
    }

    #[test]
    fn test_global_unicast_accepts_2000_slash_3() {
        let out = "2: eth0    inet6 2001:db8::5/64 scope global dynamic";
        assert!(parse_has_global_unicast_ipv6(out));
    }

    #[test]
    fn test_global_unicast_rejects_ula() {
        // fc00::/7 is global *scope* but not internet-routable — this is the
        // dev VM's SLAAC ULA from libvirt's NAT bridge, which must NOT count.
        let out = "3: eth0    inet6 fd00:c0a8:7a::5054:ff:fe99:7710/64 scope global";
        assert!(!parse_has_global_unicast_ipv6(out));
    }

    #[test]
    fn test_global_unicast_rejects_link_local_only() {
        let out = "2: eth0    inet6 fe80::5054:ff:fe99:7710/64 scope link";
        assert!(!parse_has_global_unicast_ipv6(out));
    }

    #[test]
    fn test_global_unicast_rejects_ula_plus_link_local() {
        // The exact dev-VM stack: a ULA and a link-local, no global unicast.
        let out = "\
2: eth0    inet6 fd00:c0a8:7a::5054:ff:fe99:7710/64 scope global
2: eth0    inet6 fe80::5054:ff:fe99:7710/64 scope link";
        assert!(!parse_has_global_unicast_ipv6(out));
    }

    #[test]
    fn test_global_unicast_true_when_real_address_present() {
        // A global unicast alongside a ULA/link-local still counts.
        let out = "\
2: eth0    inet6 fd00:abcd::7/64 scope global
2: eth0    inet6 2001:db8::42/64 scope global dynamic
2: eth0    inet6 fe80::1/64 scope link";
        assert!(parse_has_global_unicast_ipv6(out));
    }

    #[test]
    fn test_global_unicast_empty() {
        assert!(!parse_has_global_unicast_ipv6(""));
        assert!(!parse_has_global_unicast_ipv6(
            "2: eth0    inet 192.168.1.5/24"
        ));
    }
}
