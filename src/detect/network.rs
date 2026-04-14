use std::fs;
use std::path::Path;
use std::process::Command;

use crate::engine::real_ops::cmd_log_append;
use crate::manifest::{
    InterfaceKind, NetworkInterfaceSpec, WifiEnvironment, WifiNetworkSpec, WifiSecurity,
};

pub fn detect_interfaces() -> anyhow::Result<Vec<NetworkInterfaceSpec>> {
    cmd_log_append("$ detect network interfaces (networkd dbus → sysfs fallback)".to_string());
    // Try systemd-networkd dbus first
    match detect_interfaces_networkd() {
        Some(interfaces) if !interfaces.is_empty() => {
            cmd_log_append(format!(
                "  -> networkd returned {} interface(s)",
                interfaces.len()
            ));
            return Ok(interfaces);
        }
        Some(_) => cmd_log_append(
            "  -> networkd returned no interfaces; falling back to sysfs".to_string(),
        ),
        None => cmd_log_append(
            "  -> networkd unavailable; falling back to sysfs".to_string(),
        ),
    }

    detect_interfaces_sysfs()
}

/// Detect network interfaces via systemd-networkd dbus (org.freedesktop.network1).
fn detect_interfaces_networkd() -> Option<Vec<NetworkInterfaceSpec>> {
    cmd_log_append("$ networkd ListLinks (org.freedesktop.network1)".to_string());
    let conn = zbus::blocking::Connection::system().ok()?;

    // ListLinks returns a(iso) — array of (ifindex, name, object_path)
    let reply = conn
        .call_method(
            Some("org.freedesktop.network1"),
            "/org/freedesktop/network1",
            Some("org.freedesktop.network1.Manager"),
            "ListLinks",
            &(),
        )
        .ok()?;

    let links: Vec<(i32, String, zbus::zvariant::OwnedObjectPath)> =
        reply.body().deserialize().ok()?;

    cmd_log_append(format!("  -> networkd reports {} link(s)", links.len()));

    let mut interfaces = Vec::new();

    for (index, name, _path) in &links {
        cmd_log_append(format!("  inspect {} (ifindex={})", name, index));
        // Skip loopback and virtual interfaces
        if should_skip_interface(name) {
            cmd_log_append(format!("    skip {} (lo/veth/docker/bridge)", name));
            continue;
        }

        // Construct the link dbus path: /org/freedesktop/network1/link/_<ifindex>
        let link_path = format!("/org/freedesktop/network1/link/_{}", index);

        // Get operational and carrier state from networkd
        let oper_state =
            get_networkd_link_property(&conn, &link_path, "OperationalState").unwrap_or_default();
        let carrier_state =
            get_networkd_link_property(&conn, &link_path, "CarrierState").unwrap_or_default();

        // Interface type detection: networkd doesn't expose this directly,
        // so we still check sysfs/naming conventions
        let iface_sysfs = Path::new("/sys/class/net").join(name);
        let kind = detect_interface_kind(&iface_sysfs, name);

        let mac = read_sysfs_trimmed(&iface_sysfs.join("address"))
            .unwrap_or_else(|| "00:00:00:00:00:00".to_string());

        let has_carrier = carrier_state == "carrier";
        let has_link = matches!(
            oper_state.as_str(),
            "carrier" | "routable" | "degraded" | "enslaved"
        ) || has_carrier;

        cmd_log_append(format!(
            "    accept {} kind={:?} mac={} carrier_state={} oper_state={}",
            name, kind, mac, carrier_state, oper_state
        ));

        interfaces.push(NetworkInterfaceSpec {
            name: name.clone(),
            kind,
            mac,
            has_link,
            has_carrier,
        });
    }

    // Sort: ethernet first, then wifi, alphabetical within each group
    sort_interfaces(&mut interfaces);
    cmd_log_append(format!(
        "  -> networkd scan accepted {} interface(s)",
        interfaces.len()
    ));
    Some(interfaces)
}

/// Detect interfaces via sysfs only (no dbus).
pub fn detect_interfaces_sysfs() -> anyhow::Result<Vec<NetworkInterfaceSpec>> {
    let mut interfaces = Vec::new();
    let net_dir = Path::new("/sys/class/net");

    if !net_dir.exists() {
        cmd_log_append("  /sys/class/net does not exist".to_string());
        return Ok(interfaces);
    }

    cmd_log_append("$ scan /sys/class/net for interfaces".to_string());

    let entries: Vec<_> = fs::read_dir(net_dir)?.collect::<Result<_, _>>()?;
    cmd_log_append(format!(
        "  -> {} entry/entries in /sys/class/net",
        entries.len()
    ));

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        cmd_log_append(format!("  inspect {}", name));

        if should_skip_interface(&name) {
            cmd_log_append(format!("    skip {} (lo/veth/docker/bridge)", name));
            continue;
        }

        let iface_path = entry.path();

        let kind = detect_interface_kind(&iface_path, &name);
        let mac = read_sysfs_trimmed(&iface_path.join("address"))
            .unwrap_or_else(|| "00:00:00:00:00:00".to_string());
        let has_carrier = read_sysfs_trimmed(&iface_path.join("carrier"))
            .map(|v| v == "1")
            .unwrap_or(false);
        let operstate = read_sysfs_trimmed(&iface_path.join("operstate")).unwrap_or_default();
        let has_link = has_carrier || operstate == "up" || operstate == "dormant";

        cmd_log_append(format!(
            "    accept {} kind={:?} mac={} carrier={} operstate={}",
            name, kind, mac, has_carrier, operstate
        ));

        interfaces.push(NetworkInterfaceSpec {
            name,
            kind,
            mac,
            has_link,
            has_carrier,
        });
    }

    sort_interfaces(&mut interfaces);
    cmd_log_append(format!(
        "  -> sysfs scan accepted {} interface(s)",
        interfaces.len()
    ));
    Ok(interfaces)
}

/// Get a string property from a networkd link via dbus.
fn get_networkd_link_property(
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
    // The variant wraps a string
    let s: String = value.try_into().ok()?;
    Some(s)
}

fn should_skip_interface(name: &str) -> bool {
    name == "lo"
        || name.starts_with("veth")
        || name.starts_with("docker")
        || name.starts_with("br-")
        || name.starts_with("virbr")
}

fn sort_interfaces(interfaces: &mut [NetworkInterfaceSpec]) {
    interfaces.sort_by(|a, b| {
        let kind_ord = |k: &InterfaceKind| match k {
            InterfaceKind::Ethernet => 0,
            InterfaceKind::Wifi => 1,
        };
        kind_ord(&a.kind)
            .cmp(&kind_ord(&b.kind))
            .then(a.name.cmp(&b.name))
    });
}

fn detect_interface_kind(iface_path: &Path, name: &str) -> InterfaceKind {
    // Check if wireless directory exists under the interface
    if iface_path.join("wireless").exists() || iface_path.join("phy80211").exists() {
        return InterfaceKind::Wifi;
    }

    // Check device type: 1 = ethernet (ARPHRD_ETHER) is common for both,
    // so wireless dir is the primary signal
    if name.starts_with("wl") || name.starts_with("wlan") {
        return InterfaceKind::Wifi;
    }

    // Check /sys/class/net/<name>/type - type 801 is wifi on some systems
    if let Some(type_val) = read_sysfs_trimmed(&iface_path.join("type")) {
        if type_val == "801" {
            return InterfaceKind::Wifi;
        }
    }

    InterfaceKind::Ethernet
}

pub fn detect_wifi_environment(
    interfaces: &[NetworkInterfaceSpec],
) -> Option<WifiEnvironment> {
    let has_wifi = interfaces.iter().any(|i| i.kind == InterfaceKind::Wifi);
    if !has_wifi {
        cmd_log_append("  -> no wifi interface present, skipping scan".to_string());
        return None;
    }

    let wifi_iface = interfaces
        .iter()
        .find(|i| i.kind == InterfaceKind::Wifi)?;

    cmd_log_append(format!("$ scan wifi networks on {}", wifi_iface.name));
    let networks = scan_wifi_networks(&wifi_iface.name).unwrap_or_default();
    cmd_log_append(format!(
        "  -> {} network(s) found on {}",
        networks.len(),
        wifi_iface.name
    ));
    if networks.is_empty() {
        // Return an empty environment so the UI still shows wifi is available
        Some(WifiEnvironment {
            available_networks: Vec::new(),
        })
    } else {
        Some(WifiEnvironment {
            available_networks: networks,
        })
    }
}

fn scan_wifi_networks(interface: &str) -> anyhow::Result<Vec<WifiNetworkSpec>> {
    // Try wpa_supplicant dbus first
    if let Some(networks) = scan_wifi_wpa_supplicant_dbus(interface) {
        if !networks.is_empty() {
            return Ok(networks);
        }
    }

    // Fallback: try iw
    let output = Command::new("iw")
        .args(["dev", interface, "scan", "-u"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Ok(parse_iw_scan(&stdout));
        }
    }

    // Fallback: try iwlist
    let output = Command::new("iwlist")
        .args([interface, "scan"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Ok(parse_iwlist_scan(&stdout));
        }
    }

    Ok(Vec::new())
}

/// Scan wifi networks via wpa_supplicant dbus interface.
fn scan_wifi_wpa_supplicant_dbus(interface: &str) -> Option<Vec<WifiNetworkSpec>> {
    let conn = zbus::blocking::Connection::system().ok()?;

    let iface_path = wpa_supplicant_iface_path(interface).ok()?;

    // Trigger scan
    let scan_result = conn.call_method(
        Some("fi.w1.wpa_supplicant1"),
        &iface_path,
        Some("fi.w1.wpa_supplicant1.Interface"),
        "Scan",
        &std::collections::HashMap::<String, zbus::zvariant::Value<'_>>::new(),
    );

    if scan_result.is_err() {
        return None;
    }

    // Wait for scan to complete
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Get BSS list
    let bss_reply = conn.call_method(
        Some("fi.w1.wpa_supplicant1"),
        &iface_path,
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("fi.w1.wpa_supplicant1.Interface", "BSSs"),
    );

    let bss_paths: Vec<zbus::zvariant::OwnedObjectPath> = match bss_reply {
        Ok(reply) => {
            let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
            value.try_into().ok()?
        }
        Err(_) => return None,
    };

    let mut networks = Vec::new();

    for bss_path in &bss_paths {
        if let Some(network) = parse_bss_object(&conn, bss_path.as_str()) {
            networks.push(network);
        }
    }

    // Sort by signal strength (strongest first)
    networks.sort_by(|a, b| b.signal_strength.cmp(&a.signal_strength));
    Some(networks)
}

/// Parse a single BSS dbus object into a WifiNetworkSpec.
fn parse_bss_object(
    conn: &zbus::blocking::Connection,
    bss_path: &str,
) -> Option<WifiNetworkSpec> {
    let get_prop = |prop: &str| -> Option<zbus::zvariant::OwnedValue> {
        let reply = conn
            .call_method(
                Some("fi.w1.wpa_supplicant1"),
                bss_path,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("fi.w1.wpa_supplicant1.BSS", prop),
            )
            .ok()?;
        reply.body().deserialize().ok()
    };

    // SSID is ay (byte array)
    let ssid_value = get_prop("SSID")?;
    let ssid_bytes: Vec<u8> = ssid_value.try_into().ok()?;
    let ssid = String::from_utf8(ssid_bytes).ok()?;

    if ssid.is_empty() {
        return None;
    }

    // Signal is i16 (dBm * 100 on some, or just dBm)
    let signal: i16 = get_prop("Signal")?.try_into().ok()?;

    // Frequency is u16 (MHz)
    let frequency: u16 = get_prop("Frequency")?.try_into().ok()?;

    // Security: check RSN and WPA properties
    let security = determine_bss_security(conn, bss_path);

    Some(WifiNetworkSpec {
        ssid,
        signal_strength: signal as i32,
        frequency_mhz: frequency as u32,
        security,
        password: None,
        qr_data: None,
        reachable: true,
    })
}

/// Determine security type from BSS dbus properties.
fn determine_bss_security(
    conn: &zbus::blocking::Connection,
    bss_path: &str,
) -> WifiSecurity {
    // Check if RSN (WPA2/WPA3) is present
    let has_rsn = conn
        .call_method(
            Some("fi.w1.wpa_supplicant1"),
            bss_path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("fi.w1.wpa_supplicant1.BSS", "RSN"),
        )
        .is_ok();

    let has_wpa = conn
        .call_method(
            Some("fi.w1.wpa_supplicant1"),
            bss_path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("fi.w1.wpa_supplicant1.BSS", "WPA"),
        )
        .is_ok();

    if has_rsn {
        // Could be WPA2 or WPA3; default to WPA2 since WPA3 detection
        // requires checking key_mgmt for SAE
        WifiSecurity::Wpa2
    } else if has_wpa {
        WifiSecurity::Wpa2
    } else {
        WifiSecurity::Open
    }
}

/// Construct the wpa_supplicant dbus object path for an interface.
fn wpa_supplicant_iface_path(
    interface: &str,
) -> Result<zbus::zvariant::ObjectPath<'static>, zbus::zvariant::Error> {
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
    zbus::zvariant::ObjectPath::try_from(path).map(|p| p.into_owned())
}

pub fn parse_iw_scan(output: &str) -> Vec<WifiNetworkSpec> {
    let mut networks = Vec::new();
    let mut current_ssid: Option<String> = None;
    let mut current_signal: i32 = -100;
    let mut current_freq: u32 = 0;
    let mut current_security = WifiSecurity::Open;

    for line in output.lines() {
        let line = line.trim();

        if line.starts_with("BSS ") {
            // Flush previous entry
            if let Some(ssid) = current_ssid.take() {
                if !ssid.is_empty() {
                    networks.push(WifiNetworkSpec {
                        ssid,
                        signal_strength: current_signal,
                        frequency_mhz: current_freq,
                        security: current_security.clone(),
                        password: None,
                        qr_data: None,
                        reachable: true,
                    });
                }
            }
            current_signal = -100;
            current_freq = 0;
            current_security = WifiSecurity::Open;
        } else if line.starts_with("SSID: ") {
            current_ssid = Some(line.trim_start_matches("SSID: ").to_string());
        } else if line.starts_with("signal: ") {
            // e.g. "signal: -65.00 dBm"
            if let Some(val) = line
                .trim_start_matches("signal: ")
                .split_whitespace()
                .next()
            {
                current_signal = val.parse::<f64>().unwrap_or(-100.0) as i32;
            }
        } else if line.starts_with("freq: ") {
            if let Ok(f) = line.trim_start_matches("freq: ").parse::<u32>() {
                current_freq = f;
            }
        } else if line.contains("WPA") || line.contains("RSN") {
            if line.contains("SAE") || line.contains("WPA3") {
                current_security = WifiSecurity::Wpa3;
            } else {
                current_security = WifiSecurity::Wpa2;
            }
        } else if line.contains("WEP") {
            current_security = WifiSecurity::Wep;
        }
    }

    // Flush last entry
    if let Some(ssid) = current_ssid {
        if !ssid.is_empty() {
            networks.push(WifiNetworkSpec {
                ssid,
                signal_strength: current_signal,
                frequency_mhz: current_freq,
                security: current_security,
                password: None,
                qr_data: None,
                reachable: true,
            });
        }
    }

    // Sort by signal strength (strongest first)
    networks.sort_by(|a, b| b.signal_strength.cmp(&a.signal_strength));
    networks
}

pub fn parse_iwlist_scan(output: &str) -> Vec<WifiNetworkSpec> {
    let mut networks = Vec::new();
    let mut current_ssid: Option<String> = None;
    let mut current_signal: i32 = -100;
    let mut current_freq: u32 = 0;
    let mut current_security = WifiSecurity::Open;

    for line in output.lines() {
        let line = line.trim();

        if line.starts_with("Cell ") {
            if let Some(ssid) = current_ssid.take() {
                if !ssid.is_empty() {
                    networks.push(WifiNetworkSpec {
                        ssid,
                        signal_strength: current_signal,
                        frequency_mhz: current_freq,
                        security: current_security.clone(),
                        password: None,
                        qr_data: None,
                        reachable: true,
                    });
                }
            }
            current_signal = -100;
            current_freq = 0;
            current_security = WifiSecurity::Open;
        } else if line.starts_with("ESSID:") {
            current_ssid = Some(
                line.trim_start_matches("ESSID:")
                    .trim_matches('"')
                    .to_string(),
            );
        } else if line.starts_with("Frequency:") {
            // e.g. "Frequency:2.437 GHz"
            if let Some(val) = line.split(':').nth(1) {
                if let Some(ghz_str) = val.split_whitespace().next() {
                    if let Ok(ghz) = ghz_str.parse::<f64>() {
                        current_freq = (ghz * 1000.0) as u32;
                    }
                }
            }
        } else if line.contains("Signal level=") || line.contains("Signal level:") {
            let sep = if line.contains("Signal level=") {
                "Signal level="
            } else {
                "Signal level:"
            };
            if let Some(after) = line.split(sep).nth(1) {
                if let Some(val) = after.split_whitespace().next() {
                    current_signal = val.parse::<i32>().unwrap_or(-100);
                }
            }
        } else if line.contains("WPA2") || line.contains("WPA Version 2") {
            current_security = WifiSecurity::Wpa2;
        } else if line.contains("WPA") {
            if current_security == WifiSecurity::Open {
                current_security = WifiSecurity::Wpa2;
            }
        } else if line.contains("WEP") {
            current_security = WifiSecurity::Wep;
        }
    }

    if let Some(ssid) = current_ssid {
        if !ssid.is_empty() {
            networks.push(WifiNetworkSpec {
                ssid,
                signal_strength: current_signal,
                frequency_mhz: current_freq,
                security: current_security,
                password: None,
                qr_data: None,
                reachable: true,
            });
        }
    }

    networks.sort_by(|a, b| b.signal_strength.cmp(&a.signal_strength));
    networks
}

fn read_sysfs_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_iw_scan_output() {
        let output = r#"BSS aa:bb:cc:dd:ee:ff(on wlan0)
	TSF: 12345 usec
	freq: 5180
	signal: -45.00 dBm
	SSID: HomeNetwork
	RSN:	 * Version: 1
BSS 11:22:33:44:55:66(on wlan0)
	freq: 2437
	signal: -72.00 dBm
	SSID: Neighbor
	WEP:
"#;
        let networks = parse_iw_scan(output);
        assert_eq!(networks.len(), 2);
        assert_eq!(networks[0].ssid, "HomeNetwork");
        assert_eq!(networks[0].signal_strength, -45);
        assert_eq!(networks[0].frequency_mhz, 5180);
        assert_eq!(networks[0].security, WifiSecurity::Wpa2);
        assert_eq!(networks[1].ssid, "Neighbor");
        assert_eq!(networks[1].security, WifiSecurity::Wep);
    }

    #[test]
    fn test_parse_iw_scan_empty() {
        let networks = parse_iw_scan("");
        assert!(networks.is_empty());
    }

    #[test]
    fn test_parse_iw_scan_hidden_ssid() {
        let output = r#"BSS aa:bb:cc:dd:ee:ff(on wlan0)
	freq: 5180
	signal: -50.00 dBm
	SSID:
BSS 11:22:33:44:55:66(on wlan0)
	freq: 2437
	signal: -60.00 dBm
	SSID: Visible
"#;
        let networks = parse_iw_scan(output);
        assert_eq!(networks.len(), 1);
        assert_eq!(networks[0].ssid, "Visible");
    }

    #[test]
    fn test_parse_iwlist_scan_output() {
        let output = r#"wlan0     Scan completed :
          Cell 01 - Address: AA:BB:CC:DD:EE:FF
                    ESSID:"TestNetwork"
                    Frequency:5.18 GHz
                    Signal level=-52 dBm
                    IE: IEEE 802.11i/WPA2 Version 1
          Cell 02 - Address: 11:22:33:44:55:66
                    ESSID:"OpenNet"
                    Frequency:2.437 GHz
                    Signal level=-70 dBm
"#;
        let networks = parse_iwlist_scan(output);
        assert_eq!(networks.len(), 2);
        assert_eq!(networks[0].ssid, "TestNetwork");
        assert_eq!(networks[0].signal_strength, -52);
        assert_eq!(networks[0].security, WifiSecurity::Wpa2);
        assert_eq!(networks[1].ssid, "OpenNet");
        assert_eq!(networks[1].security, WifiSecurity::Open);
    }

    #[test]
    fn test_should_skip_interface() {
        assert!(should_skip_interface("lo"));
        assert!(should_skip_interface("veth1234"));
        assert!(should_skip_interface("docker0"));
        assert!(should_skip_interface("br-abc123"));
        assert!(should_skip_interface("virbr0"));
        assert!(!should_skip_interface("eth0"));
        assert!(!should_skip_interface("wlan0"));
        assert!(!should_skip_interface("enp3s0"));
    }
}
