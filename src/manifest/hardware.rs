use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareManifest {
    pub network: NetworkManifest,
    pub disks: Vec<DiskSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkManifest {
    pub interfaces: Vec<NetworkInterfaceSpec>,
    #[serde(default)]
    pub wifi_environment: Option<WifiEnvironment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInterfaceSpec {
    pub name: String,
    pub kind: InterfaceKind,
    pub mac: String,
    #[serde(default)]
    pub has_link: bool,
    #[serde(default)]
    pub has_carrier: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceKind {
    Ethernet,
    Wifi,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiEnvironment {
    pub available_networks: Vec<WifiNetworkSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiNetworkSpec {
    pub ssid: String,
    pub signal_strength: i32,
    pub frequency_mhz: u32,
    #[serde(default)]
    pub security: WifiSecurity,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub qr_data: Option<String>,
    #[serde(default)]
    pub reachable: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WifiSecurity {
    Open,
    #[default]
    Wpa2,
    Wpa3,
    Wep,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskSpec {
    pub device: String,
    pub make: String,
    pub model: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub serial: Option<String>,
    /// How the disk is attached: "sata", "nvme", "usb", "virtio", "mmc", "ide", "xen", "unknown".
    /// Used for grouping disks by attachment type for RAID.
    #[serde(default = "default_transport")]
    pub transport: String,
}

fn default_transport() -> String {
    "unknown".to_string()
}

impl DiskSpec {
    pub fn size_human(&self) -> String {
        let gb = self.size_bytes as f64 / 1_073_741_824.0;
        if gb >= 1024.0 {
            format!("{:.1} TB", gb / 1024.0)
        } else {
            format!("{:.1} GB", gb)
        }
    }
}

impl HardwareManifest {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let manifest: Self = toml::from_str(&content)?;
        Ok(manifest)
    }

    pub fn ethernet_interfaces(&self) -> Vec<&NetworkInterfaceSpec> {
        self.network
            .interfaces
            .iter()
            .filter(|i| i.kind == InterfaceKind::Ethernet)
            .collect()
    }

    pub fn wifi_interfaces(&self) -> Vec<&NetworkInterfaceSpec> {
        self.network
            .interfaces
            .iter()
            .filter(|i| i.kind == InterfaceKind::Wifi)
            .collect()
    }

    pub fn connected_ethernet(&self) -> Vec<&NetworkInterfaceSpec> {
        self.ethernet_interfaces()
            .into_iter()
            .filter(|i| i.has_link && i.has_carrier)
            .collect()
    }
}
