pub mod network;
pub mod disk;

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
pub fn detect_hardware_initrd() -> anyhow::Result<HardwareManifest> {
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
