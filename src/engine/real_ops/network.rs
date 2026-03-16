use std::fs;

use zbus::zvariant::ObjectPath;

use crate::detect::network::{parse_iw_scan, parse_iwlist_scan};
use crate::engine::feedback::OperationResult;
use crate::network::wifi::WifiNetwork;

use super::run_cmd;

/// Enable a network interface.
/// Uses `ip link set up` (networkd doesn't expose a simple "bring up" via dbus).
pub fn enable_interface(interface: &str) -> OperationResult {
    match run_cmd("ip", &["link", "set", interface, "up"]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("failed to enable {}: {}", interface, e)),
    }
}

/// Disable a network interface.
pub fn disable_interface(interface: &str) -> OperationResult {
    match run_cmd("ip", &["link", "set", interface, "down"]) {
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

    // Kill any existing wpa_supplicant for this interface
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

/// Configure DHCP on an interface.
/// Tries systemd-networkd reconfigure via dbus, falls back to dhclient/dhcpcd.
pub fn configure_dhcp(interface: &str) -> OperationResult {
    if let Ok(conn) = zbus::blocking::Connection::system() {
        if let Ok(index) = get_link_index_networkd(&conn, interface) {
            let result = conn.call_method(
                Some("org.freedesktop.network1"),
                ObjectPath::try_from("/org/freedesktop/network1").unwrap(),
                Some("org.freedesktop.network1.Manager"),
                "ReconfigureLink",
                &(index as i32,),
            );
            if result.is_ok() {
                return OperationResult::Success;
            }
        }
    }

    // Fallback: try dhclient
    if run_cmd("dhclient", &[interface]).is_ok() {
        return OperationResult::Success;
    }

    // Fallback: try dhcpcd
    match run_cmd("dhcpcd", &[interface]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "DHCP configuration failed on {}: {}",
            interface, e
        )),
    }
}

/// Select an interface as primary by adjusting default route metric.
pub fn select_primary_interface(interface: &str) -> OperationResult {
    let _ = run_cmd("ip", &["route", "del", "default"]);
    match run_cmd(
        "ip",
        &[
            "route", "add", "default", "dev", interface, "metric", "100",
        ],
    ) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "failed to set primary interface {}: {}",
            interface, e
        )),
    }
}

/// Shut down a network interface.
pub fn shutdown_interface(interface: &str) -> OperationResult {
    match run_cmd("ip", &["link", "set", interface, "down"]) {
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
fn check_link_via_networkd(
    conn: &zbus::blocking::Connection,
    interface: &str,
) -> Option<OperationResult> {
    let index = get_link_index_networkd(conn, interface).ok()?;
    let link_path = format!("/org/freedesktop/network1/link/_{}", index);

    let carrier_state = get_networkd_property(conn, &link_path, "CarrierState")?;

    Some(match carrier_state.as_str() {
        "carrier" | "degraded-carrier" | "enslaved" => OperationResult::LinkUp,
        _ => OperationResult::LinkDown,
    })
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
        _ => Some(OperationResult::NoIp),
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
