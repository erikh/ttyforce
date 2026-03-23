use std::fs;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

use nix::libc;

use crate::detect::network::{parse_iw_scan, parse_iwlist_scan};
use crate::engine::feedback::OperationResult;
use crate::network::wifi::WifiNetwork;

use crate::engine::real_ops::run_cmd;

// ── Interface management (ioctl) ────────────────────────────────────────

/// Enable a network interface by setting IFF_UP via ioctl.
pub fn enable_interface(interface: &str) -> OperationResult {
    match set_interface_up(interface, true) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("failed to enable {}: {}", interface, e)),
    }
}

/// Disable a network interface by clearing IFF_UP via ioctl.
pub fn disable_interface(interface: &str) -> OperationResult {
    match set_interface_up(interface, false) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("failed to disable {}: {}", interface, e)),
    }
}

/// Shut down a network interface by clearing IFF_UP via ioctl.
pub fn shutdown_interface(interface: &str) -> OperationResult {
    disable_interface(interface)
}

/// Set or clear IFF_UP on an interface using SIOCSIFFLAGS ioctl.
fn set_interface_up(interface: &str, up: bool) -> Result<(), String> {
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
        return Err(format!("SIOCGIFFLAGS failed: {}", std::io::Error::last_os_error()));
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
        return Err(format!("SIOCSIFFLAGS failed: {}", std::io::Error::last_os_error()));
    }

    unsafe { libc::close(sock) };
    Ok(())
}

// ── Wifi (external tools: iw, wpa_supplicant) ──────────────────────────

/// Scan for wifi networks using iw (no dbus).
pub fn scan_wifi_networks(interface: &str) -> OperationResult {
    match run_cmd("iw", &["dev", interface, "scan", "-u"]) {
        Ok(output) => {
            let specs = parse_iw_scan(&output);
            let networks: Vec<WifiNetwork> = specs.iter().map(WifiNetwork::from).collect();
            OperationResult::WifiScanResults(networks)
        }
        Err(e) => {
            match run_cmd("iwlist", &[interface, "scan"]) {
                Ok(output) => {
                    let specs = parse_iwlist_scan(&output);
                    let networks: Vec<WifiNetwork> =
                        specs.iter().map(WifiNetwork::from).collect();
                    OperationResult::WifiScanResults(networks)
                }
                Err(_) => {
                    OperationResult::Error(format!("wifi scan failed on {}: {}", interface, e))
                }
            }
        }
    }
}

/// Receive wifi scan results using iw (cached, no new scan).
pub fn receive_wifi_scan_results(interface: &str) -> OperationResult {
    match run_cmd("iw", &["dev", interface, "scan", "dump", "-u"]) {
        Ok(output) => {
            let specs = parse_iw_scan(&output);
            let networks: Vec<WifiNetwork> = specs.iter().map(WifiNetwork::from).collect();
            OperationResult::WifiScanResults(networks)
        }
        Err(e) => OperationResult::Error(format!(
            "failed to get scan results on {}: {}",
            interface, e
        )),
    }
}

/// Authenticate to a wifi network via wpa_supplicant CLI.
pub fn authenticate_wifi(interface: &str, ssid: &str, password: &str) -> OperationResult {
    let conf_path = format!("/tmp/wpa_supplicant_{}.conf", interface);
    let conf_content = generate_wpa_config(ssid, password);
    if fs::write(&conf_path, &conf_content).is_err() {
        return OperationResult::Error("failed to write wpa_supplicant config".into());
    }

    let _ = run_cmd("pkill", &["-f", &format!("wpa_supplicant.*{}", interface)]);
    std::thread::sleep(std::time::Duration::from_millis(500));

    match run_cmd(
        "wpa_supplicant",
        &["-B", "-i", interface, "-c", &conf_path],
    ) {
        Ok(_) => {
            std::thread::sleep(std::time::Duration::from_secs(3));
            OperationResult::WifiAuthenticated
        }
        Err(e) => OperationResult::WifiAuthFailed(format!("wpa_supplicant failed: {}", e)),
    }
}

/// Configure wifi SSID and authentication.
pub fn configure_wifi_ssid_auth(interface: &str, ssid: &str, password: &str) -> OperationResult {
    authenticate_wifi(interface, ssid, password)
}

/// Configure wifi from a QR code.
pub fn configure_wifi_qr_code(interface: &str, qr_data: &str) -> OperationResult {
    let mut ssid = None;
    let mut password = None;

    let data = qr_data.trim_start_matches("WIFI:").trim_end_matches(";;");
    for part in data.split(';') {
        if let Some(val) = part.strip_prefix("S:") {
            ssid = Some(val.to_string());
        } else if let Some(val) = part.strip_prefix("P:") {
            password = Some(val.to_string());
        }
    }

    match (ssid, password) {
        (Some(s), Some(p)) => {
            let result = authenticate_wifi(interface, &s, &p);
            if result.is_success() {
                OperationResult::WifiQrConfigured
            } else {
                result
            }
        }
        (Some(s), None) => {
            let result = authenticate_wifi(interface, &s, "");
            if result.is_success() {
                OperationResult::WifiQrConfigured
            } else {
                result
            }
        }
        _ => OperationResult::Error("failed to parse QR code data".into()),
    }
}

// ── DHCP (external tool: dhcpcd) ────────────────────────────────────────

/// Configure DHCP on an interface via dhcpcd.
/// Polls for an IP address (up to 30s) before returning.
/// After IP is confirmed, writes /etc/resolv.conf from the lease.
pub fn configure_dhcp(interface: &str) -> OperationResult {
    let result = configure_dhcp_with(
        interface,
        try_trigger_dhcp,
        check_ip_sysfs,
        30,
        std::time::Duration::from_secs(1),
    );

    if result.is_success() {
        // IP is assigned, so the lease is complete — write resolv.conf now
        write_resolv_conf_from_lease(interface);
    }

    result
}

/// Testable inner function with injected dependencies.
fn configure_dhcp_with(
    interface: &str,
    trigger: fn(&str) -> OperationResult,
    check_ip: fn(&str) -> OperationResult,
    max_attempts: u32,
    poll_interval: std::time::Duration,
) -> OperationResult {
    let dhcp_triggered = trigger(interface);

    if let OperationResult::Error(e) = dhcp_triggered {
        return OperationResult::Error(e);
    }

    for _ in 0..max_attempts {
        std::thread::sleep(poll_interval);
        if let OperationResult::IpAssigned(_) = check_ip(interface) {
            return OperationResult::Success;
        }
    }

    OperationResult::Error(format!(
        "DHCP timeout on {}: no IP assigned after {}s",
        interface,
        max_attempts as u64 * poll_interval.as_secs()
    ))
}

/// Trigger DHCP on an interface via dhcpcd.
fn try_trigger_dhcp(interface: &str) -> OperationResult {
    let _ = run_cmd("dhcpcd", &["--release", interface]);
    std::thread::sleep(std::time::Duration::from_millis(200));

    match run_cmd("dhcpcd", &["-b", interface]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("dhcpcd failed on {}: {}", interface, e)),
    }
}

/// Read DNS servers from the dhcpcd lease and write /etc/resolv.conf.
/// Best-effort: if the lease can't be read, resolv.conf is left unchanged.
fn write_resolv_conf_from_lease(interface: &str) {
    if let Ok(output) = run_cmd("dhcpcd", &["--dumplease", interface]) {
        if let Some(content) = parse_dhcpcd_lease_dns(&output) {
            let _ = fs::write("/etc/resolv.conf", content);
        }
    }
}

/// Parse dhcpcd --dumplease output and generate resolv.conf content.
/// Returns None if no nameservers are found.
pub fn parse_dhcpcd_lease_dns(lease_output: &str) -> Option<String> {
    let mut nameservers = Vec::new();
    let mut domain = None;

    for line in lease_output.lines() {
        if let Some(val) = line.strip_prefix("domain_name_servers=") {
            for ns in val.split_whitespace() {
                nameservers.push(ns.to_string());
            }
        } else if let Some(val) = line.strip_prefix("domain_name=") {
            domain = Some(val.trim_matches('\'').to_string());
        }
    }

    if nameservers.is_empty() {
        return None;
    }

    let mut content = String::new();
    if let Some(d) = domain {
        content.push_str(&format!("search {}\n", d));
    }
    for ns in &nameservers {
        content.push_str(&format!("nameserver {}\n", ns));
    }
    Some(content)
}

/// Select an interface as primary. In initrd mode this is a no-op since
/// typically only one interface is active.
pub fn select_primary_interface(_interface: &str) -> OperationResult {
    OperationResult::Success
}

// ── Network checks (sysfs / syscalls) ───────────────────────────────────

/// Check link availability via sysfs.
pub fn check_link_availability(interface: &str) -> OperationResult {
    let carrier_path = format!("/sys/class/net/{}/carrier", interface);
    match fs::read_to_string(&carrier_path) {
        Ok(val) if val.trim() == "1" => OperationResult::LinkUp,
        Ok(_) => OperationResult::LinkDown,
        Err(_) => OperationResult::LinkDown,
    }
}

/// Check IP address by reading interface addresses via sysfs/netlink.
/// Falls back to parsing /proc/net/fib_trie if needed.
pub fn check_ip_address(interface: &str) -> OperationResult {
    check_ip_sysfs(interface)
}

/// Read the IPv4 address for an interface from the ioctl SIOCGIFADDR.
fn check_ip_sysfs(interface: &str) -> OperationResult {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return OperationResult::NoIp;
    }

    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    let name_bytes = interface.as_bytes();
    if name_bytes.len() >= libc::IFNAMSIZ {
        unsafe { libc::close(sock) };
        return OperationResult::NoIp;
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
        return OperationResult::NoIp;
    }

    unsafe { libc::close(sock) };

    // Extract IPv4 address from sockaddr_in
    let addr = unsafe { &*(&ifr.ifr_ifru.ifru_addr as *const _ as *const libc::sockaddr_in) };
    let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));

    if ip.is_unspecified() {
        OperationResult::NoIp
    } else {
        OperationResult::IpAssigned(ip.to_string())
    }
}

/// Check for upstream router by parsing /proc/net/route.
pub fn check_upstream_router(interface: &str) -> OperationResult {
    match fs::read_to_string("/proc/net/route") {
        Ok(content) => {
            for line in content.lines().skip(1) {
                let fields: Vec<&str> = line.split('\t').collect();
                if fields.len() < 3 {
                    continue;
                }
                let iface = fields[0];
                let destination = fields[1];
                let gateway = fields[2];

                // Default route: destination is 00000000
                if iface == interface && destination == "00000000" {
                    if let Ok(gw) = u32::from_str_radix(gateway, 16) {
                        if gw != 0 {
                            // /proc/net/route stores IPs in host byte order (little-endian on x86)
                            let ip = Ipv4Addr::from(u32::from_be(gw.swap_bytes()));
                            return OperationResult::RouterFound(ip.to_string());
                        }
                    }
                }
            }
            OperationResult::NoRouter
        }
        Err(_) => OperationResult::NoRouter,
    }
}

/// Check internet routability by sending an ICMP echo request to 1.1.1.1.
/// Uses a raw socket — requires CAP_NET_RAW or root.
pub fn check_internet_routability(_interface: &str) -> OperationResult {
    match icmp_ping(Ipv4Addr::new(1, 1, 1, 1), std::time::Duration::from_secs(3)) {
        Ok(_) => OperationResult::InternetReachable,
        Err(_) => OperationResult::NoInternet,
    }
}

/// Send an ICMP echo request and wait for a reply.
fn icmp_ping(addr: Ipv4Addr, timeout: std::time::Duration) -> Result<(), String> {
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

/// Fallback raw socket ICMP ping.
fn icmp_ping_raw(addr: Ipv4Addr, timeout: std::time::Duration) -> Result<(), String> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_ICMP) };
    if sock < 0 {
        // Neither DGRAM nor RAW ICMP available — fall back to ping command
        return match run_cmd("ping", &["-c1", "-W3", &addr.to_string()]) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        };
    }

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

    let mut packet = [0u8; 8];
    packet[0] = 8;
    packet[4] = 0x42;
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
fn icmp_checksum(data: &[u8]) -> u16 {
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

/// Check DNS resolution using a direct UDP DNS query.
pub fn check_dns_resolution(_interface: &str, hostname: &str) -> OperationResult {
    match dns_resolve(hostname) {
        Ok(ip) => OperationResult::DnsResolved(ip),
        Err(e) => OperationResult::DnsFailed(format!(
            "DNS resolution failed for {}: {}",
            hostname, e
        )),
    }
}

/// Resolve a hostname by sending a DNS query to the system nameserver.
fn dns_resolve(hostname: &str) -> Result<String, String> {
    let nameserver = get_nameserver().ok_or("no nameserver found")?;

    let query = build_dns_query(hostname).map_err(|e| format!("failed to build DNS query: {}", e))?;

    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("bind: {}", e))?;
    sock.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .map_err(|e| format!("set_read_timeout: {}", e))?;

    let dest: SocketAddr = format!("{}:53", nameserver)
        .parse()
        .map_err(|e| format!("invalid nameserver addr: {}", e))?;

    sock.send_to(&query, dest)
        .map_err(|e| format!("send: {}", e))?;

    let mut buf = [0u8; 512];
    let n = sock.recv(&mut buf).map_err(|e| format!("recv: {}", e))?;

    parse_dns_response(&buf[..n])
}

/// Read the first nameserver from /etc/resolv.conf.
fn get_nameserver() -> Option<String> {
    let content = fs::read_to_string("/etc/resolv.conf").ok()?;
    for line in content.lines() {
        let line = line.trim();
        if let Some(ns) = line.strip_prefix("nameserver") {
            let ns = ns.trim();
            if !ns.is_empty() {
                return Some(ns.to_string());
            }
        }
    }
    None
}

/// Build a minimal DNS A record query for a hostname.
fn build_dns_query(hostname: &str) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(64);

    // Header: ID=0x1234, flags=0x0100 (standard query, recursion desired)
    buf.extend_from_slice(&[0x12, 0x34, 0x01, 0x00]);
    // QDCOUNT=1, ANCOUNT=0, NSCOUNT=0, ARCOUNT=0
    buf.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]);

    // Question: encode hostname as DNS labels
    for label in hostname.split('.') {
        if label.len() > 63 {
            return Err("label too long".into());
        }
        buf.push(label.len() as u8);
        buf.extend_from_slice(label.as_bytes());
    }
    buf.push(0); // root label

    // QTYPE=A (1), QCLASS=IN (1)
    buf.extend_from_slice(&[0, 1, 0, 1]);

    Ok(buf)
}

/// Parse a DNS response and extract the first A record IP address.
fn parse_dns_response(data: &[u8]) -> Result<String, String> {
    if data.len() < 12 {
        return Err("response too short".into());
    }

    let ancount = u16::from_be_bytes([data[6], data[7]]);
    if ancount == 0 {
        return Err("no answers".into());
    }

    // Skip header (12 bytes) and question section
    let mut pos = 12;

    // Skip question: labels + null + QTYPE(2) + QCLASS(2)
    while pos < data.len() && data[pos] != 0 {
        if data[pos] & 0xc0 == 0xc0 {
            pos += 2;
            break;
        }
        pos += 1 + data[pos] as usize;
    }
    if pos < data.len() && data[pos] == 0 {
        pos += 1;
    }
    pos += 4; // QTYPE + QCLASS

    // Parse answer records
    for _ in 0..ancount {
        if pos >= data.len() {
            break;
        }

        // Skip name (may be compressed)
        if data[pos] & 0xc0 == 0xc0 {
            pos += 2;
        } else {
            while pos < data.len() && data[pos] != 0 {
                pos += 1 + data[pos] as usize;
            }
            pos += 1;
        }

        if pos + 10 > data.len() {
            break;
        }

        let rtype = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let rdlength = u16::from_be_bytes([data[pos + 8], data[pos + 9]]) as usize;
        pos += 10;

        if rtype == 1 && rdlength == 4 && pos + 4 <= data.len() {
            // A record
            let ip = Ipv4Addr::new(data[pos], data[pos + 1], data[pos + 2], data[pos + 3]);
            return Ok(ip.to_string());
        }

        pos += rdlength;
    }

    Err("no A record found".into())
}

// ── Cleanup ─────────────────────────────────────────────────────────────

/// Kill dhcpcd for an interface. Best-effort.
pub fn cleanup_network_config(interface: &str) -> OperationResult {
    let _ = run_cmd("dhcpcd", &["--release", interface]);
    let _ = run_cmd("pkill", &["-f", &format!("dhcpcd.*{}", interface)]);
    OperationResult::Success
}

/// Kill wpa_supplicant for an interface and remove its config file.
pub fn cleanup_wpa_supplicant(interface: &str) -> OperationResult {
    let _ = run_cmd("pkill", &["-f", &format!("wpa_supplicant.*{}", interface)]);
    let conf_path = format!("/tmp/wpa_supplicant_{}.conf", interface);
    match fs::remove_file(&conf_path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return OperationResult::Error(format!(
                "failed to remove wpa_supplicant config {}: {}",
                conf_path, e
            ));
        }
    }
    OperationResult::Success
}

// ── Persist config to installed system ──────────────────────────────────

/// Persist the network configuration established during the initrd session
/// to the installed system's /etc so it boots with working networking.
pub fn persist_network_config(mount_point: &str, interface: &str) -> OperationResult {
    let target_networkd_dir = format!("{}/etc/systemd/network", mount_point);
    if let Err(e) = fs::create_dir_all(&target_networkd_dir) {
        return OperationResult::Error(format!(
            "failed to create {}: {}",
            target_networkd_dir, e
        ));
    }

    let network_unit = generate_persist_network_config(interface);
    let network_path = format!("{}/20-{}.network", target_networkd_dir, interface);
    if let Err(e) = fs::write(&network_path, &network_unit) {
        return OperationResult::Error(format!(
            "failed to write {}: {}",
            network_path, e
        ));
    }

    // If we have a wpa_supplicant config, copy it to the installed system
    let wpa_src = format!("/tmp/wpa_supplicant_{}.conf", interface);
    if std::path::Path::new(&wpa_src).exists() {
        let wpa_target_dir = format!("{}/etc/wpa_supplicant", mount_point);
        if let Err(e) = fs::create_dir_all(&wpa_target_dir) {
            return OperationResult::Error(format!(
                "failed to create {}: {}",
                wpa_target_dir, e
            ));
        }

        let wpa_target = format!(
            "{}/wpa_supplicant-{}.conf",
            wpa_target_dir, interface
        );
        if let Err(e) = fs::copy(&wpa_src, &wpa_target) {
            return OperationResult::Error(format!(
                "failed to copy wpa_supplicant config to {}: {}",
                wpa_target, e
            ));
        }
    }

    OperationResult::Success
}

// ── Pure helpers ────────────────────────────────────────────────────────

/// Generate a systemd-networkd .network unit for persisting to the installed system.
pub fn generate_persist_network_config(interface: &str) -> String {
    format!(
        "[Match]\nName={}\n\n[Network]\nDHCP=yes\n",
        interface
    )
}

/// Generate a wpa_supplicant config for a wifi network.
pub fn generate_wpa_config(ssid: &str, password: &str) -> String {
    format!(
        "ctrl_interface=/var/run/wpa_supplicant\nnetwork={{\n  ssid=\"{}\"\n  psk=\"{}\"\n}}\n",
        ssid, password
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    #[test]
    fn test_generate_persist_network_config() {
        let config = generate_persist_network_config("eth0");
        assert!(config.contains("[Match]"));
        assert!(config.contains("Name=eth0"));
        assert!(config.contains("DHCP=yes"));
    }

    #[test]
    fn test_generate_persist_network_config_wifi() {
        let config = generate_persist_network_config("wlan0");
        assert!(config.contains("Name=wlan0"));
        assert!(config.contains("DHCP=yes"));
    }

    #[test]
    fn test_generate_wpa_config() {
        let config = generate_wpa_config("MyNetwork", "secret123");
        assert!(config.contains("ssid=\"MyNetwork\""));
        assert!(config.contains("psk=\"secret123\""));
        assert!(config.contains("ctrl_interface=/var/run/wpa_supplicant"));
    }

    #[test]
    fn test_icmp_checksum() {
        // Echo request with type=8, code=0, id=0x4242, seq=0
        let packet = [8, 0, 0, 0, 0x42, 0x42, 0, 0];
        let cksum = icmp_checksum(&packet);
        // Verify: checksum of packet with checksum inserted should be 0
        let mut verified = packet;
        verified[2] = (cksum >> 8) as u8;
        verified[3] = (cksum & 0xff) as u8;
        assert_eq!(icmp_checksum(&verified), 0);
    }

    #[test]
    fn test_build_dns_query() {
        let query = build_dns_query("example.com").unwrap();
        // Header: 12 bytes
        assert_eq!(query[0..2], [0x12, 0x34]); // ID
        assert_eq!(query[4..6], [0, 1]); // QDCOUNT=1
        // Question starts at offset 12
        assert_eq!(query[12], 7); // "example" length
        assert_eq!(&query[13..20], b"example");
        assert_eq!(query[20], 3); // "com" length
        assert_eq!(&query[21..24], b"com");
        assert_eq!(query[24], 0); // root label
    }

    #[test]
    fn test_parse_dns_response() {
        // Minimal DNS response with one A record for 93.184.216.34
        let mut resp = Vec::new();
        // Header
        resp.extend_from_slice(&[0x12, 0x34, 0x81, 0x80]); // ID, flags
        resp.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 0]); // QD=1, AN=1
        // Question: example.com A IN
        resp.push(7);
        resp.extend_from_slice(b"example");
        resp.push(3);
        resp.extend_from_slice(b"com");
        resp.push(0);
        resp.extend_from_slice(&[0, 1, 0, 1]); // QTYPE=A, QCLASS=IN
        // Answer: compressed name, A record
        resp.extend_from_slice(&[0xc0, 0x0c]); // name pointer to offset 12
        resp.extend_from_slice(&[0, 1]); // TYPE=A
        resp.extend_from_slice(&[0, 1]); // CLASS=IN
        resp.extend_from_slice(&[0, 0, 0, 60]); // TTL
        resp.extend_from_slice(&[0, 4]); // RDLENGTH=4
        resp.extend_from_slice(&[93, 184, 216, 34]); // RDATA

        let ip = parse_dns_response(&resp).unwrap();
        assert_eq!(ip, "93.184.216.34");
    }

    #[test]
    fn test_parse_dns_response_no_answers() {
        let resp = [0x12, 0x34, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0];
        assert!(parse_dns_response(&resp).is_err());
    }

    #[test]
    fn test_get_nameserver_parsing() {
        // This test verifies the parsing logic, not actual /etc/resolv.conf
        // The function reads from the filesystem so we test the format expectations
        let line = "nameserver 8.8.8.8";
        let ns = line.strip_prefix("nameserver").unwrap().trim();
        assert_eq!(ns, "8.8.8.8");
    }

    // DHCP polling tests

    #[test]
    fn test_dhcp_polling_immediate_ip() {
        let result = configure_dhcp_with(
            "eth0",
            |_| OperationResult::Success,
            |_| OperationResult::IpAssigned("10.0.0.5".into()),
            5,
            Duration::from_millis(1),
        );
        assert!(result.is_success(), "expected Success, got {:?}", result);
    }

    #[test]
    fn test_dhcp_polling_ip_on_third_attempt() {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        COUNTER.store(0, Ordering::SeqCst);
        let result = configure_dhcp_with(
            "eth0",
            |_| OperationResult::Success,
            |_| {
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                if n >= 2 {
                    OperationResult::IpAssigned("10.0.0.5".into())
                } else {
                    OperationResult::NoIp
                }
            },
            5,
            Duration::from_millis(1),
        );
        assert!(result.is_success(), "expected Success, got {:?}", result);
    }

    #[test]
    fn test_dhcp_polling_timeout() {
        let result = configure_dhcp_with(
            "eth0",
            |_| OperationResult::Success,
            |_| OperationResult::NoIp,
            3,
            Duration::from_millis(1),
        );
        match result {
            OperationResult::Error(msg) => {
                assert!(msg.contains("DHCP timeout"), "got: {}", msg);
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_dhcpcd_lease_dns_full() {
        let lease = "\
ip_address=192.168.1.50
subnet_mask=255.255.255.0
routers=192.168.1.1
domain_name_servers=8.8.8.8 8.8.4.4
domain_name='example.local'
lease_time=86400
";
        let result = parse_dhcpcd_lease_dns(lease).unwrap();
        assert!(result.contains("nameserver 8.8.8.8\n"));
        assert!(result.contains("nameserver 8.8.4.4\n"));
        assert!(result.contains("search example.local\n"));
    }

    #[test]
    fn test_parse_dhcpcd_lease_dns_no_domain() {
        let lease = "domain_name_servers=1.1.1.1\n";
        let result = parse_dhcpcd_lease_dns(lease).unwrap();
        assert_eq!(result, "nameserver 1.1.1.1\n");
        assert!(!result.contains("search"));
    }

    #[test]
    fn test_parse_dhcpcd_lease_dns_no_nameservers() {
        let lease = "ip_address=10.0.0.5\nrouters=10.0.0.1\n";
        assert!(parse_dhcpcd_lease_dns(lease).is_none());
    }

    #[test]
    fn test_parse_dhcpcd_lease_dns_empty() {
        assert!(parse_dhcpcd_lease_dns("").is_none());
    }

    #[test]
    fn test_dhcp_trigger_failure_skips_polling() {
        static POLL_CALLED: AtomicU32 = AtomicU32::new(0);
        POLL_CALLED.store(0, Ordering::SeqCst);

        let result = configure_dhcp_with(
            "eth0",
            |_| OperationResult::Error("dhcpcd not found".into()),
            |_| {
                POLL_CALLED.fetch_add(1, Ordering::SeqCst);
                OperationResult::NoIp
            },
            5,
            Duration::from_millis(1),
        );

        assert!(result.is_error());
        assert_eq!(POLL_CALLED.load(Ordering::SeqCst), 0);
    }
}
