use std::fs;

use zbus::zvariant::ObjectPath;

use crate::detect::network::{parse_iw_scan, parse_iwlist_scan};
use crate::engine::feedback::OperationResult;
use crate::network::wifi::WifiNetwork;

use super::{cmd_log_append, run_cmd};

/// Enable a network interface via networkctl.
pub fn enable_interface(interface: &str) -> OperationResult {
    match run_cmd("networkctl", &["up", interface]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("failed to enable {}: {}", interface, e)),
    }
}

/// Disable a network interface via networkctl.
pub fn disable_interface(interface: &str) -> OperationResult {
    match run_cmd("networkctl", &["down", interface]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("failed to disable {}: {}", interface, e)),
    }
}

/// Scan for wifi networks.
/// Tries wpa_supplicant dbus interface first, falls back to `iw dev scan`.
pub fn scan_wifi_networks(interface: &str) -> OperationResult {
    // Try wpa_supplicant dbus: trigger scan
    if let Ok(conn) = zbus::blocking::Connection::system() {
        if let Ok(iface_path) = wpa_supplicant_iface_path(interface) {
            let scan_result = conn.call_method(
                Some("fi.w1.wpa_supplicant1"),
                &iface_path,
                Some("fi.w1.wpa_supplicant1.Interface"),
                "Scan",
                &std::collections::HashMap::<String, zbus::zvariant::Value<'_>>::new(),
            );
            if scan_result.is_ok() {
                std::thread::sleep(std::time::Duration::from_secs(2));
                return receive_wifi_scan_results(interface);
            }
        }
    }

    // Fallback: iw scan
    match run_cmd("iw", &["dev", interface, "scan", "-u"]) {
        Ok(output) => {
            let specs = parse_iw_scan(&output);
            let networks: Vec<WifiNetwork> = specs.iter().map(WifiNetwork::from).collect();
            OperationResult::WifiScanResults(networks)
        }
        Err(e) => {
            // Try iwlist as second fallback
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

/// Receive wifi scan results from wpa_supplicant or iw.
pub fn receive_wifi_scan_results(interface: &str) -> OperationResult {
    if let Ok(conn) = zbus::blocking::Connection::system() {
        if let Ok(iface_path) = wpa_supplicant_iface_path(interface) {
            let result = conn.call_method(
                Some("fi.w1.wpa_supplicant1"),
                &iface_path,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("fi.w1.wpa_supplicant1.Interface", "BSSs"),
            );
            if let Ok(_reply) = result {
                // BSS parsing from dbus is complex; fall through to iw
            }
        }
    }

    // Fallback: use iw scan dump (cached results, no new scan)
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

/// Authenticate to a wifi network.
/// Tries wpa_supplicant dbus first, falls back to wpa_supplicant CLI.
pub fn authenticate_wifi(interface: &str, ssid: &str, password: &str) -> OperationResult {
    if let Ok(conn) = zbus::blocking::Connection::system() {
        if let Ok(iface_path) = wpa_supplicant_iface_path(interface) {
            // AddNetwork
            let add_result = conn.call_method(
                Some("fi.w1.wpa_supplicant1"),
                &iface_path,
                Some("fi.w1.wpa_supplicant1.Interface"),
                "AddNetwork",
                &std::collections::HashMap::from([
                    ("ssid", zbus::zvariant::Value::from(ssid)),
                    ("psk", zbus::zvariant::Value::from(password)),
                ]),
            );

            if let Ok(reply) = add_result {
                if let Ok(network_path) =
                    reply.body().deserialize::<zbus::zvariant::OwnedObjectPath>()
                {
                    // SelectNetwork
                    let select_result = conn.call_method(
                        Some("fi.w1.wpa_supplicant1"),
                        &iface_path,
                        Some("fi.w1.wpa_supplicant1.Interface"),
                        "SelectNetwork",
                        &network_path,
                    );
                    if select_result.is_ok() {
                        return OperationResult::WifiAuthenticated;
                    }
                }
            }
        }
    }

    // Fallback: write wpa_supplicant config and start it
    let conf_path = format!("/tmp/wpa_supplicant_{}.conf", interface);
    let conf_content = format!(
        "ctrl_interface=/var/run/wpa_supplicant\nnetwork={{\n  ssid=\"{}\"\n  psk=\"{}\"\n}}\n",
        ssid, password
    );
    if fs::write(&conf_path, conf_content).is_err() {
        return OperationResult::Error("failed to write wpa_supplicant config".into());
    }

    // Kill any existing wpa_supplicant for this interface (best-effort)
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

/// Configure wifi SSID and authentication — same as authenticate_wifi.
pub fn configure_wifi_ssid_auth(interface: &str, ssid: &str, password: &str) -> OperationResult {
    authenticate_wifi(interface, ssid, password)
}

/// Configure wifi from a QR code.
/// QR code format: WIFI:T:WPA;S:ssid;P:password;;
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

/// Configure DHCP on an interface via systemd-networkd.
/// After triggering DHCP, polls for an IP address (up to 30s) before returning.
pub fn configure_dhcp(interface: &str) -> OperationResult {
    configure_dhcp_with(
        interface,
        try_trigger_dhcp,
        check_ip_via_command,
        30,
        std::time::Duration::from_secs(1),
    )
}

/// Testable inner function with injected dependencies for DHCP trigger, IP check,
/// retry count, and poll interval.
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

/// Build the path for a ttyforce-managed networkd `.network` unit.
fn networkd_unit_path(interface: &str) -> String {
    format!("/etc/systemd/network/80-ttyforce-{}.network", interface)
}

/// Generate the networkd `.network` unit content for DHCP on an interface.
fn generate_dhcp_network_config(interface: &str) -> String {
    format!(
        "[Match]\nName={}\n\n[Network]\nDHCP=yes\nMulticastDNS=yes\n",
        interface
    )
}

/// Trigger DHCP on an interface via systemd-networkd.
///
/// Writes a `.network` unit with `DHCP=yes` for the interface, then tells
/// networkd to reload config and reconfigure the link.  DNS is handled by
/// systemd-resolved — no dhclient/dhcpcd needed.
fn try_trigger_dhcp(interface: &str) -> OperationResult {
    let network_path = networkd_unit_path(interface);
    let network_content = generate_dhcp_network_config(interface);
    if let Err(e) = fs::write(&network_path, network_content) {
        return OperationResult::Error(format!(
            "failed to write networkd config for {}: {}",
            interface, e
        ));
    }

    // Tell networkd to pick up the new config file
    if let Err(e) = run_cmd("networkctl", &["reload"]) {
        return OperationResult::Error(format!("networkctl reload failed: {}", e));
    }

    // Reconfigure the link to apply DHCP
    match run_cmd("networkctl", &["reconfigure", interface]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "DHCP configuration failed on {}: {}",
            interface, e
        )),
    }
}

/// Merge a `[DHCPv4]` section with `RouteMetric=100` into an existing networkd
/// config, or generate a fresh one.  Pure function — no I/O.
fn merge_primary_interface_config(interface: &str, existing: &str) -> String {
    if existing.contains("[DHCPv4]") {
        // Update existing DHCPv4 section
        if existing.contains("RouteMetric=") {
            existing
                .lines()
                .map(|l| {
                    if l.starts_with("RouteMetric=") {
                        "RouteMetric=100"
                    } else {
                        l
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        } else {
            existing.replace("[DHCPv4]", "[DHCPv4]\nRouteMetric=100")
        }
    } else if existing.is_empty() {
        format!(
            "[Match]\nName={}\n\n[Network]\nDHCP=yes\nMulticastDNS=yes\n\n[DHCPv4]\nRouteMetric=100\n",
            interface
        )
    } else {
        format!("{}\n[DHCPv4]\nRouteMetric=100\n", existing.trim_end())
    }
}

/// Select an interface as primary by writing a networkd config with a low
/// route metric and reconfiguring the link.
pub fn select_primary_interface(interface: &str) -> OperationResult {
    let network_path = networkd_unit_path(interface);

    // Read existing config or start fresh
    let existing = fs::read_to_string(&network_path).unwrap_or_default();
    let network_content = merge_primary_interface_config(interface, &existing);

    if let Err(e) = fs::write(&network_path, network_content) {
        return OperationResult::Error(format!(
            "failed to write networkd config for {}: {}",
            interface, e
        ));
    }

    if let Err(e) = run_cmd("networkctl", &["reload"]) {
        return OperationResult::Error(format!("networkctl reload failed: {}", e));
    }

    match run_cmd("networkctl", &["reconfigure", interface]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "failed to set primary interface {}: {}",
            interface, e
        )),
    }
}

/// Remove the ttyforce-managed networkd config for an interface and reload networkd.
/// Best-effort: missing files and reload failures are not errors.
pub fn cleanup_network_config(interface: &str) -> OperationResult {
    let path = networkd_unit_path(interface);
    match fs::remove_file(&path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return OperationResult::Error(format!(
                "failed to remove networkd config {}: {}",
                path, e
            ));
        }
    }
    // Best-effort reload
    if let Err(e) = run_cmd("networkctl", &["reload"]) {
        cmd_log_append(format!("  networkctl reload warning: {}", e));
    }
    OperationResult::Success
}

/// Kill wpa_supplicant for an interface and remove its config file.
/// Best-effort: missing processes and files are not errors.
pub fn cleanup_wpa_supplicant(interface: &str) -> OperationResult {
    // Kill any wpa_supplicant for this interface (best-effort)
    if let Err(e) = run_cmd("pkill", &["-f", &format!("wpa_supplicant.*{}", interface)]) {
        cmd_log_append(format!("  pkill wpa_supplicant: {}", e));
    }
    // Remove config file (ignore NotFound)
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

/// Shut down a network interface via networkctl.
pub fn shutdown_interface(interface: &str) -> OperationResult {
    match run_cmd("networkctl", &["down", interface]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("failed to shut down {}: {}", interface, e)),
    }
}

/// Check link availability via systemd-networkd dbus, falling back to sysfs.
pub fn check_link_availability(interface: &str) -> OperationResult {
    // Try networkd dbus first
    if let Ok(conn) = zbus::blocking::Connection::system() {
        if let Some(result) = check_link_via_networkd(&conn, interface) {
            return result;
        }
    }

    // Fallback: sysfs
    let carrier_path = format!("/sys/class/net/{}/carrier", interface);
    match fs::read_to_string(&carrier_path) {
        Ok(val) if val.trim() == "1" => OperationResult::LinkUp,
        Ok(_) => OperationResult::LinkDown,
        Err(_) => OperationResult::LinkDown,
    }
}

/// Check link via systemd-networkd dbus.
/// Returns Some(LinkUp) only on a positive match; returns None otherwise
/// so the caller falls through to sysfs.
fn check_link_via_networkd(
    conn: &zbus::blocking::Connection,
    interface: &str,
) -> Option<OperationResult> {
    let index = get_link_index_networkd(conn, interface).ok()?;
    let link_path = format!("/org/freedesktop/network1/link/_{}", index);

    let carrier_state = get_networkd_property(conn, &link_path, "CarrierState")?;

    match carrier_state.as_str() {
        "carrier" | "degraded-carrier" | "enslaved" => Some(OperationResult::LinkUp),
        // Don't trust a negative from networkd — it may not be managing this
        // interface. Fall through to sysfs check instead.
        _ => None,
    }
}

/// Check if an IP address is assigned to the interface.
/// Tries systemd-networkd dbus first, falls back to `ip` command.
pub fn check_ip_address(interface: &str) -> OperationResult {
    // Try networkd dbus first
    if let Ok(conn) = zbus::blocking::Connection::system() {
        if let Some(result) = check_ip_via_networkd(&conn, interface) {
            return result;
        }
    }

    // Fallback: ip command
    check_ip_via_command(interface)
}

/// Check IP address via systemd-networkd dbus.
/// Returns Some only on a positive match; returns None otherwise
/// so the caller falls through to the ip command check.
fn check_ip_via_networkd(
    conn: &zbus::blocking::Connection,
    interface: &str,
) -> Option<OperationResult> {
    let index = get_link_index_networkd(conn, interface).ok()?;
    let link_path = format!("/org/freedesktop/network1/link/_{}", index);

    let addr_state = get_networkd_property(conn, &link_path, "AddressState")?;

    match addr_state.as_str() {
        "routable" | "degraded" => {
            // networkd says we have an address; get the actual IP via ip command
            // since networkd doesn't directly expose the address string
            Some(check_ip_via_command(interface))
        }
        // Don't trust a negative from networkd — it may not be managing this
        // interface. Fall through to ip command check instead.
        _ => None,
    }
}

/// Check IP address via `ip -j addr show`.
fn check_ip_via_command(interface: &str) -> OperationResult {
    match run_cmd("ip", &["-j", "addr", "show", interface]) {
        Ok(output) => {
            if let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(&output) {
                for iface in &parsed {
                    if let Some(addr_info) = iface.get("addr_info").and_then(|a| a.as_array()) {
                        for addr in addr_info {
                            if addr.get("family").and_then(|f| f.as_str()) == Some("inet") {
                                if let Some(local) = addr.get("local").and_then(|l| l.as_str()) {
                                    return OperationResult::IpAssigned(local.to_string());
                                }
                            }
                        }
                    }
                }
            }
            OperationResult::NoIp
        }
        Err(e) => OperationResult::Error(format!("failed to check IP on {}: {}", interface, e)),
    }
}

/// Check for upstream router via systemd-networkd dbus, falling back to `ip route`.
pub fn check_upstream_router(interface: &str) -> OperationResult {
    // Try networkd dbus first: check if operational state is "routable"
    if let Ok(conn) = zbus::blocking::Connection::system() {
        if let Ok(index) = get_link_index_networkd(&conn, interface) {
            let link_path = format!("/org/freedesktop/network1/link/_{}", index);
            if let Some(oper_state) = get_networkd_property(&conn, &link_path, "OperationalState") {
                if oper_state == "routable" {
                    // We know there's a route; get the gateway via ip command
                    return check_router_via_command(interface);
                }
            }
        }
    }

    // Fallback
    check_router_via_command(interface)
}

/// Check upstream router via `ip route` command.
fn check_router_via_command(interface: &str) -> OperationResult {
    match run_cmd("ip", &["-j", "route", "show", "default", "dev", interface]) {
        Ok(output) => {
            if let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(&output) {
                for route in &parsed {
                    if let Some(gateway) = route.get("gateway").and_then(|g| g.as_str()) {
                        return OperationResult::RouterFound(gateway.to_string());
                    }
                }
            }
            OperationResult::NoRouter
        }
        Err(_) => OperationResult::NoRouter,
    }
}

/// Check internet routability by pinging 1.1.1.1.
pub fn check_internet_routability(_interface: &str) -> OperationResult {
    match run_cmd("ping", &["-c1", "-W3", "1.1.1.1"]) {
        Ok(_) => OperationResult::InternetReachable,
        Err(_) => OperationResult::NoInternet,
    }
}

/// Check DNS resolution for a hostname.
/// Tries systemd-resolved dbus first, falls back to dig/getent.
pub fn check_dns_resolution(_interface: &str, hostname: &str) -> OperationResult {
    // Try systemd-resolved dbus (org.freedesktop.resolve1)
    if let Ok(conn) = zbus::blocking::Connection::system() {
        if let Some(result) = resolve_via_resolved(&conn, hostname) {
            return result;
        }
    }

    // Fallback: dig
    if let Ok(output) = run_cmd("dig", &["+short", hostname]) {
        let trimmed = output.trim();
        if !trimmed.is_empty() && !trimmed.starts_with(";;") {
            if let Some(first_line) = trimmed.lines().next() {
                return OperationResult::DnsResolved(first_line.to_string());
            }
        }
    }

    // Fallback: getent
    match run_cmd("getent", &["hosts", hostname]) {
        Ok(output) => {
            if let Some(ip) = output.split_whitespace().next() {
                OperationResult::DnsResolved(ip.to_string())
            } else {
                OperationResult::DnsFailed(format!("no result for {}", hostname))
            }
        }
        Err(e) => OperationResult::DnsFailed(format!(
            "DNS resolution failed for {}: {}",
            hostname, e
        )),
    }
}

/// Resolve hostname via systemd-resolved dbus.
fn resolve_via_resolved(
    conn: &zbus::blocking::Connection,
    hostname: &str,
) -> Option<OperationResult> {
    // ResolveHostname(ifindex=0, name, family=AF_UNSPEC=0, flags=0)
    // Returns a(iiay) — array of (ifindex, family, address_bytes), canonical_name, flags
    let reply = conn
        .call_method(
            Some("org.freedesktop.resolve1"),
            "/org/freedesktop/resolve1",
            Some("org.freedesktop.resolve1.Manager"),
            "ResolveHostname",
            &(0i32, hostname, 0i32, 0u64),
        )
        .ok()?;

    // Parse the first address from the result
    type ResolvedAddresses = (Vec<(i32, i32, Vec<u8>)>, String, u64);
    let (addresses, _canonical, _flags): ResolvedAddresses =
        reply.body().deserialize().ok()?;

    if let Some((_ifindex, family, addr_bytes)) = addresses.first() {
        let ip = match *family {
            2 if addr_bytes.len() == 4 => {
                // AF_INET
                format!(
                    "{}.{}.{}.{}",
                    addr_bytes[0], addr_bytes[1], addr_bytes[2], addr_bytes[3]
                )
            }
            10 if addr_bytes.len() == 16 => {
                // AF_INET6
                let parts: Vec<String> = (0..8)
                    .map(|i| {
                        format!(
                            "{:x}",
                            u16::from_be_bytes([addr_bytes[i * 2], addr_bytes[i * 2 + 1]])
                        )
                    })
                    .collect();
                parts.join(":")
            }
            _ => return None,
        };
        Some(OperationResult::DnsResolved(ip))
    } else {
        Some(OperationResult::DnsFailed(format!(
            "no result for {}",
            hostname
        )))
    }
}

/// Get the link index for an interface via systemd-networkd dbus.
fn get_link_index_networkd(
    conn: &zbus::blocking::Connection,
    interface: &str,
) -> Result<u32, String> {
    // Try GetLinkByName first
    let reply = conn
        .call_method(
            Some("org.freedesktop.network1"),
            "/org/freedesktop/network1",
            Some("org.freedesktop.network1.Manager"),
            "GetLinkByName",
            &(interface,),
        )
        .map_err(|e| format!("GetLinkByName failed for {}: {}", interface, e))?;

    let (index, _path): (i32, zbus::zvariant::OwnedObjectPath) = reply
        .body()
        .deserialize()
        .map_err(|e| format!("failed to parse GetLinkByName result: {}", e))?;

    Ok(index as u32)
}

/// Get a string property from a networkd link via dbus.
fn get_networkd_property(
    conn: &zbus::blocking::Connection,
    link_path: &str,
    property: &str,
) -> Option<String> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.network1"),
            link_path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.network1.Link", property),
        )
        .ok()?;

    let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    let s: String = value.try_into().ok()?;
    Some(s)
}

/// Construct the wpa_supplicant dbus object path for an interface.
fn wpa_supplicant_iface_path(interface: &str) -> Result<ObjectPath<'static>, zbus::zvariant::Error> {
    let escaped: String = interface
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_string()
            } else {
                format!("_{:02x}", c as u32)
            }
        })
        .collect();
    let path = format!("/fi/w1/wpa_supplicant1/Interfaces/{}", escaped);
    ObjectPath::try_from(path).map(|p| p.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    // ---------------------------------------------------------------
    // DHCP polling tests
    // ---------------------------------------------------------------

    // Fast poll settings for unit tests
    const TEST_ATTEMPTS: u32 = 3;
    const TEST_INTERVAL: Duration = Duration::from_millis(1);

    fn trigger_success(_interface: &str) -> OperationResult {
        OperationResult::Success
    }

    fn trigger_error(_interface: &str) -> OperationResult {
        OperationResult::Error("trigger failed".into())
    }

    fn check_ip_always_assigned(_interface: &str) -> OperationResult {
        OperationResult::IpAssigned("10.0.0.1".into())
    }

    fn check_ip_always_none(_interface: &str) -> OperationResult {
        OperationResult::NoIp
    }

    static CALL_COUNT: AtomicU32 = AtomicU32::new(0);

    fn check_ip_on_third_call(_interface: &str) -> OperationResult {
        let count = CALL_COUNT.fetch_add(1, Ordering::SeqCst);
        if count >= 2 {
            OperationResult::IpAssigned("10.0.0.1".into())
        } else {
            OperationResult::NoIp
        }
    }

    #[test]
    fn test_dhcp_polling_immediate_ip() {
        let result = configure_dhcp_with(
            "eth0",
            trigger_success,
            check_ip_always_assigned,
            TEST_ATTEMPTS,
            TEST_INTERVAL,
        );
        assert!(
            result.is_success(),
            "expected Success, got {:?}",
            result
        );
    }

    #[test]
    fn test_dhcp_polling_ip_on_third_attempt() {
        CALL_COUNT.store(0, Ordering::SeqCst);
        let result = configure_dhcp_with(
            "eth0",
            trigger_success,
            check_ip_on_third_call,
            5,
            TEST_INTERVAL,
        );
        assert!(
            result.is_success(),
            "expected Success, got {:?}",
            result
        );
    }

    #[test]
    fn test_dhcp_polling_timeout() {
        let result = configure_dhcp_with(
            "eth0",
            trigger_success,
            check_ip_always_none,
            TEST_ATTEMPTS,
            TEST_INTERVAL,
        );
        match &result {
            OperationResult::Error(msg) => {
                assert!(
                    msg.contains("DHCP timeout"),
                    "expected timeout message, got: {}",
                    msg
                );
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_dhcp_trigger_failure_skips_polling() {
        // Use a static to track whether check_ip is ever called
        static CHECK_IP_CALLED: AtomicU32 = AtomicU32::new(0);

        fn check_ip_tracking(_interface: &str) -> OperationResult {
            CHECK_IP_CALLED.fetch_add(1, Ordering::SeqCst);
            OperationResult::NoIp
        }

        CHECK_IP_CALLED.store(0, Ordering::SeqCst);
        let result = configure_dhcp_with(
            "eth0",
            trigger_error,
            check_ip_tracking,
            TEST_ATTEMPTS,
            TEST_INTERVAL,
        );

        assert!(
            matches!(result, OperationResult::Error(_)),
            "expected Error, got {:?}",
            result
        );
        assert_eq!(
            CHECK_IP_CALLED.load(Ordering::SeqCst),
            0,
            "check_ip should not have been called when trigger fails"
        );
    }

    #[test]
    fn test_dhcp_polling_zero_attempts_is_timeout() {
        let result = configure_dhcp_with(
            "eth0",
            trigger_success,
            check_ip_always_assigned,
            0, // zero attempts — should never check
            TEST_INTERVAL,
        );
        match &result {
            OperationResult::Error(msg) => {
                assert!(msg.contains("DHCP timeout"), "got: {}", msg);
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    // ---------------------------------------------------------------
    // Networkd unit path tests
    // ---------------------------------------------------------------

    #[test]
    fn test_networkd_unit_path() {
        assert_eq!(
            networkd_unit_path("eth0"),
            "/etc/systemd/network/80-ttyforce-eth0.network"
        );
        assert_eq!(
            networkd_unit_path("wlan0"),
            "/etc/systemd/network/80-ttyforce-wlan0.network"
        );
    }

    // ---------------------------------------------------------------
    // DHCP config generation tests
    // ---------------------------------------------------------------

    #[test]
    fn test_generate_dhcp_config_content() {
        let config = generate_dhcp_network_config("eth0");
        assert!(config.contains("[Match]"), "missing [Match] section");
        assert!(config.contains("Name=eth0"), "missing Name=eth0");
        assert!(config.contains("[Network]"), "missing [Network] section");
        assert!(config.contains("DHCP=yes"), "missing DHCP=yes");
        assert!(config.contains("MulticastDNS=yes"), "missing MulticastDNS=yes");
    }

    #[test]
    fn test_generate_dhcp_config_interface_name_substitution() {
        let config = generate_dhcp_network_config("wlan0");
        assert!(config.contains("Name=wlan0"));
        assert!(!config.contains("Name=eth0"));
    }

    // ---------------------------------------------------------------
    // Primary interface config merging tests
    // ---------------------------------------------------------------

    #[test]
    fn test_merge_primary_empty_config() {
        let result = merge_primary_interface_config("eth0", "");
        assert!(result.contains("[Match]"), "missing [Match]");
        assert!(result.contains("Name=eth0"), "missing Name=eth0");
        assert!(result.contains("[Network]"), "missing [Network]");
        assert!(result.contains("DHCP=yes"), "missing DHCP=yes");
        assert!(result.contains("MulticastDNS=yes"), "missing MulticastDNS=yes");
        assert!(result.contains("[DHCPv4]"), "missing [DHCPv4]");
        assert!(result.contains("RouteMetric=100"), "missing RouteMetric=100");
    }

    #[test]
    fn test_merge_primary_existing_without_dhcpv4() {
        let existing = "[Match]\nName=eth0\n\n[Network]\nDHCP=yes\n";
        let result = merge_primary_interface_config("eth0", existing);
        assert!(result.contains("[DHCPv4]"), "missing [DHCPv4]");
        assert!(result.contains("RouteMetric=100"), "missing RouteMetric=100");
        // Should preserve existing content
        assert!(result.contains("[Match]"));
        assert!(result.contains("DHCP=yes"));
    }

    #[test]
    fn test_merge_primary_existing_dhcpv4_without_route_metric() {
        let existing = "[Match]\nName=eth0\n\n[Network]\nDHCP=yes\n\n[DHCPv4]\nUseDNS=true\n";
        let result = merge_primary_interface_config("eth0", existing);
        assert!(
            result.contains("[DHCPv4]\nRouteMetric=100"),
            "RouteMetric should be inserted right after [DHCPv4], got:\n{}",
            result
        );
        assert!(result.contains("UseDNS=true"), "should preserve existing keys");
    }

    #[test]
    fn test_merge_primary_existing_dhcpv4_with_route_metric() {
        let existing =
            "[Match]\nName=eth0\n\n[Network]\nDHCP=yes\n\n[DHCPv4]\nRouteMetric=500\n";
        let result = merge_primary_interface_config("eth0", existing);
        assert!(
            result.contains("RouteMetric=100"),
            "RouteMetric should be updated to 100"
        );
        assert!(
            !result.contains("RouteMetric=500"),
            "old RouteMetric=500 should be replaced"
        );
    }

    #[test]
    fn test_merge_primary_does_not_duplicate_dhcpv4_section() {
        let existing = "[Match]\nName=eth0\n\n[Network]\nDHCP=yes\n\n[DHCPv4]\nRouteMetric=200\n";
        let result = merge_primary_interface_config("eth0", existing);
        let count = result.matches("[DHCPv4]").count();
        assert_eq!(count, 1, "should have exactly one [DHCPv4] section, got {}", count);
    }

    #[test]
    fn test_merge_primary_preserves_match_section_for_correct_interface() {
        // When generating from scratch, should use the interface name provided
        let result = merge_primary_interface_config("wlan0", "");
        assert!(result.contains("Name=wlan0"));
        assert!(!result.contains("Name=eth0"));
    }
}
