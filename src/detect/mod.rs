pub mod disk;
pub mod network;

use crate::manifest::{HardwareManifest, NetworkManifest};

pub fn detect_hardware() -> anyhow::Result<HardwareManifest> {
    let interfaces = network::detect_interfaces()?;
    let wifi_environment = network::detect_wifi_environment(&interfaces);
    let disks = disk::detect_disks()?;

    Ok(HardwareManifest {
        network: NetworkManifest {
            interfaces,
            wifi_environment,
        },
        disks,
    })
}

/// Detect hardware using only sysfs — no dbus. For use in initrd where
/// systemd-networkd, udisks2, and dbus are not available.
///
/// Before scanning interfaces, attempts to ensure wifi hardware is available
/// by unblocking rfkill, loading common wifi kernel modules, and waiting
/// briefly for interfaces to appear.
pub fn detect_hardware_initrd() -> anyhow::Result<HardwareManifest> {
    prepare_wifi_hardware();

    let interfaces = network::detect_interfaces_sysfs()?;
    let wifi_environment = network::detect_wifi_environment(&interfaces);
    let disks = disk::detect_disks_sysfs()?;

    Ok(HardwareManifest {
        network: NetworkManifest {
            interfaces,
            wifi_environment,
        },
        disks,
    })
}

/// Best-effort preparation to make wifi hardware visible in the initrd.
///
/// In initrd environments, wifi interfaces may not appear in /sys/class/net
/// because:
/// 1. rfkill has wifi soft-blocked by default
/// 2. Wifi kernel modules aren't auto-loaded (no udev rules)
/// 3. Firmware loading may need a kick
///
/// This function tries to fix all of these before we scan for interfaces.
fn prepare_wifi_hardware() {
    use crate::engine::real_ops::{cmd_log_append, run_cmd};

    cmd_log_append("$ preparing wifi hardware for detection".to_string());

    // Step 1: Unblock rfkill (both soft and hard blocks where possible)
    if let Err(e) = run_cmd("rfkill", &["unblock", "wifi"]) {
        cmd_log_append(format!("  rfkill not available: {}", e));
    }

    // Step 2: Try loading common wifi kernel modules.
    // In initrd, udev may not be running, so modules that would normally
    // auto-load on PCI/USB enumeration may need manual loading.
    // We try each one silently — modprobe returns non-zero for missing
    // modules or modules that are already loaded (which is fine).
    let wifi_modules = [
        // Intel
        "iwlwifi",
        "iwlmvm",
        "iwldvm",
        // Broadcom
        "brcmfmac",
        "brcmsmac",
        "b43",
        "wl",
        // Realtek
        "rtw88_pci",
        "rtw89_pci",
        "rtl8xxxu",
        "r8188eu",
        "rtl8192ce",
        "rtl8192cu",
        "rtl8192de",
        "rtl8192se",
        "rtl8723be",
        "rtl8821ae",
        // Qualcomm/Atheros
        "ath9k",
        "ath10k_pci",
        "ath11k_pci",
        "ath12k",
        // MediaTek
        "mt7921e",
        "mt7921_common",
        "mt76x2e",
        // Ralink
        "rt2800pci",
        "rt2800usb",
        // Marvell
        "mwifiex_pcie",
        "mwifiex_sdio",
    ];

    let mut loaded = 0;
    for module in &wifi_modules {
        // Use modprobe -q (quiet) to avoid noise for missing modules
        if run_cmd("modprobe", &["-q", module]).is_ok() {
            loaded += 1;
        }
    }
    cmd_log_append(format!("  loaded {} wifi module(s)", loaded));

    // Step 3: Wait briefly for interfaces to appear after module loading.
    // Kernel module init + firmware loading is asynchronous.
    let has_wifi_before = has_wifi_interface();
    if !has_wifi_before {
        cmd_log_append("  waiting for wifi interfaces to appear...".to_string());
        for i in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if has_wifi_interface() {
                cmd_log_append(format!("  -> wifi interface appeared after {}ms", (i + 1) * 250));
                return;
            }
        }
        cmd_log_append("  -> no wifi interface after 5s".to_string());

        // Last resort: check dmesg for firmware issues
        if let Ok(output) = run_cmd("dmesg", &[]) {
            let fw_errors: Vec<&str> = output
                .lines()
                .filter(|l| {
                    let lower = l.to_lowercase();
                    lower.contains("firmware") && (lower.contains("fail") || lower.contains("error") || lower.contains("not found"))
                })
                .collect();
            if !fw_errors.is_empty() {
                cmd_log_append("  firmware issues detected:".to_string());
                for line in fw_errors.iter().take(5) {
                    cmd_log_append(format!("    {}", line.trim()));
                }
            }
        }
    } else {
        cmd_log_append("  wifi interface already present".to_string());
    }
}

/// Check if any wifi interface exists in /sys/class/net.
fn has_wifi_interface() -> bool {
    let net_dir = std::path::Path::new("/sys/class/net");
    if let Ok(entries) = std::fs::read_dir(net_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("wireless").exists() || path.join("phy80211").exists() {
                return true;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("wl") || name.starts_with("wlan") {
                return true;
            }
        }
    }
    false
}
