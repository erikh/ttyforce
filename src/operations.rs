use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Operation {
    // Network operations
    EnableInterface {
        interface: String,
    },
    DisableInterface {
        interface: String,
    },
    ScanWifiNetworks {
        interface: String,
    },
    CheckLinkAvailability {
        interface: String,
    },
    AuthenticateWifi {
        interface: String,
        ssid: String,
        password: String,
    },
    ConfigureWifiQrCode {
        interface: String,
        qr_data: String,
    },
    ConfigureDhcp {
        interface: String,
    },
    CheckIpAddress {
        interface: String,
    },
    CheckUpstreamRouter {
        interface: String,
    },
    CheckInternetRoutability {
        interface: String,
    },
    CheckDnsResolution {
        interface: String,
        hostname: String,
    },
    SelectPrimaryInterface {
        interface: String,
    },
    ShutdownInterface {
        interface: String,
    },
    WifiConnectionTimeout {
        interface: String,
        ssid: String,
    },
    WifiAuthError {
        interface: String,
        ssid: String,
    },
    ConfigureWifiSsidAuth {
        interface: String,
        ssid: String,
        password: String,
    },
    ReceiveWifiScanResults {
        interface: String,
    },

    // WPS push-button connection
    WpsPbcStart {
        interface: String,
    },
    WpsPbcStatus {
        interface: String,
    },

    // Disk operations (common)
    PartitionDisk {
        device: String,
    },

    // Btrfs operations
    MkfsBtrfs {
        devices: Vec<String>,
    },
    CreateBtrfsSubvolume {
        mount_point: String,
        name: String,
    },
    BtrfsRaidSetup {
        devices: Vec<String>,
        raid_level: String,
    },
    MountFilesystem {
        device: String,
        mount_point: String,
        fs_type: String,
        #[serde(default)]
        options: Option<String>,
    },

    // System operations
    InstallBaseSystem {
        target: String,
    },
    Reboot,
    Exit,
    Abort {
        reason: String,
    },

    // Generate /etc/fstab in the installed system
    GenerateFstab {
        mount_point: String,
        device: String,
        fs_type: String,
    },

    // Persist network config to installed system
    PersistNetworkConfig {
        mount_point: String,
        interface: String,
        mac_address: String,
    },

    // Cleanup operations (emitted on abort to revert artifacts)
    CleanupNetworkConfig {
        interface: String,
    },
    CleanupWpaSupplicant {
        interface: String,
    },
    CleanupUnmount {
        mount_point: String,
    },

    // Getty operations
    PowerOff,
    StopAllContainers,
    WipeDisk {
        device: String,
    },
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Operation::EnableInterface { interface } => {
                write!(f, "Enable interface {}", interface)
            }
            Operation::DisableInterface { interface } => {
                write!(f, "Disable interface {}", interface)
            }
            Operation::ScanWifiNetworks { interface } => {
                write!(f, "Scan wifi networks on {}", interface)
            }
            Operation::CheckLinkAvailability { interface } => {
                write!(f, "Check link on {}", interface)
            }
            Operation::AuthenticateWifi {
                interface, ssid, ..
            } => write!(f, "Authenticate wifi {} on {}", ssid, interface),
            Operation::ConfigureWifiQrCode { interface, .. } => {
                write!(f, "Configure wifi via QR on {}", interface)
            }
            Operation::ConfigureDhcp { interface } => {
                write!(f, "Configure DHCP on {}", interface)
            }
            Operation::CheckIpAddress { interface } => {
                write!(f, "Check IP on {}", interface)
            }
            Operation::CheckUpstreamRouter { interface } => {
                write!(f, "Check upstream router on {}", interface)
            }
            Operation::CheckInternetRoutability { interface } => {
                write!(f, "Check internet on {}", interface)
            }
            Operation::CheckDnsResolution {
                interface,
                hostname,
            } => write!(f, "Resolve {} on {}", hostname, interface),
            Operation::SelectPrimaryInterface { interface } => {
                write!(f, "Select primary interface {}", interface)
            }
            Operation::ShutdownInterface { interface } => {
                write!(f, "Shutdown interface {}", interface)
            }
            Operation::WifiConnectionTimeout { interface, ssid } => {
                write!(f, "Wifi timeout {} on {}", ssid, interface)
            }
            Operation::WifiAuthError { interface, ssid } => {
                write!(f, "Wifi auth error {} on {}", ssid, interface)
            }
            Operation::ConfigureWifiSsidAuth {
                interface, ssid, ..
            } => write!(f, "Configure wifi auth {} on {}", ssid, interface),
            Operation::ReceiveWifiScanResults { interface } => {
                write!(f, "Receive wifi scan results on {}", interface)
            }
            Operation::WpsPbcStart { interface } => {
                write!(f, "WPS push-button start on {}", interface)
            }
            Operation::WpsPbcStatus { interface } => {
                write!(f, "WPS push-button status on {}", interface)
            }
            Operation::PartitionDisk { device } => write!(f, "Partition {}", device),
            Operation::MkfsBtrfs { devices } => {
                write!(f, "mkfs.btrfs on {}", devices.join(", "))
            }
            Operation::CreateBtrfsSubvolume { mount_point, name } => {
                write!(f, "Create btrfs subvolume {}@{}", name, mount_point)
            }
            Operation::BtrfsRaidSetup {
                devices,
                raid_level,
            } => write!(
                f,
                "Btrfs {} on {}",
                raid_level,
                devices.join(", ")
            ),
            Operation::MountFilesystem {
                device,
                mount_point,
                fs_type,
                options,
            } => {
                if let Some(opts) = options {
                    write!(f, "Mount {} ({}, {}) at {}", device, fs_type, opts, mount_point)
                } else {
                    write!(f, "Mount {} ({}) at {}", device, fs_type, mount_point)
                }
            }
            Operation::InstallBaseSystem { target } => {
                write!(f, "Install base system to {}", target)
            }
            Operation::Reboot => write!(f, "Reboot"),
            Operation::Exit => write!(f, "Exit"),
            Operation::Abort { reason } => write!(f, "Abort: {}", reason),
            Operation::GenerateFstab {
                mount_point,
                device,
                fs_type,
            } => write!(
                f,
                "Generate fstab: {} {} {}",
                device, mount_point, fs_type
            ),
            Operation::PersistNetworkConfig {
                mount_point,
                interface,
                ..
            } => write!(
                f,
                "Persist network config for {} to {}",
                interface, mount_point
            ),
            Operation::CleanupNetworkConfig { interface } => {
                write!(f, "Cleanup networkd config for {}", interface)
            }
            Operation::CleanupWpaSupplicant { interface } => {
                write!(f, "Cleanup wpa_supplicant for {}", interface)
            }
            Operation::CleanupUnmount { mount_point } => {
                write!(f, "Cleanup unmount {}", mount_point)
            }
            Operation::PowerOff => write!(f, "Power off"),
            Operation::StopAllContainers => write!(f, "Stop all containers"),
            Operation::WipeDisk { device } => write!(f, "Wipe disk {}", device),
        }
    }
}
