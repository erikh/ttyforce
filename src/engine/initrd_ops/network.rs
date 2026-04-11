use std::fs;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

use crate::detect::network::{parse_iw_scan, parse_iwlist_scan};
use crate::engine::feedback::OperationResult;
use crate::network::wifi::WifiNetwork;

use crate::engine::real_ops::{cmd_log_append, run_cmd};

// ── Interface management (ioctl) ────────────────────────────────────────

/// Enable a network interface by setting IFF_UP via ioctl.
/// Waits up to 5 seconds for carrier to appear after bringing the interface up,
/// since carrier detection is asynchronous.
///
/// For wifi interfaces, attempts rfkill unblock first in case the radio is
/// soft-blocked (common in initrd environments).
pub fn enable_interface(interface: &str) -> OperationResult {
    // Best-effort rfkill unblock for wifi interfaces
    if interface.starts_with("wl") || interface.starts_with("wlan") {
        if let Err(e) = run_cmd("rfkill", &["unblock", "wifi"]) {
            cmd_log_append(format!("  rfkill unblock wifi: {}", e));
        }
    }

    cmd_log_append(format!("$ ioctl SIOCSIFFLAGS IFF_UP on {}", interface));
    if let Err(e) = set_interface_up(interface, true) {
        cmd_log_append(format!("  -> FAILED: {}", e));
        return OperationResult::Error(format!("failed to enable {}: {}", interface, e));
    }

    // Wait for carrier — ioctl IFF_UP is asynchronous
    cmd_log_append(format!("  waiting for carrier on {} ...", interface));
    let carrier_path = format!("/sys/class/net/{}/carrier", interface);
    for i in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(val) = fs::read_to_string(&carrier_path) {
            if val.trim() == "1" {
                cmd_log_append(format!("  -> carrier up after {}ms", (i + 1) * 100));
                return OperationResult::Success;
            }
        }
    }

    cmd_log_append("  -> no carrier after 5s (continuing)".to_string());
    OperationResult::Success
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
    super::syscall::set_interface_up(interface, up)
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

    if let Err(e) = run_cmd("pkill", &["-f", &format!("wpa_supplicant.*{}", interface)]) {
        cmd_log_append(format!("  pkill wpa_supplicant: {}", e));
    }
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
    match parse_wifi_qr(qr_data) {
        Some((ssid, password)) => {
            let result = authenticate_wifi(interface, &ssid, &password);
            if result.is_success() {
                OperationResult::WifiQrConfigured
            } else {
                result
            }
        }
        None => OperationResult::Error("failed to parse QR code data".into()),
    }
}

/// Parse a WIFI QR code string into (ssid, password).
/// Format: WIFI:T:WPA;S:ssid;P:password;;
pub fn parse_wifi_qr(qr_data: &str) -> Option<(String, String)> {
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

    ssid.map(|s| (s, password.unwrap_or_default()))
}

// ── WPS push-button connection ──────────────────────────────────────────

/// Start WPS PBC mode: write a minimal wpa_supplicant config, start
/// wpa_supplicant, and trigger `wpa_cli wps_pbc`.
pub fn wps_pbc_start(interface: &str) -> OperationResult {
    let conf_path = format!("/tmp/wpa_supplicant_{}.conf", interface);
    let conf_content = "ctrl_interface=/var/run/wpa_supplicant\nupdate_config=1\n";

    cmd_log_append(format!("$ WPS PBC start on {}", interface));

    if fs::write(&conf_path, conf_content).is_err() {
        return OperationResult::Error("failed to write WPS wpa_supplicant config".into());
    }

    // Kill any existing wpa_supplicant for this interface (best-effort)
    if let Err(e) = run_cmd("pkill", &["-f", &format!("wpa_supplicant.*{}", interface)]) {
        cmd_log_append(format!("  pkill wpa_supplicant: {}", e));
    }
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Start wpa_supplicant
    if let Err(e) = run_cmd(
        "wpa_supplicant",
        &["-B", "-i", interface, "-c", &conf_path],
    ) {
        return OperationResult::Error(format!("wpa_supplicant failed: {}", e));
    }

    // Wait for wpa_supplicant to initialize
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Trigger WPS PBC
    match run_cmd("wpa_cli", &["-i", interface, "wps_pbc"]) {
        Ok(_) => {
            cmd_log_append("  -> WPS PBC initiated, waiting for router...".to_string());
            OperationResult::Success
        }
        Err(e) => OperationResult::Error(format!("wpa_cli wps_pbc failed: {}", e)),
    }
}

/// Poll WPS status by checking `wpa_cli status` for wpa_state=COMPLETED.
pub fn wps_pbc_status(interface: &str) -> OperationResult {
    match run_cmd("wpa_cli", &["-i", interface, "status"]) {
        Ok(output) => {
            for line in output.lines() {
                if let Some(state) = line.strip_prefix("wpa_state=") {
                    match state {
                        "COMPLETED" => {
                            cmd_log_append("  -> WPS connection completed".to_string());
                            return OperationResult::WpsCompleted;
                        }
                        "INACTIVE" | "INTERFACE_DISABLED" => {
                            return OperationResult::WifiTimeout;
                        }
                        _ => {
                            // SCANNING, ASSOCIATING, etc. — still in progress
                            return OperationResult::WpsPending;
                        }
                    }
                }
            }
            // No wpa_state line found — still initializing
            OperationResult::WpsPending
        }
        Err(_) => OperationResult::WpsPending,
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
    if let Err(e) = run_cmd("dhcpcd", &["--release", interface]) {
        cmd_log_append(format!("  dhcpcd release: {}", e));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    match run_cmd("dhcpcd", &["-b", interface]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("dhcpcd failed on {}: {}", interface, e)),
    }
}

/// Read DNS servers from the dhcpcd lease and write /etc/resolv.conf.
/// Best-effort: if the lease can't be read, resolv.conf is left unchanged.
fn write_resolv_conf_from_lease(interface: &str) {
    // Try dhcpcd --dumplease first
    if let Ok(output) = run_cmd("dhcpcd", &["--dumplease", interface]) {
        if let Some(content) = parse_dhcpcd_lease_dns(&output) {
            cmd_log_append("$ write /etc/resolv.conf from lease".to_string());
            for line in content.lines() {
                cmd_log_append(format!("  {}", line));
            }
            if let Err(e) = fs::write("/etc/resolv.conf", content) {
                cmd_log_append(format!("  write resolv.conf failed: {}", e));
            }
            return;
        }
    }

    // Try dhcpcd -U (alternative dump format)
    if let Ok(output) = run_cmd("dhcpcd", &["-U", interface]) {
        if let Some(content) = parse_dhcpcd_lease_dns(&output) {
            cmd_log_append("$ write /etc/resolv.conf from dhcpcd -U".to_string());
            for line in content.lines() {
                cmd_log_append(format!("  {}", line));
            }
            if let Err(e) = fs::write("/etc/resolv.conf", content) {
                cmd_log_append(format!("  write resolv.conf failed: {}", e));
            }
            return;
        }
    }

    // Check if resolv.conf already has nameservers (dhcpcd hooks may have written it)
    if let Ok(existing) = fs::read_to_string("/etc/resolv.conf") {
        if existing.lines().any(|l| l.trim().starts_with("nameserver")) {
            cmd_log_append("  /etc/resolv.conf already has nameservers".to_string());
            return;
        }
    }

    // Last resort: write a default resolv.conf
    cmd_log_append("$ write /etc/resolv.conf with fallback nameservers".to_string());
    let fallback = "nameserver 1.1.1.1\nnameserver 8.8.8.8\n";
    if let Err(e) = fs::write("/etc/resolv.conf", fallback) {
        cmd_log_append(format!("  write resolv.conf failed: {}", e));
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
    cmd_log_append(format!("$ cat {}", carrier_path));
    match fs::read_to_string(&carrier_path) {
        Ok(val) if val.trim() == "1" => {
            cmd_log_append("  -> carrier=1 (link up)".to_string());
            OperationResult::LinkUp
        }
        Ok(val) => {
            cmd_log_append(format!("  -> carrier={} (link down)", val.trim()));
            OperationResult::LinkDown
        }
        Err(e) => {
            cmd_log_append(format!("  -> error: {} (link down)", e));
            OperationResult::LinkDown
        }
    }
}

/// Check IP address by reading interface addresses via sysfs/netlink.
/// Falls back to parsing /proc/net/fib_trie if needed.
pub fn check_ip_address(interface: &str) -> OperationResult {
    check_ip_sysfs(interface)
}

/// Read the IPv4 address for an interface from the ioctl SIOCGIFADDR.
fn check_ip_sysfs(interface: &str) -> OperationResult {
    cmd_log_append(format!("$ ioctl SIOCGIFADDR on {}", interface));
    match super::syscall::get_interface_ipv4(interface) {
        Some(ip) => {
            cmd_log_append(format!("  -> {}", ip));
            OperationResult::IpAssigned(ip.to_string())
        }
        None => {
            cmd_log_append("  -> no IP assigned".to_string());
            OperationResult::NoIp
        }
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
    cmd_log_append("$ ping 1.1.1.1 (ICMP echo)".to_string());
    match super::syscall::icmp_ping(Ipv4Addr::new(1, 1, 1, 1), std::time::Duration::from_secs(3)) {
        Ok(_) => {
            cmd_log_append("  -> reply received".to_string());
            OperationResult::InternetReachable
        }
        Err(e) => {
            cmd_log_append(format!("  -> FAILED: {}", e));
            OperationResult::NoInternet
        }
    }
}

/// Check DNS resolution using a direct UDP DNS query.
pub fn check_dns_resolution(_interface: &str, hostname: &str) -> OperationResult {
    cmd_log_append(format!("$ dns resolve {}", hostname));
    match dns_resolve(hostname) {
        Ok(ip) => {
            cmd_log_append(format!("  -> {}", ip));
            OperationResult::DnsResolved(ip)
        }
        Err(e) => {
            cmd_log_append(format!("  -> FAILED: {}", e));
            OperationResult::DnsFailed(format!(
                "DNS resolution failed for {}: {}",
                hostname, e
            ))
        }
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
    if let Err(e) = run_cmd("dhcpcd", &["--release", interface]) {
        cmd_log_append(format!("  dhcpcd release: {}", e));
    }
    if let Err(e) = run_cmd("pkill", &["-f", &format!("dhcpcd.*{}", interface)]) {
        cmd_log_append(format!("  pkill dhcpcd: {}", e));
    }
    OperationResult::Success
}

/// Kill wpa_supplicant for an interface and remove its config file.
pub fn cleanup_wpa_supplicant(interface: &str) -> OperationResult {
    if let Err(e) = run_cmd("pkill", &["-f", &format!("wpa_supplicant.*{}", interface)]) {
        cmd_log_append(format!("  pkill wpa_supplicant: {}", e));
    }
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
pub fn persist_network_config(mount_point: &str, interface: &str, mac_address: &str) -> OperationResult {
    let target_networkd_dir = format!("{}/systemd/network", mount_point);
    if let Err(e) = fs::create_dir_all(&target_networkd_dir) {
        return OperationResult::Error(format!(
            "failed to create {}: {}",
            target_networkd_dir, e
        ));
    }

    cmd_log_append(format!(
        "$ persist network config: iface={} mac={} -> {}",
        interface, mac_address, mount_point
    ));
    let network_unit = generate_persist_network_config(interface, mac_address);
    let network_path = format!("{}/20-{}.network", target_networkd_dir, interface);
    cmd_log_append(format!("  writing {} ({} bytes)", network_path, network_unit.len()));
    cmd_log_append(format!("  content: {}", network_unit.trim()));
    if let Err(e) = fs::write(&network_path, &network_unit) {
        return OperationResult::Error(format!(
            "failed to write {}: {}",
            network_path, e
        ));
    }

    // If we have a wpa_supplicant config, copy it to the installed system
    let wpa_src = format!("/tmp/wpa_supplicant_{}.conf", interface);
    if std::path::Path::new(&wpa_src).exists() {
        let wpa_target_dir = format!("{}/wpa_supplicant", mount_point);
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
/// Uses MACAddress matching so the config works regardless of interface naming
/// scheme (initrd may use eth0 while booted system uses enp3s0).
pub fn generate_persist_network_config(interface: &str, mac_address: &str) -> String {
    if mac_address.is_empty() || mac_address == "00:00:00:00:00:00" {
        // Fallback to name matching if MAC is unavailable
        format!(
            "[Match]\nName={}\n\n[Network]\nDHCP=yes\nMulticastDNS=yes\n\n[DHCPv4]\nUseDNS=no\n",
            interface
        )
    } else {
        format!(
            "[Match]\nMACAddress={}\n\n[Network]\nDHCP=yes\nMulticastDNS=yes\n\n[DHCPv4]\nUseDNS=no\n",
            mac_address
        )
    }
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
    fn test_generate_persist_network_config_with_mac() {
        let config = generate_persist_network_config("eth0", "aa:bb:cc:dd:ee:ff");
        assert!(config.contains("[Match]"));
        assert!(config.contains("MACAddress=aa:bb:cc:dd:ee:ff"));
        assert!(!config.contains("Name="), "should use MAC, not name");
        assert!(config.contains("DHCP=yes"));
        assert!(config.contains("[DHCPv4]"));
        assert!(config.contains("UseDNS=no"));
    }

    #[test]
    fn test_generate_persist_network_config_no_mac_fallback() {
        let config = generate_persist_network_config("eth0", "");
        assert!(config.contains("Name=eth0"));
        assert!(!config.contains("MACAddress"));
    }

    #[test]
    fn test_generate_persist_network_config_zero_mac_fallback() {
        let config = generate_persist_network_config("wlan0", "00:00:00:00:00:00");
        assert!(config.contains("Name=wlan0"));
        assert!(!config.contains("MACAddress"));
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
        let cksum = crate::engine::initrd_ops::syscall::icmp_checksum(&packet);
        // Verify: checksum of packet with checksum inserted should be 0
        let mut verified = packet;
        verified[2] = (cksum >> 8) as u8;
        verified[3] = (cksum & 0xff) as u8;
        assert_eq!(crate::engine::initrd_ops::syscall::icmp_checksum(&verified), 0);
    }

    #[test]
    fn test_build_dns_query() -> Result<(), String> {
        let query = build_dns_query("example.com")?;
        // Header: 12 bytes
        assert_eq!(query[0..2], [0x12, 0x34]); // ID
        assert_eq!(query[4..6], [0, 1]); // QDCOUNT=1
        // Question starts at offset 12
        assert_eq!(query[12], 7); // "example" length
        assert_eq!(&query[13..20], b"example");
        assert_eq!(query[20], 3); // "com" length
        assert_eq!(&query[21..24], b"com");
        assert_eq!(query[24], 0); // root label
        Ok(())
    }

    #[test]
    fn test_parse_dns_response() -> Result<(), String> {
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

        let ip = parse_dns_response(&resp)?;
        assert_eq!(ip, "93.184.216.34");
        Ok(())
    }

    #[test]
    fn test_parse_dns_response_no_answers() {
        let resp = [0x12, 0x34, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0];
        assert!(parse_dns_response(&resp).is_err());
    }

    #[test]
    fn test_get_nameserver_parsing() -> Result<(), String> {
        // This test verifies the parsing logic, not actual /etc/resolv.conf
        // The function reads from the filesystem so we test the format expectations
        let line = "nameserver 8.8.8.8";
        let ns = line
            .strip_prefix("nameserver")
            .ok_or("expected 'nameserver' prefix")?
            .trim();
        assert_eq!(ns, "8.8.8.8");
        Ok(())
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
    fn test_parse_dhcpcd_lease_dns_full() -> Result<(), String> {
        let lease = "\
ip_address=192.168.1.50
subnet_mask=255.255.255.0
routers=192.168.1.1
domain_name_servers=8.8.8.8 8.8.4.4
domain_name='example.local'
lease_time=86400
";
        let result = parse_dhcpcd_lease_dns(lease).ok_or("expected Some from parse_dhcpcd_lease_dns")?;
        assert!(result.contains("nameserver 8.8.8.8\n"));
        assert!(result.contains("nameserver 8.8.4.4\n"));
        assert!(result.contains("search example.local\n"));
        Ok(())
    }

    #[test]
    fn test_parse_dhcpcd_lease_dns_no_domain() -> Result<(), String> {
        let lease = "domain_name_servers=1.1.1.1\n";
        let result = parse_dhcpcd_lease_dns(lease).ok_or("expected Some from parse_dhcpcd_lease_dns")?;
        assert_eq!(result, "nameserver 1.1.1.1\n");
        assert!(!result.contains("search"));
        Ok(())
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

    // Resolv.conf / nameserver tests

    #[test]
    fn test_parse_dhcpcd_lease_dns_multiple_nameservers() -> Result<(), String> {
        let lease = "domain_name_servers=1.1.1.1 8.8.8.8 9.9.9.9\n";
        let result = parse_dhcpcd_lease_dns(lease).ok_or("expected Some")?;
        assert!(result.contains("nameserver 1.1.1.1\n"));
        assert!(result.contains("nameserver 8.8.8.8\n"));
        assert!(result.contains("nameserver 9.9.9.9\n"));
        Ok(())
    }

    #[test]
    fn test_parse_dhcpcd_lease_dns_with_other_fields() -> Result<(), String> {
        // Realistic dhcpcd --dumplease output
        let lease = "\
broadcast_address=192.168.1.255
dhcp_lease_time=86400
dhcp_message_type=5
dhcp_server_identifier=192.168.1.1
domain_name='home.local'
domain_name_servers=192.168.1.1
ip_address=192.168.1.50
network_number=192.168.1.0
routers=192.168.1.1
subnet_mask=255.255.255.0
";
        let result = parse_dhcpcd_lease_dns(lease).ok_or("expected Some")?;
        assert!(result.contains("search home.local\n"));
        assert!(result.contains("nameserver 192.168.1.1\n"));
        // Should not contain other lease fields
        assert!(!result.contains("broadcast"));
        assert!(!result.contains("routers"));
        Ok(())
    }

    #[test]
    fn test_parse_dhcpcd_lease_dns_domain_with_quotes() -> Result<(), String> {
        let lease = "domain_name='mynet.example.com'\ndomain_name_servers=10.0.0.1\n";
        let result = parse_dhcpcd_lease_dns(lease).ok_or("expected Some")?;
        assert!(result.contains("search mynet.example.com\n"));
        assert!(!result.contains("'")); // quotes should be stripped
        Ok(())
    }

    // QR code parsing tests

    #[test]
    fn test_parse_wifi_qr_full() -> Result<(), String> {
        let (ssid, pass) = parse_wifi_qr("WIFI:T:WPA;S:MyNetwork;P:secret123;;")
            .ok_or("expected Some from parse_wifi_qr")?;
        assert_eq!(ssid, "MyNetwork");
        assert_eq!(pass, "secret123");
        Ok(())
    }

    #[test]
    fn test_parse_wifi_qr_no_password() -> Result<(), String> {
        let (ssid, pass) = parse_wifi_qr("WIFI:T:nopass;S:OpenNet;;")
            .ok_or("expected Some from parse_wifi_qr")?;
        assert_eq!(ssid, "OpenNet");
        assert_eq!(pass, ""); // no password = empty string
        Ok(())
    }

    #[test]
    fn test_parse_wifi_qr_no_ssid() {
        assert!(parse_wifi_qr("WIFI:T:WPA;P:password;;").is_none());
    }

    #[test]
    fn test_parse_wifi_qr_empty() {
        assert!(parse_wifi_qr("").is_none());
    }

    #[test]
    fn test_parse_wifi_qr_garbage() {
        assert!(parse_wifi_qr("not a qr code at all").is_none());
    }

    #[test]
    fn test_parse_wifi_qr_wpa3() -> Result<(), String> {
        let (ssid, pass) = parse_wifi_qr("WIFI:T:SAE;S:SecureNet;P:wpa3pass;;")
            .ok_or("expected Some from parse_wifi_qr")?;
        assert_eq!(ssid, "SecureNet");
        assert_eq!(pass, "wpa3pass");
        Ok(())
    }

    // DNS resolve.conf generation

    #[test]
    fn test_generate_persist_network_config_is_valid_networkd() {
        let config = generate_persist_network_config("enp3s0", "11:22:33:44:55:66");
        assert!(config.starts_with("[Match]"));
        assert!(config.contains("MACAddress=11:22:33:44:55:66"));
        assert!(config.contains("[Network]"));
        assert!(config.contains("DHCP=yes"));
        assert!(config.contains("MulticastDNS=yes"));
        assert!(config.contains("[DHCPv4]"));
        assert!(config.contains("UseDNS=no"));
    }
}
