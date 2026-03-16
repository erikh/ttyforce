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
