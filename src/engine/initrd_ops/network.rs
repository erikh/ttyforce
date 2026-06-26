use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::path::{Path, PathBuf};

use crate::detect::network::{parse_iw_scan, parse_iwlist_scan};
use crate::engine::feedback::OperationResult;
use crate::network::wifi::WifiNetwork;
use crate::network::PUBLIC_FALLBACK_DNS;

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
    cmd_log_append(format!("  waiting for carrier on {} (up to 5s) ...", interface));
    let carrier_path = format!("/sys/class/net/{}/carrier", interface);
    let mut last_state: Option<String> = None;
    for i in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let val = fs::read_to_string(&carrier_path)
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|e| format!("err:{}", e));
        if val == "1" {
            cmd_log_append(format!("  -> carrier up on {} after {}ms", interface, (i + 1) * 100));
            return OperationResult::Success;
        }
        // Heartbeat once per second so the log never appears stalled.
        if (i + 1) % 10 == 0 {
            cmd_log_append(format!(
                "  ... {} carrier={} ({}ms elapsed)",
                interface,
                val,
                (i + 1) * 100
            ));
        }
        last_state = Some(val);
    }

    cmd_log_append(format!(
        "  -> no carrier on {} after 5s (last carrier={}) (continuing)",
        interface,
        last_state.as_deref().unwrap_or("?")
    ));
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
///
/// The full DHCP handshake is two stages from our perspective:
///   1. an IPv4 address appears on the interface
///   2. a default route (gateway) is installed in `/proc/net/route`
///
/// dhcpcd installs the address slightly before the route, so callers that
/// immediately check for an upstream router can race the lease and falsely
/// declare "no router". We poll for both the address (up to 30s) and the
/// default route (an additional 15s once the address is up) before
/// returning. Each attempt is logged so the log never appears to stall.
pub fn configure_dhcp(interface: &str) -> OperationResult {
    let result = configure_dhcp_with(
        interface,
        try_trigger_dhcp,
        check_ip_sysfs,
        check_default_route_proc,
        30,
        15,
        std::time::Duration::from_secs(1),
    );

    if result.is_success() {
        // IP and route are both up — write resolv.conf from the lease now
        write_resolv_conf_from_lease(interface);
    }

    result
}

/// Return true if a default route (IPv4 *or* IPv6) is installed for
/// `interface`. Used to confirm the DHCP handshake completed end-to-end. A
/// dual-stack lease only needs one family's gateway to call the handshake done;
/// the other family is allowed to settle on its own.
fn check_default_route_proc(interface: &str) -> bool {
    if let Ok(content) = fs::read_to_string("/proc/net/route") {
        if parse_ipv4_default_route(&content, interface).is_some() {
            return true;
        }
    }
    if let Ok(content) = fs::read_to_string("/proc/net/ipv6_route") {
        if parse_ipv6_default_route(&content, interface).is_some() {
            return true;
        }
    }
    false
}

/// Testable inner function with injected dependencies.
///
/// Polls in two phases:
///   1. up to `ip_attempts` ticks waiting for an IPv4 address
///   2. up to `route_attempts` additional ticks waiting for the default route
fn configure_dhcp_with(
    interface: &str,
    trigger: fn(&str) -> OperationResult,
    check_ip: fn(&str) -> OperationResult,
    check_route: fn(&str) -> bool,
    ip_attempts: u32,
    route_attempts: u32,
    poll_interval: std::time::Duration,
) -> OperationResult {
    cmd_log_append(format!(
        "$ dhcp handshake on {} (lease {}s + route {}s)",
        interface,
        ip_attempts as u64 * poll_interval.as_secs(),
        route_attempts as u64 * poll_interval.as_secs(),
    ));

    let dhcp_triggered = trigger(interface);
    if let OperationResult::Error(e) = dhcp_triggered {
        cmd_log_append(format!("  -> dhcp trigger FAILED: {}", e));
        return OperationResult::Error(e);
    }

    // Phase 1: wait for an IP address.
    let mut got_ip: Option<String> = None;
    for attempt in 1..=ip_attempts {
        std::thread::sleep(poll_interval);
        match check_ip(interface) {
            OperationResult::IpAssigned(ip) => {
                cmd_log_append(format!(
                    "  -> {} acquired {} after {}s",
                    interface,
                    ip,
                    attempt as u64 * poll_interval.as_secs()
                ));
                got_ip = Some(ip);
                break;
            }
            _ => {
                cmd_log_append(format!(
                    "  ... {} waiting for lease ({}/{})",
                    interface, attempt, ip_attempts
                ));
            }
        }
    }

    if got_ip.is_none() {
        return OperationResult::Error(format!(
            "DHCP timeout on {}: no IP assigned after {}s",
            interface,
            ip_attempts as u64 * poll_interval.as_secs()
        ));
    }

    // Phase 2: wait for default route. Don't fail the whole DHCP step if
    // the route never shows up — the upstream-router check will retry on
    // its own and the user will see a clear "no router" error from the
    // state machine. We just want to give the lease time to settle so the
    // first router check has a fighting chance.
    if check_route(interface) {
        cmd_log_append(format!("  -> {} default route already installed", interface));
        return OperationResult::Success;
    }
    for attempt in 1..=route_attempts {
        std::thread::sleep(poll_interval);
        if check_route(interface) {
            cmd_log_append(format!(
                "  -> {} default route installed after {}s",
                interface,
                attempt as u64 * poll_interval.as_secs()
            ));
            return OperationResult::Success;
        }
        cmd_log_append(format!(
            "  ... {} waiting for default route ({}/{})",
            interface, attempt, route_attempts
        ));
    }

    cmd_log_append(format!(
        "  -> {} default route not installed after {}s (continuing — router check will retry)",
        interface,
        route_attempts as u64 * poll_interval.as_secs()
    ));
    OperationResult::Success
}

/// Directories dhcpcd needs before it can obtain and persist a lease in the
/// initrd. The initrd root is a fresh tmpfs with none of these present, and
/// dhcpcd will not write its lease database (read back by `--dumplease`/`-U`)
/// or create its run dir / control socket without them. Without the lease the
/// DHCP-offered DNS is unreadable and the resolver check has to fall back to
/// the public servers — so ttyforce creates these itself rather than relying on
/// the initrd's hook script or on dhcpcd's own hook scripts, which are not
/// bundled in the initrd.
const DHCPCD_DIRS: [&str; 3] = ["/run/dhcpcd", "/run/dhcpcd/resolv.conf", "/var/db/dhcpcd"];

/// Create the dhcpcd directories under `root`, returning the paths created (or
/// that already existed). Split out from [`prepare_dhcpcd_dirs`] so tests can
/// point it at a temporary root instead of the real filesystem.
pub fn prepare_dhcpcd_dirs_in(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut made = Vec::new();
    for dir in DHCPCD_DIRS {
        // Strip the leading '/' so the join stays under `root` — joining an
        // absolute path would discard `root` entirely.
        let path = root.join(dir.trim_start_matches('/'));
        fs::create_dir_all(&path).map_err(|e| format!("mkdir {}: {}", path.display(), e))?;
        made.push(path);
    }
    Ok(made)
}

/// Create the dhcpcd directories on the real initrd filesystem. Best-effort:
/// failures are logged but do not abort DHCP, since `dhcpcd -U` against the
/// running daemon can still yield the lease without the on-disk database.
fn prepare_dhcpcd_dirs() {
    match prepare_dhcpcd_dirs_in(Path::new("/")) {
        Ok(_) => cmd_log_append("$ prepared dhcpcd dirs (/run/dhcpcd, /var/db/dhcpcd)".to_string()),
        Err(e) => cmd_log_append(format!("  prepare dhcpcd dirs: {}", e)),
    }
}

/// Trigger DHCP on an interface via dhcpcd.
fn try_trigger_dhcp(interface: &str) -> OperationResult {
    prepare_dhcpcd_dirs();
    if let Err(e) = run_cmd("dhcpcd", &["--release", interface]) {
        cmd_log_append(format!("  dhcpcd release: {}", e));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    match run_cmd("dhcpcd", &["-b", interface]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("dhcpcd failed on {}: {}", interface, e)),
    }
}

/// dhcpcd run directories where, when not driving an external resolvconf,
/// dhcpcd writes per-interface resolv.conf fragments (`<iface>.<proto>`).
/// The newer `/run` path is tried first, then the legacy `/var/run` path.
const DHCPCD_RESOLV_DIRS: [&str; 2] = ["/run/dhcpcd/resolv.conf", "/var/run/dhcpcd/resolv.conf"];

/// Determine the DHCP-provided DNS for `interface` as resolv.conf content.
///
/// Sources, most reliable first:
///   1. dhcpcd run-dir resolv fragments (`/run/dhcpcd/resolv.conf/<iface>.*`).
///      These are already `nameserver`-formatted, keyed by interface, and
///      need neither the lease database nor a version-specific dump
///      subcommand — so they work even when `--dumplease` finds nothing.
///   2. `dhcpcd --dumplease <iface>` (lease database, env format).
///   3. `dhcpcd -U <iface>` (alternative dump format).
///
/// Returns None if no source yields a nameserver, leaving the caller to fall
/// back to whatever is already in /etc/resolv.conf.
fn dhcp_resolv_content(interface: &str) -> Option<String> {
    if let Some(content) = read_dhcpcd_rundir_resolv(interface) {
        return Some(content);
    }
    for args in [["--dumplease", interface], ["-U", interface]] {
        if let Ok(output) = run_cmd("dhcpcd", &args) {
            if let Some(content) = parse_dhcpcd_lease_dns(&output) {
                return Some(content);
            }
        }
    }
    None
}

/// Read dhcpcd's per-interface resolv.conf fragments from its run directory
/// and combine them into resolv.conf content. Returns None if no fragment
/// with a nameserver exists.
fn read_dhcpcd_rundir_resolv(interface: &str) -> Option<String> {
    let fragments = read_resolv_fragments_from_dirs(&DHCPCD_RESOLV_DIRS, interface);
    combine_resolv_fragments(&fragments)
}

/// Read the contents of every `<interface>.*` file in each of `dirs`.
/// dhcpcd names its fragments `<iface>.dhcp`, `<iface>.ra`, etc. Missing or
/// unreadable directories/files are silently skipped (best-effort).
fn read_resolv_fragments_from_dirs(dirs: &[&str], interface: &str) -> Vec<String> {
    let prefix = format!("{}.", interface);
    let mut fragments = Vec::new();
    for dir in dirs {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(&prefix) {
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    fragments.push(content);
                }
            }
        }
    }
    fragments
}

/// Combine resolv.conf fragments into a single resolv.conf body. Collects
/// `nameserver` lines (de-duplicated, order preserved) and the first
/// `search`/`domain` line seen. Returns None if no nameserver is present.
fn combine_resolv_fragments(fragments: &[String]) -> Option<String> {
    let mut nameservers: Vec<String> = Vec::new();
    let mut search: Option<String> = None;

    for frag in fragments {
        for line in frag.lines() {
            let line = line.trim();
            if let Some(ns) = line.strip_prefix("nameserver") {
                let ns = ns.trim();
                if !ns.is_empty() && !nameservers.iter().any(|n| n == ns) {
                    nameservers.push(ns.to_string());
                }
            } else if search.is_none() {
                // dhcpcd fragments may carry the domain as `search` or `domain`.
                let dom = line
                    .strip_prefix("search")
                    .or_else(|| line.strip_prefix("domain"));
                if let Some(d) = dom {
                    let d = d.trim();
                    if !d.is_empty() {
                        search = Some(d.to_string());
                    }
                }
            }
        }
    }

    if nameservers.is_empty() {
        return None;
    }

    let mut content = String::new();
    if let Some(d) = search {
        content.push_str(&format!("search {}\n", d));
    }
    for ns in &nameservers {
        content.push_str(&format!("nameserver {}\n", ns));
    }
    Some(content)
}

/// Determine the DHCP-provided DNS and write /etc/resolv.conf.
/// Best-effort: if no DHCP source yields a nameserver, leave any existing
/// resolv.conf nameservers in place, and only as a last resort write the
/// public fallback resolvers.
fn write_resolv_conf_from_lease(interface: &str) {
    if let Some(content) = dhcp_resolv_content(interface) {
        cmd_log_append("$ write /etc/resolv.conf from DHCP DNS".to_string());
        for line in content.lines() {
            cmd_log_append(format!("  {}", line));
        }
        if let Err(e) = fs::write("/etc/resolv.conf", &content) {
            cmd_log_append(format!("  write resolv.conf failed: {}", e));
        }
        return;
    }

    // Fall back to resolv.conf if dhcpcd hooks already populated it.
    if let Ok(existing) = fs::read_to_string("/etc/resolv.conf") {
        if existing.lines().any(|l| l.trim().starts_with("nameserver")) {
            cmd_log_append("  /etc/resolv.conf already has nameservers".to_string());
            return;
        }
    }

    // Last resort: write a default resolv.conf from the shared public list.
    cmd_log_append("$ write /etc/resolv.conf with fallback nameservers".to_string());
    let fallback: String = PUBLIC_FALLBACK_DNS
        .iter()
        .map(|ns| format!("nameserver {}\n", ns))
        .collect();
    if let Err(e) = fs::write("/etc/resolv.conf", &fallback) {
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

/// Read the assigned address for an interface, accepting either family. IPv4 is
/// preferred when present (it is what most install-time checks exercise first),
/// but a global IPv6 address alone is enough to consider the interface
/// addressed on an IPv6-only network.
fn check_ip_sysfs(interface: &str) -> OperationResult {
    cmd_log_append(format!("$ check IP address on {} (IPv4 + IPv6)", interface));
    if let Some(ip) = super::syscall::get_interface_ipv4(interface) {
        cmd_log_append(format!("  -> {} (IPv4)", ip));
        return OperationResult::IpAssigned(ip.to_string());
    }
    if let Some(ip) = super::syscall::get_interface_ipv6(interface) {
        cmd_log_append(format!("  -> {} (IPv6)", ip));
        return OperationResult::IpAssigned(ip.to_string());
    }
    cmd_log_append("  -> no IP assigned".to_string());
    OperationResult::NoIp
}

/// Check for an upstream router by parsing the kernel routing tables. An IPv4
/// default route (`/proc/net/route`) is reported first; failing that, an IPv6
/// default route (`/proc/net/ipv6_route`) is accepted so IPv6-only networks
/// still pass the upstream-router gate.
pub fn check_upstream_router(interface: &str) -> OperationResult {
    cmd_log_append(format!(
        "$ check default route for {} (/proc/net/route + ipv6_route)",
        interface
    ));
    if let Ok(content) = fs::read_to_string("/proc/net/route") {
        if let Some(gw) = parse_ipv4_default_route(&content, interface) {
            cmd_log_append(format!("  -> gateway {} via {} (IPv4)", gw, interface));
            return OperationResult::RouterFound(gw.to_string());
        }
    }
    if let Ok(content) = fs::read_to_string("/proc/net/ipv6_route") {
        if let Some(gw) = parse_ipv6_default_route(&content, interface) {
            cmd_log_append(format!("  -> gateway {} via {} (IPv6)", gw, interface));
            return OperationResult::RouterFound(gw.to_string());
        }
    }
    cmd_log_append(format!("  -> no default route on {}", interface));
    OperationResult::NoRouter
}

/// Parse `/proc/net/route` for the IPv4 default-route gateway on `interface`.
/// The destination field is `00000000` for the default route; gateways are
/// stored as little-endian hex on x86.
fn parse_ipv4_default_route(content: &str, interface: &str) -> Option<Ipv4Addr> {
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        if fields[0] != interface || fields[1] != "00000000" {
            continue;
        }
        if let Ok(gw) = u32::from_str_radix(fields[2], 16) {
            if gw != 0 {
                // /proc/net/route prints the gateway as the host-order (LE on
                // x86/aarch64) integer of the network-order address, so the
                // parsed value's octets are reversed — swap them back.
                return Some(Ipv4Addr::from(gw.swap_bytes()));
            }
        }
    }
    None
}

/// Parse `/proc/net/ipv6_route` for the IPv6 default-route gateway on
/// `interface`. Fields are whitespace-separated:
///   dest_net dest_plen src_net src_plen next_hop metric refcnt use flags dev
/// A default route has an all-zero destination with prefix length `00`; the
/// gateway is the `next_hop` column (often the router's link-local address).
fn parse_ipv6_default_route(content: &str, interface: &str) -> Option<Ipv6Addr> {
    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        if fields[9] != interface {
            continue;
        }
        if fields[0] != "00000000000000000000000000000000" || fields[1] != "00" {
            continue;
        }
        let gw = parse_proc_hex_ipv6(fields[4])?;
        if !gw.is_unspecified() {
            return Some(gw);
        }
    }
    None
}

/// Parse a 32-hex-char IPv6 address as stored (no colons) in
/// `/proc/net/ipv6_route`.
fn parse_proc_hex_ipv6(hex: &str) -> Option<Ipv6Addr> {
    if hex.len() != 32 {
        return None;
    }
    let mut octets = [0u8; 16];
    for (i, octet) in octets.iter_mut().enumerate() {
        *octet = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(Ipv6Addr::from(octets))
}

/// Check internet routability by pinging the public fallback resolvers. Each
/// IPv4 resolver in the shared list is tried in turn; if none reply, IPv6
/// (2606:4700:4700::1111) is tried *only when the interface carries a global
/// **unicast** IPv6 address (2000::/3)*, so a working connection on either stack
/// is enough to proceed while IPv4-only stacks — and stacks with only a
/// non-routable ULA (e.g. the dev VM's SLAAC ULA from libvirt's NAT bridge) —
/// never wait on (or report failures for) an IPv6 probe that cannot succeed.
pub fn check_internet_routability(interface: &str) -> OperationResult {
    check_internet_routability_inner(
        interface,
        |ip| super::syscall::icmp_ping(ip, std::time::Duration::from_secs(3)),
        super::syscall::interface_has_global_unicast_ipv6,
        |ip| super::syscall::icmp_ping6(ip, std::time::Duration::from_secs(3)),
    )
}

/// Testable core of [`check_internet_routability`] with injected probes.
///
/// `has_ipv6` gates the IPv6 fallback: when the interface has no global-unicast
/// IPv6 address there is no routable IPv6 in the stack, so `ping6` is never
/// invoked.
fn check_internet_routability_inner(
    interface: &str,
    ping4: impl Fn(Ipv4Addr) -> Result<(), String>,
    has_ipv6: impl Fn(&str) -> bool,
    ping6: impl Fn(Ipv6Addr) -> Result<(), String>,
) -> OperationResult {
    for addr in PUBLIC_FALLBACK_DNS {
        let ip: Ipv4Addr = match addr.parse() {
            Ok(ip) => ip,
            Err(_) => continue,
        };
        cmd_log_append(format!("$ ping {} (ICMPv4 echo)", ip));
        match ping4(ip) {
            Ok(_) => {
                cmd_log_append("  -> reply received (IPv4)".to_string());
                return OperationResult::InternetReachable;
            }
            Err(e) => {
                cmd_log_append(format!("  -> {} unreachable: {}", ip, e));
            }
        }
    }

    if !has_ipv6(interface) {
        cmd_log_append(format!(
            "  -> no global IPv6 address on {}, skipping IPv6 routability check",
            interface
        ));
        return OperationResult::NoInternet;
    }

    let v6 = Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111);
    cmd_log_append(format!("$ ping {} (ICMPv6 echo)", v6));
    match ping6(v6) {
        Ok(_) => {
            cmd_log_append("  -> reply received (IPv6)".to_string());
            OperationResult::InternetReachable
        }
        Err(e) => {
            cmd_log_append(format!("  -> IPv6 unreachable: {}", e));
            OperationResult::NoInternet
        }
    }
}

/// Check DNS resolution using a direct UDP DNS query.
///
/// The query is sent to the DNS server handed out by DHCP for `interface`
/// (read straight from the dhcpcd lease) so the check validates the
/// nameserver the network actually provided. If no lease DNS is available
/// it falls back to the first nameserver in `/etc/resolv.conf`.
pub fn check_dns_resolution(interface: &str, hostname: &str) -> OperationResult {
    cmd_log_append(format!("$ dns resolve {}", hostname));
    match dns_resolve(interface, hostname) {
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

/// Resolve a hostname, trying nameservers in priority order: every DHCP-offered
/// resolver first, then anything already in /etc/resolv.conf, then the public
/// fallback resolvers. The first server that returns an A record wins; a server
/// that is filtered, times out, or errors is skipped and the next is tried.
/// This is what lets the check succeed on a network that drops queries to
/// 1.1.1.1 but runs its own resolver (handed out via DHCP) — e.g. the libvirt
/// NAT's dnsmasq at 192.168.122.1.
fn dns_resolve(interface: &str, hostname: &str) -> Result<String, String> {
    let candidates = nameserver_candidates(interface);
    if candidates.is_empty() {
        return Err("no nameserver found".to_string());
    }
    cmd_log_append(format!("  nameservers (in order): {}", candidates.join(", ")));

    let dests: Vec<SocketAddr> = candidates
        .iter()
        .filter_map(|ns| match parse_nameserver_socket(ns) {
            Some(d) => Some(d),
            None => {
                cmd_log_append(format!("  skipping invalid nameserver: {}", ns));
                None
            }
        })
        .collect();

    resolve_via(&dests, hostname, std::time::Duration::from_secs(3))
}

/// Build the ordered, deduplicated nameserver candidate list for `interface`:
/// DHCP-offered resolvers, then /etc/resolv.conf, then the public fallback.
fn nameserver_candidates(interface: &str) -> Vec<String> {
    // DHCP-offered resolvers first, then the default gateway, then anything in
    // /etc/resolv.conf, then the public fallback. The gateway belongs in the
    // local tier because in NAT and home-router setups it runs a DNS forwarder
    // (e.g. the libvirt dev VM's dnsmasq at 192.168.122.1, which forwards to the
    // host's real resolvers) — and on networks that filter outbound DNS to
    // public resolvers it is frequently the ONLY resolver that answers. Folding
    // it in here means it's tried even when the DHCP lease's DNS can't be read
    // back (no hook fragments / no lease DB), which is exactly the dev-VM case.
    let mut local = dhcp_resolv_content(interface)
        .map(|c| all_nameservers(&c))
        .unwrap_or_default();
    if let Some(gw) = default_gateway_v4(interface) {
        if !local.contains(&gw) {
            local.push(gw);
        }
    }
    let resolv = fs::read_to_string("/etc/resolv.conf")
        .ok()
        .map(|c| all_nameservers(&c))
        .unwrap_or_default();
    order_nameservers(&local, &resolv, &PUBLIC_FALLBACK_DNS)
}

/// The IPv4 default-route gateway for `interface`, as a string, if any. In NAT
/// and home-router setups the gateway runs a DNS forwarder, so it's a useful
/// resolver candidate — and on networks that filter public DNS it may be the
/// only one that works (e.g. the libvirt dev VM's dnsmasq at 192.168.122.1).
fn default_gateway_v4(interface: &str) -> Option<String> {
    let content = fs::read_to_string("/proc/net/route").ok()?;
    parse_ipv4_default_route(&content, interface).map(|gw| gw.to_string())
}

/// Merge nameserver sources into one ordered, deduplicated list. DHCP-offered
/// resolvers come first (the local network's own resolver, which answers even
/// when public DNS is filtered), then /etc/resolv.conf, then the public
/// fallback resolvers as a last resort. Duplicates are dropped keeping the
/// first (highest-priority) occurrence, so the priority order is preserved.
pub fn order_nameservers(dhcp: &[String], resolv: &[String], fallback: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |ns: &str, out: &mut Vec<String>| {
        let ns = ns.trim();
        if !ns.is_empty() && !out.iter().any(|n| n == ns) {
            out.push(ns.to_string());
        }
    };
    for ns in dhcp {
        push(ns, &mut out);
    }
    for ns in resolv {
        push(ns, &mut out);
    }
    for ns in fallback {
        push(ns, &mut out);
    }
    out
}

/// Try each nameserver in order, returning the first hostname resolution that
/// succeeds. A server that is filtered, times out, or errors is logged and
/// skipped, and the next candidate is tried. Returns the last error if every
/// server fails. `timeout` bounds the wait for each individual server's reply.
pub fn resolve_via(
    candidates: &[SocketAddr],
    hostname: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let query =
        build_dns_query(hostname).map_err(|e| format!("failed to build DNS query: {}", e))?;
    let mut last_err = "no nameservers to try".to_string();
    for dest in candidates {
        match query_one(*dest, &query, timeout) {
            Ok(ip) => {
                cmd_log_append(format!("  resolved via {}", dest));
                return Ok(ip);
            }
            Err(e) => {
                cmd_log_append(format!("  {} failed: {}", dest, e));
                last_err = e;
            }
        }
    }
    Err(last_err)
}

/// Send a single DNS A query to `dest` and parse the first A record from the
/// reply. Binds an ephemeral socket of the same family as `dest` (an IPv4
/// socket cannot send to an IPv6 resolver and vice versa).
fn query_one(
    dest: SocketAddr,
    query: &[u8],
    timeout: std::time::Duration,
) -> Result<String, String> {
    let bind_addr = if dest.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
    let sock = UdpSocket::bind(bind_addr).map_err(|e| format!("bind: {}", e))?;
    sock.set_read_timeout(Some(timeout))
        .map_err(|e| format!("set_read_timeout: {}", e))?;
    sock.send_to(query, dest).map_err(|e| format!("send: {}", e))?;
    let mut buf = [0u8; 512];
    let n = sock.recv(&mut buf).map_err(|e| format!("recv: {}", e))?;
    parse_dns_response(&buf[..n])
}

/// Collect every `nameserver` value from resolv.conf-formatted content, in
/// order of appearance. Empty values are skipped. Used to gather all
/// DHCP-offered and /etc/resolv.conf resolvers as fallback candidates.
fn all_nameservers(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .map(str::trim)
        .filter(|ns| !ns.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse a resolv.conf `nameserver` value into a UDP socket address on port 53.
/// Accepts plain IPv4 (`1.1.1.1`), IPv6 (`2001:db8::1`), and zoned IPv6
/// link-local addresses (`fe80::1%eth0`) — the zone is resolved to a kernel
/// interface index so the query can be sent out the right link.
fn parse_nameserver_socket(nameserver: &str) -> Option<SocketAddr> {
    if let Some((addr, zone)) = nameserver.split_once('%') {
        let ip: Ipv6Addr = addr.parse().ok()?;
        let scope_id = ifindex_for_zone(zone).unwrap_or(0);
        Some(SocketAddr::V6(SocketAddrV6::new(ip, 53, 0, scope_id)))
    } else {
        let ip: IpAddr = nameserver.parse().ok()?;
        Some(SocketAddr::new(ip, 53))
    }
}

/// Resolve an IPv6 zone identifier to a kernel interface index. A purely
/// numeric zone is used directly; a name is looked up via sysfs.
fn ifindex_for_zone(zone: &str) -> Option<u32> {
    if let Ok(idx) = zone.parse::<u32>() {
        return Some(idx);
    }
    let content = fs::read_to_string(format!("/sys/class/net/{}/ifindex", zone)).ok()?;
    content.trim().parse().ok()
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
    // `DHCP=yes` enables both the DHCPv4 client and the DHCPv6 client, and
    // `IPv6AcceptRA=yes` brings up SLAAC/RA-based IPv6 — so the installed system
    // comes up dual-stack regardless of which family the network offers. DNS is
    // suppressed on both clients (UseDNS=no) because resolution is handled
    // separately during install.
    let match_section = if mac_address.is_empty() || mac_address == "00:00:00:00:00:00" {
        // Fallback to name matching if MAC is unavailable
        format!("[Match]\nName={}\n", interface)
    } else {
        format!("[Match]\nMACAddress={}\n", mac_address)
    };
    format!(
        "{}\n[Network]\nDHCP=yes\nIPv6AcceptRA=yes\nMulticastDNS=yes\n\n[DHCPv4]\nUseDNS=no\n\n[DHCPv6]\nUseDNS=no\n",
        match_section
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
    fn test_routability_ipv4_reachable_skips_ipv6() {
        // First IPv4 ping succeeds: never reaches the IPv6 probe.
        let ping6_calls = AtomicU32::new(0);
        let result = check_internet_routability_inner(
            "eth0",
            |_| Ok(()),
            |_| panic!("has_ipv6 must not be consulted when IPv4 is reachable"),
            |_| {
                ping6_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        assert!(matches!(result, OperationResult::InternetReachable));
        assert_eq!(ping6_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_routability_no_ipv6_in_stack_skips_ipv6_probe() {
        // All IPv4 pings fail and the interface has no global IPv6 address, so
        // the IPv6 probe must be skipped entirely (no `ping -6`).
        let ping6_calls = AtomicU32::new(0);
        let result = check_internet_routability_inner(
            "eth0",
            |_| Err("filtered".to_string()),
            |_| false,
            |_| {
                ping6_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        assert!(matches!(result, OperationResult::NoInternet));
        assert_eq!(
            ping6_calls.load(Ordering::SeqCst),
            0,
            "IPv6 must not be probed when there is no IPv6 in the stack"
        );
    }

    #[test]
    fn test_routability_ipv6_used_when_present_and_ipv4_fails() {
        let ping6_calls = AtomicU32::new(0);
        let result = check_internet_routability_inner(
            "eth0",
            |_| Err("no v4".to_string()),
            |_| true,
            |_| {
                ping6_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        assert!(matches!(result, OperationResult::InternetReachable));
        assert_eq!(ping6_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_routability_ipv6_present_but_unreachable_is_no_internet() {
        let result = check_internet_routability_inner(
            "eth0",
            |_| Err("no v4".to_string()),
            |_| true,
            |_| Err("no v6".to_string()),
        );
        assert!(matches!(result, OperationResult::NoInternet));
    }

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
    fn test_generate_persist_network_config_handles_ipv6() {
        // DHCP=yes covers both families; RA + DHCPv6 must be present so the
        // installed system comes up dual-stack.
        let config = generate_persist_network_config("eth0", "aa:bb:cc:dd:ee:ff");
        assert!(config.contains("IPv6AcceptRA=yes"), "missing IPv6AcceptRA");
        assert!(config.contains("[DHCPv6]"), "missing [DHCPv6] section");
        // UseDNS=no must appear for both v4 and v6 clients.
        assert_eq!(config.matches("UseDNS=no").count(), 2, "config: {}", config);
    }

    #[test]
    fn test_generate_persist_network_config_name_fallback_handles_ipv6() {
        let config = generate_persist_network_config("wlan0", "");
        assert!(config.contains("Name=wlan0"));
        assert!(config.contains("IPv6AcceptRA=yes"));
        assert!(config.contains("[DHCPv6]"));
    }

    // ── IPv4/IPv6 default-route parsing ─────────────────────────────────

    #[test]
    fn test_parse_ipv4_default_route_found() {
        // dest=00000000 (default), gateway 0102A8C0 = 192.168.2.1 little-endian.
        let content = "\
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask
eth0\t0002A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF
eth0\t00000000\t0102A8C0\t0003\t0\t0\t0\t00000000";
        assert_eq!(
            parse_ipv4_default_route(content, "eth0"),
            Some("192.168.2.1".parse().unwrap())
        );
    }

    #[test]
    fn test_parse_ipv4_default_route_wrong_interface() {
        let content = "\
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask
eth0\t00000000\t0102A8C0\t0003\t0\t0\t0\t00000000";
        assert_eq!(parse_ipv4_default_route(content, "wlan0"), None);
    }

    #[test]
    fn test_parse_ipv4_default_route_none_when_no_default() {
        let content = "\
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask
eth0\t0002A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF";
        assert_eq!(parse_ipv4_default_route(content, "eth0"), None);
    }

    #[test]
    fn test_parse_ipv6_default_route_found() {
        // Default route (all-zero dest, plen 00) with a link-local gateway.
        let line = "00000000000000000000000000000000 00 \
00000000000000000000000000000000 00 \
fe800000000000000000000000000001 \
00000400 00000001 00000000 00000003 eth0";
        assert_eq!(
            parse_ipv6_default_route(line, "eth0"),
            Some("fe80::1".parse().unwrap())
        );
    }

    #[test]
    fn test_parse_ipv6_default_route_global_gateway() {
        let line = "00000000000000000000000000000000 00 \
00000000000000000000000000000000 00 \
20010db8000000000000000000000001 \
00000400 00000001 00000000 00000003 eth0";
        assert_eq!(
            parse_ipv6_default_route(line, "eth0"),
            Some("2001:db8::1".parse().unwrap())
        );
    }

    #[test]
    fn test_parse_ipv6_default_route_skips_non_default_and_other_iface() {
        // A /64 connected route (plen 40 hex = 64) and a default on wlan0.
        let content = "\
20010db8000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 eth0
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000099 00000400 00000001 00000000 00000003 wlan0";
        assert_eq!(parse_ipv6_default_route(content, "eth0"), None);
        assert_eq!(
            parse_ipv6_default_route(content, "wlan0"),
            Some("fe80::99".parse().unwrap())
        );
    }

    #[test]
    fn test_parse_proc_hex_ipv6_roundtrip() {
        assert_eq!(
            parse_proc_hex_ipv6("20010db8000000000000000000000001"),
            Some("2001:db8::1".parse().unwrap())
        );
        assert_eq!(parse_proc_hex_ipv6("tooshort"), None);
        assert_eq!(parse_proc_hex_ipv6("zz010db8000000000000000000000001"), None);
    }

    // ── Nameserver socket parsing (IPv4 / IPv6 / zoned) ─────────────────

    #[test]
    fn test_parse_nameserver_socket_ipv4() {
        let s = parse_nameserver_socket("1.1.1.1").expect("ipv4 ns");
        assert!(s.is_ipv4());
        assert_eq!(s.port(), 53);
        assert_eq!(s.ip().to_string(), "1.1.1.1");
    }

    #[test]
    fn test_parse_nameserver_socket_ipv6() {
        let s = parse_nameserver_socket("2606:4700:4700::1111").expect("ipv6 ns");
        assert!(s.is_ipv6());
        assert_eq!(s.port(), 53);
    }

    #[test]
    fn test_parse_nameserver_socket_ipv6_zoned_numeric() {
        // A numeric zone is used directly as the scope id.
        let s = parse_nameserver_socket("fe80::1%2").expect("zoned ns");
        match s {
            SocketAddr::V6(v6) => {
                assert_eq!(v6.scope_id(), 2);
                assert_eq!(v6.port(), 53);
            }
            _ => panic!("expected V6, got {:?}", s),
        }
    }

    #[test]
    fn test_parse_nameserver_socket_rejects_garbage() {
        assert!(parse_nameserver_socket("not-an-ip").is_none());
        assert!(parse_nameserver_socket("").is_none());
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
    fn test_nameserver_line_parsing() -> Result<(), String> {
        // Verifies the `nameserver` line format expectations that
        // all_nameservers relies on, independent of any filesystem read.
        let line = "nameserver 8.8.8.8";
        let ns = line
            .strip_prefix("nameserver")
            .ok_or("expected 'nameserver' prefix")?
            .trim();
        assert_eq!(ns, "8.8.8.8");
        Ok(())
    }

    #[test]
    fn test_all_nameservers_collects_in_order() {
        let content = "search lan\nnameserver 192.168.1.1\nnameserver 192.168.1.2\n";
        assert_eq!(
            all_nameservers(content),
            vec!["192.168.1.1".to_string(), "192.168.1.2".to_string()]
        );
    }

    #[test]
    fn test_all_nameservers_none_when_empty() {
        assert!(all_nameservers("search lan\n").is_empty());
        assert!(all_nameservers("").is_empty());
    }

    #[test]
    fn test_order_nameservers_dhcp_first_fallback_last() {
        // The DHCP-offered resolver leads; the public fallback trails, so a
        // filtered public resolver is never tried ahead of the working one.
        let dhcp = vec!["192.168.122.1".to_string()];
        let resolv: Vec<String> = vec![];
        let order = order_nameservers(&dhcp, &resolv, &["1.1.1.1", "8.8.8.8"]);
        assert_eq!(
            order,
            vec![
                "192.168.122.1".to_string(),
                "1.1.1.1".to_string(),
                "8.8.8.8".to_string(),
            ]
        );
    }

    #[test]
    fn test_order_nameservers_dedups_keeping_first_position() {
        // A resolver that appears in more than one source is listed once, at
        // its highest-priority (earliest) position.
        let dhcp = vec!["192.168.122.1".to_string()];
        let resolv = vec!["192.168.122.1".to_string(), "8.8.8.8".to_string()];
        let order = order_nameservers(&dhcp, &resolv, &["1.1.1.1", "8.8.8.8"]);
        assert_eq!(
            order,
            vec![
                "192.168.122.1".to_string(),
                "8.8.8.8".to_string(),
                "1.1.1.1".to_string(),
            ]
        );
    }

    #[test]
    fn test_order_nameservers_skips_blank_entries() {
        let dhcp: Vec<String> = vec![];
        let resolv = vec!["".to_string(), "   ".to_string()];
        let order = order_nameservers(&dhcp, &resolv, &["1.1.1.1"]);
        assert_eq!(order, vec!["1.1.1.1".to_string()]);
    }

    #[test]
    fn test_gateway_resolver_tried_before_public_fallback() {
        // The dev-VM case: the DHCP lease's DNS can't be read back, so the local
        // group is just the gateway (nameserver_candidates folds it into the
        // first arg). It must be tried before the public fallback, which on a
        // DNS-filtering network is the only way the check can succeed.
        let local = vec!["192.168.122.1".to_string()]; // gateway, no DHCP DNS
        let resolv: Vec<String> = vec![]; // bootstrap resolv elided for clarity
        let order = order_nameservers(&local, &resolv, &PUBLIC_FALLBACK_DNS);
        assert_eq!(order.first(), Some(&"192.168.122.1".to_string()));
        let gw_pos = order.iter().position(|n| n == "192.168.122.1").unwrap();
        let pub_pos = order.iter().position(|n| n == "8.8.8.8").unwrap();
        assert!(gw_pos < pub_pos, "gateway must precede public fallback: {:?}", order);
    }

    #[test]
    fn test_dhcp_dns_still_leads_gateway() {
        // When the DHCP DNS *is* readable, it stays ahead of the gateway (both
        // in the local group, DHCP pushed first by nameserver_candidates).
        let local = vec!["10.0.0.53".to_string(), "192.168.122.1".to_string()];
        let order = order_nameservers(&local, &[], &PUBLIC_FALLBACK_DNS);
        assert_eq!(
            order,
            vec![
                "10.0.0.53".to_string(),
                "192.168.122.1".to_string(),
                "8.8.8.8".to_string(),
                "8.8.4.4".to_string(),
                "1.1.1.1".to_string(),
            ]
        );
    }

    #[test]
    fn test_dhcp_lease_nameservers_lead_candidate_order() {
        // The DNS check feeds the parsed lease through all_nameservers, so the
        // servers it queries first are the ones DHCP handed out, ahead of the
        // public fallback — not a hardcoded resolver.
        let lease = "domain_name_servers=10.0.0.1 10.0.0.2\ndomain_name='lan'\n";
        let resolv = parse_dhcpcd_lease_dns(lease).expect("lease has DNS");
        let dhcp = all_nameservers(&resolv);
        assert_eq!(dhcp, vec!["10.0.0.1".to_string(), "10.0.0.2".to_string()]);

        let order = order_nameservers(&dhcp, &[], &PUBLIC_FALLBACK_DNS);
        assert_eq!(order.first(), Some(&"10.0.0.1".to_string()));
        assert_eq!(order.last(), Some(&"1.1.1.1".to_string()));
    }

    // DHCP polling tests

    #[test]
    fn test_dhcp_polling_immediate_ip() {
        let result = configure_dhcp_with(
            "eth0",
            |_| OperationResult::Success,
            |_| OperationResult::IpAssigned("10.0.0.5".into()),
            |_| true,
            5,
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
            |_| true,
            5,
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
            |_| true,
            3,
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
    fn test_dhcp_waits_for_default_route() {
        // IP appears immediately, but the route takes 3 ticks to install.
        // The dhcp step should wait for the route, not return on the IP alone.
        static ROUTE_TICKS: AtomicU32 = AtomicU32::new(0);
        ROUTE_TICKS.store(0, Ordering::SeqCst);
        let result = configure_dhcp_with(
            "eth0",
            |_| OperationResult::Success,
            |_| OperationResult::IpAssigned("10.0.0.5".into()),
            |_| {
                let n = ROUTE_TICKS.fetch_add(1, Ordering::SeqCst);
                n >= 3
            },
            5,
            10,
            Duration::from_millis(1),
        );
        assert!(result.is_success(), "expected Success, got {:?}", result);
        assert!(
            ROUTE_TICKS.load(Ordering::SeqCst) >= 4,
            "route checker should have been polled multiple times, got {}",
            ROUTE_TICKS.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn test_dhcp_succeeds_even_if_route_never_installs() {
        // Lease arrives but the gateway never appears. We still want Success
        // so the state machine moves on to its own router-check retry loop
        // and surfaces a clean "no router" error there.
        let result = configure_dhcp_with(
            "eth0",
            |_| OperationResult::Success,
            |_| OperationResult::IpAssigned("10.0.0.5".into()),
            |_| false,
            5,
            3,
            Duration::from_millis(1),
        );
        assert!(
            result.is_success(),
            "expected Success even without route, got {:?}",
            result
        );
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
            |_| true,
            5,
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

    // dhcpcd run-dir resolv fragment tests (the primary DNS source)

    #[test]
    fn test_combine_resolv_fragments_single() -> Result<(), String> {
        let frags = vec!["nameserver 192.168.1.1\n".to_string()];
        let out = combine_resolv_fragments(&frags).ok_or("expected Some")?;
        assert_eq!(out, "nameserver 192.168.1.1\n");
        Ok(())
    }

    #[test]
    fn test_combine_resolv_fragments_dedups_across_files() -> Result<(), String> {
        // dhcpcd may write one fragment per protocol; the same resolver can
        // appear in more than one. Order is preserved, duplicates dropped.
        let frags = vec![
            "nameserver 10.0.0.1\nnameserver 10.0.0.2\n".to_string(),
            "nameserver 10.0.0.1\nnameserver 10.0.0.3\n".to_string(),
        ];
        let out = combine_resolv_fragments(&frags).ok_or("expected Some")?;
        assert_eq!(out, "nameserver 10.0.0.1\nnameserver 10.0.0.2\nnameserver 10.0.0.3\n");
        Ok(())
    }

    #[test]
    fn test_combine_resolv_fragments_keeps_search_and_domain() -> Result<(), String> {
        let frags = vec!["search lan\nnameserver 192.168.1.1\n".to_string()];
        let out = combine_resolv_fragments(&frags).ok_or("expected Some")?;
        assert_eq!(out, "search lan\nnameserver 192.168.1.1\n");

        // `domain` is normalized to a `search` line.
        let frags = vec!["domain corp.example\nnameserver 10.1.1.1\n".to_string()];
        let out = combine_resolv_fragments(&frags).ok_or("expected Some")?;
        assert_eq!(out, "search corp.example\nnameserver 10.1.1.1\n");
        Ok(())
    }

    #[test]
    fn test_combine_resolv_fragments_none_without_nameserver() {
        // A search-only or empty fragment yields nothing — the caller then
        // falls through to the lease dump and finally /etc/resolv.conf.
        assert!(combine_resolv_fragments(&["search lan\n".to_string()]).is_none());
        assert!(combine_resolv_fragments(&[]).is_none());
        assert!(combine_resolv_fragments(&["".to_string()]).is_none());
    }

    #[test]
    fn test_read_resolv_fragments_from_dirs_matches_interface() -> Result<(), String> {
        // Build a hermetic dhcpcd run dir under the temp dir (never touches
        // the host's real /run/dhcpcd) and confirm only the requested
        // interface's fragments are read.
        let base = unique_temp_dir("resolv-frags");
        fs::create_dir_all(&base).map_err(|e| e.to_string())?;
        fs::write(base.join("eth0.dhcp"), "nameserver 192.168.50.1\n")
            .map_err(|e| e.to_string())?;
        fs::write(base.join("eth0.ra"), "nameserver fe80::1\n").map_err(|e| e.to_string())?;
        fs::write(base.join("wlan0.dhcp"), "nameserver 10.9.9.9\n")
            .map_err(|e| e.to_string())?;

        let dir = base.to_string_lossy().to_string();
        let frags = read_resolv_fragments_from_dirs(&[&dir], "eth0");
        let combined = combine_resolv_fragments(&frags).ok_or("expected Some")?;
        assert!(combined.contains("nameserver 192.168.50.1\n"));
        assert!(combined.contains("nameserver fe80::1\n"));
        assert!(!combined.contains("10.9.9.9"), "wlan0 must not leak into eth0");

        fs::remove_dir_all(&base).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn test_read_resolv_fragments_from_dirs_missing_dir_is_empty() {
        // A nonexistent run dir is not an error — returns no fragments so the
        // source chain moves on to the lease dump.
        let frags = read_resolv_fragments_from_dirs(&["/nonexistent/ttyforce/run"], "eth0");
        assert!(frags.is_empty());
    }

    /// Build a process-unique path under the system temp dir for hermetic
    /// filesystem tests. Avoids `Math.random`/clock use (forbidden in this
    /// crate's harness) by combining the pid with a per-call counter.
    fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("ttyforce-{}-{}-{}", tag, std::process::id(), n))
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
