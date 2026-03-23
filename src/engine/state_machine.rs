use crate::disk::{DiskGroup, DiskInfo, FilesystemType, RaidConfig};
use crate::engine::executor::OperationExecutor;
use crate::engine::feedback::OperationResult;
use crate::manifest::{
    ActionManifest, HardwareManifest, InstallerFinalState, InterfaceKind, OperationOutcome,
};
use crate::network::interface::NetworkInterface;
use crate::network::state::NetworkState;
use crate::network::wifi::WifiNetwork;
use crate::operations::Operation;

#[derive(Debug, Clone, PartialEq)]
pub enum ScreenId {
    NetworkConfig,
    WifiSelect,
    WifiPassword,
    NetworkProgress,
    DiskGroupSelect,
    RaidConfig,
    Confirm,
    InstallProgress,
    Reboot,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum UserInput {
    // Navigation
    Confirm,
    Cancel,
    Back,
    Quit,

    // Selection (0-indexed)
    Select(usize),

    // Text input
    TextInput(String),

    // Wifi
    RefreshWifiScan,
    SelectWifiNetwork(usize),
    EnterWifiPassword(String),

    // Disk
    SelectDiskGroup(usize),
    SelectRaidOption(usize),

    // System
    ConfirmInstall,
    RebootSystem,
    ExitInstaller,
    AbortInstall,
}

pub struct InstallerStateMachine {
    pub current_screen: ScreenId,
    pub network_state: NetworkState,
    pub interfaces: Vec<NetworkInterface>,
    pub wifi_networks: Vec<WifiNetwork>,
    pub all_disks: Vec<DiskInfo>,
    pub disk_groups: Vec<DiskGroup>,
    pub selected_interface: Option<String>,
    pub selected_ssid: Option<String>,
    pub selected_disk_group: Option<usize>,
    pub selected_disk: Option<usize>,
    pub selected_filesystem: FilesystemType,
    pub selected_raid: Option<crate::disk::RaidConfig>,
    pub action_manifest: ActionManifest,
    pub hardware: HardwareManifest,
    pub error_message: Option<String>,
    pub mount_point: String,
    /// Target directory for /etc config files. If None, uses mount_point.
    pub etc_target: Option<String>,
    connectivity_retries: u32,
}

impl InstallerStateMachine {
    pub fn new(hardware: HardwareManifest) -> Self {
        let interfaces: Vec<NetworkInterface> = hardware
            .network
            .interfaces
            .iter()
            .map(NetworkInterface::from)
            .collect();

        let disks: Vec<DiskInfo> = hardware.disks.iter().map(DiskInfo::from).collect();
        let disk_groups = DiskGroup::from_disks(&disks);

        let wifi_networks: Vec<WifiNetwork> = hardware
            .network
            .wifi_environment
            .as_ref()
            .map(|env| env.available_networks.iter().map(WifiNetwork::from).collect())
            .unwrap_or_default();

        Self {
            current_screen: ScreenId::NetworkConfig,
            network_state: NetworkState::Offline,
            interfaces,
            wifi_networks,
            all_disks: disks,
            disk_groups,
            selected_interface: None,
            selected_ssid: None,
            selected_disk_group: None,
            selected_disk: None,
            selected_filesystem: FilesystemType::default(),
            selected_raid: None,
            action_manifest: ActionManifest::new(),
            hardware,
            error_message: None,
            mount_point: "/town-os".to_string(),
            etc_target: None,
            connectivity_retries: 0,
        }
    }

    pub fn with_mount_point(mut self, mp: String) -> Self {
        self.mount_point = mp;
        self
    }

    /// The target directory for /etc config files.
    /// Defaults to mount_point if not explicitly set.
    pub fn etc_target(&self) -> &str {
        self.etc_target.as_deref().unwrap_or(&self.mount_point)
    }

    pub fn process_input(
        &mut self,
        input: UserInput,
        executor: &mut dyn OperationExecutor,
    ) -> Option<ScreenId> {
        self.error_message = None;

        match (&self.current_screen, input) {
            // === Network Config Screen ===
            (ScreenId::NetworkConfig, UserInput::Confirm) => {
                self.auto_detect_network(executor)
            }
            (ScreenId::NetworkConfig, UserInput::Select(idx)) => {
                self.select_interface(idx, executor)
            }
            (ScreenId::NetworkConfig, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at network config".to_string())
            }

            // === Wifi Select Screen ===
            (ScreenId::WifiSelect, UserInput::SelectWifiNetwork(idx)) => {
                self.select_wifi_network(idx)
            }
            (ScreenId::WifiSelect, UserInput::RefreshWifiScan) => {
                self.refresh_wifi_scan(executor)
            }
            (ScreenId::WifiSelect, UserInput::Back) => {
                self.current_screen = ScreenId::NetworkConfig;
                Some(ScreenId::NetworkConfig)
            }
            (ScreenId::WifiSelect, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at wifi select".to_string())
            }

            // === Wifi Password Screen ===
            (ScreenId::WifiPassword, UserInput::EnterWifiPassword(password)) => {
                self.connect_wifi(password, executor)
            }
            (ScreenId::WifiPassword, UserInput::Back) => {
                self.current_screen = ScreenId::WifiSelect;
                Some(ScreenId::WifiSelect)
            }
            (ScreenId::WifiPassword, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at wifi password".to_string())
            }

            // === Network Progress Screen ===
            (ScreenId::NetworkProgress, UserInput::Confirm) => {
                if self.network_state.is_online() {
                    self.current_screen = ScreenId::RaidConfig;
                    Some(ScreenId::RaidConfig)
                } else {
                    self.error_message = Some("Network is not yet online".to_string());
                    None
                }
            }
            (ScreenId::NetworkProgress, UserInput::Back) => {
                self.current_screen = ScreenId::NetworkConfig;
                self.network_state = NetworkState::Offline;
                Some(ScreenId::NetworkConfig)
            }
            (ScreenId::NetworkProgress, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at network progress".to_string())
            }

            // === RAID Config ===
            (ScreenId::RaidConfig, UserInput::SelectRaidOption(idx)) => {
                self.select_raid_option(idx)
            }
            (ScreenId::RaidConfig, UserInput::Back) => {
                self.current_screen = ScreenId::NetworkProgress;
                Some(ScreenId::NetworkProgress)
            }
            (ScreenId::RaidConfig, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at RAID config".to_string())
            }

            // === Disk Group Select (now after RAID) ===
            (ScreenId::DiskGroupSelect, UserInput::SelectDiskGroup(idx)) => {
                if self.is_single_disk_mode() {
                    // Single mode: selecting individual disk
                    if idx < self.all_disks.len() {
                        self.selected_disk = Some(idx);
                        self.selected_disk_group = None;
                        self.current_screen = ScreenId::Confirm;
                        Some(ScreenId::Confirm)
                    } else {
                        self.error_message = Some("Invalid disk selection".to_string());
                        None
                    }
                } else {
                    let compatible = self.compatible_disk_groups();
                    if idx < compatible.len() {
                        self.selected_disk_group = Some(compatible[idx]);
                        self.selected_disk = None;
                        self.current_screen = ScreenId::Confirm;
                        Some(ScreenId::Confirm)
                    } else {
                        self.error_message = Some("Invalid disk group selection".to_string());
                        None
                    }
                }
            }
            (ScreenId::DiskGroupSelect, UserInput::Back) => {
                self.current_screen = ScreenId::RaidConfig;
                Some(ScreenId::RaidConfig)
            }
            (ScreenId::DiskGroupSelect, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at disk selection".to_string())
            }

            // === Confirm ===
            (ScreenId::Confirm, UserInput::ConfirmInstall) => {
                self.run_install(executor)
            }
            (ScreenId::Confirm, UserInput::Back) => {
                self.current_screen = ScreenId::DiskGroupSelect;
                Some(ScreenId::DiskGroupSelect)
            }
            (ScreenId::Confirm, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at confirmation".to_string())
            }

            // === Install Progress ===
            (ScreenId::InstallProgress, UserInput::Confirm) => {
                self.current_screen = ScreenId::Reboot;
                Some(ScreenId::Reboot)
            }
            (ScreenId::InstallProgress, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at install progress".to_string())
            }

            // === Reboot ===
            (ScreenId::Reboot, UserInput::RebootSystem) => {
                self.action_manifest
                    .record(Operation::Reboot, OperationOutcome::Success);
                self.action_manifest.final_state = InstallerFinalState::Rebooted;
                let result = executor.execute(&Operation::Reboot);
                let _ = result;
                None
            }
            (ScreenId::Reboot, UserInput::ExitInstaller) => {
                self.action_manifest
                    .record(Operation::Exit, OperationOutcome::Success);
                self.action_manifest.final_state = InstallerFinalState::Exited;
                let result = executor.execute(&Operation::Exit);
                let _ = result;
                None
            }
            (ScreenId::Reboot, UserInput::AbortInstall) => {
                self.abort(executor, "User chose not to reboot".to_string())
            }

            // Global quit
            (_, UserInput::Quit) => {
                self.abort(executor, "User quit".to_string())
            }

            _ => None,
        }
    }

    fn auto_detect_network(&mut self, executor: &mut dyn OperationExecutor) -> Option<ScreenId> {
        // Priority: connected ethernet first, then wifi
        let connected_eth: Vec<String> = self
            .interfaces
            .iter()
            .filter(|i| i.kind == InterfaceKind::Ethernet && i.has_link && i.has_carrier)
            .map(|i| i.name.clone())
            .collect();

        if let Some(eth_name) = connected_eth.first() {
            self.selected_interface = Some(eth_name.clone());
            self.bring_ethernet_online(eth_name.clone(), executor)
        } else {
            // Check for wifi interfaces
            let has_wifi = self
                .interfaces
                .iter()
                .any(|i| i.kind == InterfaceKind::Wifi);
            if has_wifi {
                let wifi_name = self
                    .interfaces
                    .iter()
                    .find(|i| i.kind == InterfaceKind::Wifi)
                    .unwrap()
                    .name
                    .clone();
                self.selected_interface = Some(wifi_name.clone());

                // Enable and scan
                let enable_result = executor.execute(&Operation::EnableInterface {
                    interface: wifi_name.clone(),
                });
                self.action_manifest.record(
                    Operation::EnableInterface {
                        interface: wifi_name.clone(),
                    },
                    enable_result.to_outcome(),
                );
                self.network_state = NetworkState::DeviceEnabled;

                let scan_result = executor.execute(&Operation::ScanWifiNetworks {
                    interface: wifi_name.clone(),
                });
                self.action_manifest.record(
                    Operation::ScanWifiNetworks {
                        interface: wifi_name.clone(),
                    },
                    scan_result.to_outcome(),
                );

                if let OperationResult::WifiScanResults(networks) = &scan_result {
                    self.wifi_networks = networks.clone();
                }

                let recv_result = executor.execute(&Operation::ReceiveWifiScanResults {
                    interface: wifi_name.clone(),
                });
                self.action_manifest.record(
                    Operation::ReceiveWifiScanResults {
                        interface: wifi_name,
                    },
                    recv_result.to_outcome(),
                );

                self.network_state = NetworkState::Scanning;
                self.current_screen = ScreenId::WifiSelect;
                Some(ScreenId::WifiSelect)
            } else {
                // No interfaces at all - check for ethernet without link
                let has_any_eth = self
                    .interfaces
                    .iter()
                    .any(|i| i.kind == InterfaceKind::Ethernet);
                if has_any_eth {
                    // Try the first ethernet anyway
                    let eth_name = self
                        .interfaces
                        .iter()
                        .find(|i| i.kind == InterfaceKind::Ethernet)
                        .unwrap()
                        .name
                        .clone();
                    self.selected_interface = Some(eth_name.clone());
                    self.bring_ethernet_online(eth_name, executor)
                } else {
                    self.error_message = Some("No network interfaces available".to_string());
                    None
                }
            }
        }
    }

    fn select_interface(
        &mut self,
        idx: usize,
        executor: &mut dyn OperationExecutor,
    ) -> Option<ScreenId> {
        if idx >= self.interfaces.len() {
            self.error_message = Some("Invalid interface selection".to_string());
            return None;
        }

        let iface = self.interfaces[idx].clone();
        self.selected_interface = Some(iface.name.clone());

        match iface.kind {
            InterfaceKind::Ethernet => self.bring_ethernet_online(iface.name, executor),
            InterfaceKind::Wifi => {
                let enable_result = executor.execute(&Operation::EnableInterface {
                    interface: iface.name.clone(),
                });
                self.action_manifest.record(
                    Operation::EnableInterface {
                        interface: iface.name.clone(),
                    },
                    enable_result.to_outcome(),
                );
                self.network_state = NetworkState::DeviceEnabled;

                let scan_result = executor.execute(&Operation::ScanWifiNetworks {
                    interface: iface.name.clone(),
                });
                self.action_manifest.record(
                    Operation::ScanWifiNetworks {
                        interface: iface.name.clone(),
                    },
                    scan_result.to_outcome(),
                );

                if let OperationResult::WifiScanResults(networks) = &scan_result {
                    self.wifi_networks = networks.clone();
                }

                let recv_result = executor.execute(&Operation::ReceiveWifiScanResults {
                    interface: iface.name.clone(),
                });
                self.action_manifest.record(
                    Operation::ReceiveWifiScanResults {
                        interface: iface.name,
                    },
                    recv_result.to_outcome(),
                );

                self.network_state = NetworkState::Scanning;
                self.current_screen = ScreenId::WifiSelect;
                Some(ScreenId::WifiSelect)
            }
        }
    }

    fn bring_ethernet_online(
        &mut self,
        iface_name: String,
        _executor: &mut dyn OperationExecutor,
    ) -> Option<ScreenId> {
        // Immediately show the progress screen. All steps (enable, link check,
        // DHCP, connectivity) are driven by advance_connectivity() from the
        // TUI loop so the user sees real-time progress.
        let already_connected = self
            .interfaces
            .iter()
            .any(|i| i.name == iface_name && i.has_link && i.has_carrier);

        self.connectivity_retries = 0;
        if already_connected {
            // Skip enable/link — go straight to checking IP
            self.network_state = NetworkState::DeviceEnabled;
        } else {
            self.network_state = NetworkState::Offline;
        }
        self.current_screen = ScreenId::NetworkProgress;
        Some(ScreenId::NetworkProgress)
    }

    fn shutdown_other_interfaces(&self, primary: &str) -> Vec<Operation> {
        self.interfaces
            .iter()
            .filter(|i| i.name != primary && i.enabled)
            .map(|i| Operation::ShutdownInterface {
                interface: i.name.clone(),
            })
            .collect()
    }

    fn select_wifi_network(&mut self, idx: usize) -> Option<ScreenId> {
        if idx >= self.wifi_networks.len() {
            self.error_message = Some("Invalid network selection".to_string());
            return None;
        }

        let network = &self.wifi_networks[idx];
        self.selected_ssid = Some(network.ssid.clone());
        self.network_state = NetworkState::NetworkSelected;

        if network.security == crate::manifest::WifiSecurity::Open {
            // No password needed for open networks
            self.current_screen = ScreenId::WifiPassword;
        } else {
            self.current_screen = ScreenId::WifiPassword;
        }
        Some(ScreenId::WifiPassword)
    }

    fn refresh_wifi_scan(&mut self, executor: &mut dyn OperationExecutor) -> Option<ScreenId> {
        let iface_name = match &self.selected_interface {
            Some(name) => name.clone(),
            None => return None,
        };

        let scan_result = executor.execute(&Operation::ScanWifiNetworks {
            interface: iface_name.clone(),
        });
        self.action_manifest.record(
            Operation::ScanWifiNetworks {
                interface: iface_name.clone(),
            },
            scan_result.to_outcome(),
        );

        if let OperationResult::WifiScanResults(networks) = &scan_result {
            self.wifi_networks = networks.clone();
        }

        let recv_result = executor.execute(&Operation::ReceiveWifiScanResults {
            interface: iface_name.clone(),
        });
        self.action_manifest.record(
            Operation::ReceiveWifiScanResults {
                interface: iface_name,
            },
            recv_result.to_outcome(),
        );

        Some(ScreenId::WifiSelect)
    }

    fn connect_wifi(
        &mut self,
        password: String,
        executor: &mut dyn OperationExecutor,
    ) -> Option<ScreenId> {
        let iface_name = match &self.selected_interface {
            Some(name) => name.clone(),
            None => return None,
        };
        let ssid = match &self.selected_ssid {
            Some(s) => s.clone(),
            None => return None,
        };

        // Configure wifi auth
        let config_result = executor.execute(&Operation::ConfigureWifiSsidAuth {
            interface: iface_name.clone(),
            ssid: ssid.clone(),
            password: password.clone(),
        });
        self.action_manifest.record(
            Operation::ConfigureWifiSsidAuth {
                interface: iface_name.clone(),
                ssid: ssid.clone(),
                password: password.clone(),
            },
            config_result.to_outcome(),
        );

        // Authenticate
        let auth_result = executor.execute(&Operation::AuthenticateWifi {
            interface: iface_name.clone(),
            ssid: ssid.clone(),
            password: password.clone(),
        });
        self.action_manifest.record(
            Operation::AuthenticateWifi {
                interface: iface_name.clone(),
                ssid: ssid.clone(),
                password: password.clone(),
            },
            auth_result.to_outcome(),
        );

        match &auth_result {
            OperationResult::WifiAuthFailed(msg) => {
                self.action_manifest.record(
                    Operation::WifiAuthError {
                        interface: iface_name.clone(),
                        ssid: ssid.clone(),
                    },
                    OperationOutcome::Error(msg.clone()),
                );
                self.error_message = Some(format!("Authentication failed: {}", msg));
                self.network_state = NetworkState::NetworkSelected;
                self.current_screen = ScreenId::WifiPassword;
                return Some(ScreenId::WifiPassword);
            }
            OperationResult::WifiTimeout => {
                self.action_manifest.record(
                    Operation::WifiConnectionTimeout {
                        interface: iface_name.clone(),
                        ssid: ssid.clone(),
                    },
                    OperationOutcome::Timeout,
                );
                self.error_message = Some("Connection timed out".to_string());
                self.network_state = NetworkState::NetworkSelected;
                self.current_screen = ScreenId::WifiSelect;
                return Some(ScreenId::WifiSelect);
            }
            _ => {}
        }

        // All remaining steps (DHCP, IP, connectivity) are driven by
        // advance_connectivity() from the TUI loop.
        self.connectivity_retries = 0;
        self.network_state = NetworkState::Connected;
        self.current_screen = ScreenId::NetworkProgress;
        Some(ScreenId::NetworkProgress)
    }

    fn select_raid_option(&mut self, idx: usize) -> Option<ScreenId> {
        let max_disks = self.max_disk_count();
        let options = RaidConfig::for_disk_count(max_disks);

        if idx >= options.len() {
            self.error_message = Some("Invalid RAID option".to_string());
            return None;
        }

        self.selected_raid = Some(options[idx].clone());
        self.current_screen = ScreenId::DiskGroupSelect;
        Some(ScreenId::DiskGroupSelect)
    }

    pub fn max_disk_count(&self) -> usize {
        self.disk_groups
            .iter()
            .map(|g| g.disk_count())
            .max()
            .unwrap_or(0)
    }

    pub fn min_disks_for_raid(&self) -> usize {
        match &self.selected_raid {
            Some(RaidConfig::Single) => 1,
            Some(RaidConfig::BtrfsRaid1) => 2,
            Some(RaidConfig::BtrfsRaid5) => 3,
            None => 1,
        }
    }

    pub fn is_single_disk_mode(&self) -> bool {
        matches!(&self.selected_raid, Some(RaidConfig::Single))
    }

    pub fn compatible_disk_groups(&self) -> Vec<usize> {
        let min_disks = self.min_disks_for_raid();
        self.disk_groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.disk_count() >= min_disks)
            .map(|(i, _)| i)
            .collect()
    }

    fn run_install(&mut self, executor: &mut dyn OperationExecutor) -> Option<ScreenId> {
        let raid = self.selected_raid.clone()?;

        let devices = if self.is_single_disk_mode() {
            let disk_idx = self.selected_disk?;
            vec![self.all_disks[disk_idx].device.clone()]
        } else {
            let group_idx = self.selected_disk_group?;
            let group = self.disk_groups[group_idx].clone();
            group.device_paths()
        };

        // Partition all disks
        for device in &devices {
            let op = Operation::PartitionDisk {
                device: device.clone(),
            };
            let result = executor.execute(&op);
            self.action_manifest.record(op, result.to_outcome());
        }

        // Filesystem operations (always Btrfs)
        match &raid {
            RaidConfig::Single => {
                let op = Operation::MkfsBtrfs {
                    devices: devices.clone(),
                };
                let result = executor.execute(&op);
                self.action_manifest.record(op, result.to_outcome());
            }
            RaidConfig::BtrfsRaid1 | RaidConfig::BtrfsRaid5 => {
                let op = Operation::BtrfsRaidSetup {
                    devices: devices.clone(),
                    raid_level: raid.raid_level().to_string(),
                };
                let result = executor.execute(&op);
                self.action_manifest.record(op, result.to_outcome());
            }
        }

        // Mount the new filesystem
        let mount_device = super::real_ops::disk::partition_path(&devices[0]);
        let mount_op = Operation::MountFilesystem {
            device: mount_device,
            mount_point: self.mount_point.clone(),
            fs_type: "btrfs".to_string(),
        };
        let mount_result = executor.execute(&mount_op);
        self.action_manifest
            .record(mount_op, mount_result.to_outcome());
        if mount_result.is_error() {
            self.action_manifest.final_state =
                InstallerFinalState::Error(format!("Mount failed: {:?}", mount_result));
            self.error_message = Some("Failed to mount filesystem".to_string());
            self.current_screen = ScreenId::InstallProgress;
            return Some(ScreenId::InstallProgress);
        }

        // Create subvolumes
        for name in &["@", "@home", "@snapshots"] {
            let op = Operation::CreateBtrfsSubvolume {
                mount_point: self.mount_point.clone(),
                name: name.to_string(),
            };
            let result = executor.execute(&op);
            self.action_manifest.record(op, result.to_outcome());
        }

        // Install base system
        let op = Operation::InstallBaseSystem {
            target: self.mount_point.clone(),
        };
        let result = executor.execute(&op);
        self.action_manifest.record(op, result.to_outcome());

        if result.is_error() {
            self.action_manifest.final_state =
                InstallerFinalState::Error(format!("Install failed: {:?}", result));
            self.error_message = Some("Installation failed".to_string());
        } else {
            // Generate mount service so the installed system mounts at boot
            let etc = self.etc_target().to_string();
            let fstab_device = super::real_ops::disk::partition_path(&devices[0]);
            let fstab_op = Operation::GenerateFstab {
                mount_point: etc.clone(),
                device: fstab_device,
                fs_type: "btrfs".to_string(),
            };
            let fstab_result = executor.execute(&fstab_op);
            self.action_manifest
                .record(fstab_op, fstab_result.to_outcome());

            // Persist network configuration to the installed system
            if let Some(ref iface_name) = self.selected_interface {
                let mac = self
                    .interfaces
                    .iter()
                    .find(|i| &i.name == iface_name)
                    .map(|i| i.mac.clone())
                    .unwrap_or_default();
                let op = Operation::PersistNetworkConfig {
                    mount_point: etc.clone(),
                    interface: iface_name.clone(),
                    mac_address: mac,
                };
                let persist_result = executor.execute(&op);
                self.action_manifest
                    .record(op, persist_result.to_outcome());
            }

            // Unmount the volume so systemd doesn't see it in /proc/mounts
            // and try to auto-generate an invalid town-os.mount unit
            let unmount_op = Operation::CleanupUnmount {
                mount_point: self.mount_point.clone(),
            };
            let unmount_result = executor.execute(&unmount_op);
            self.action_manifest
                .record(unmount_op, unmount_result.to_outcome());

            self.action_manifest.final_state = InstallerFinalState::Installed;
        }

        self.current_screen = ScreenId::InstallProgress;
        Some(ScreenId::InstallProgress)
    }

    fn abort(
        &mut self,
        executor: &mut dyn OperationExecutor,
        reason: String,
    ) -> Option<ScreenId> {
        self.cleanup(executor);

        let op = Operation::Abort {
            reason: reason.clone(),
        };
        let result = executor.execute(&op);
        self.action_manifest.record(op, result.to_outcome());
        self.action_manifest.final_state = InstallerFinalState::Aborted;
        self.current_screen = ScreenId::Reboot;
        Some(ScreenId::Reboot)
    }

    fn cleanup(&mut self, executor: &mut dyn OperationExecutor) {
        use std::collections::BTreeSet;

        let mut networkd_interfaces: BTreeSet<String> = BTreeSet::new();
        let mut wpa_interfaces: BTreeSet<String> = BTreeSet::new();
        let mut needs_unmount = false;

        for entry in &self.action_manifest.operations {
            match &entry.operation {
                Operation::ConfigureDhcp { interface }
                | Operation::SelectPrimaryInterface { interface } => {
                    networkd_interfaces.insert(interface.clone());
                }
                Operation::AuthenticateWifi { interface, .. }
                | Operation::ConfigureWifiSsidAuth { interface, .. }
                | Operation::ConfigureWifiQrCode { interface, .. } => {
                    wpa_interfaces.insert(interface.clone());
                }
                Operation::MkfsBtrfs { .. }
                | Operation::BtrfsRaidSetup { .. }
                | Operation::CreateBtrfsSubvolume { .. }
                | Operation::InstallBaseSystem { .. } => {
                    needs_unmount = true;
                }
                _ => {}
            }
        }

        // Unmount first
        if needs_unmount {
            let op = Operation::CleanupUnmount {
                mount_point: self.mount_point.clone(),
            };
            let result = executor.execute(&op);
            self.action_manifest.record(op, result.to_outcome());
        }

        // Kill wpa_supplicant processes
        for interface in &wpa_interfaces {
            let op = Operation::CleanupWpaSupplicant {
                interface: interface.clone(),
            };
            let result = executor.execute(&op);
            self.action_manifest.record(op, result.to_outcome());
        }

        // Remove networkd configs
        for interface in &networkd_interfaces {
            let op = Operation::CleanupNetworkConfig {
                interface: interface.clone(),
            };
            let result = executor.execute(&op);
            self.action_manifest.record(op, result.to_outcome());
        }
    }

    /// Advance one step in the network bring-up / connectivity sequence.
    /// Called from the TUI loop on each tick when on NetworkProgress screen.
    /// Returns true if a step was executed (state changed).
    pub fn advance_connectivity(&mut self, executor: &mut dyn OperationExecutor) -> bool {
        let iface_name = match &self.selected_interface {
            Some(name) => name.clone(),
            None => return false,
        };

        const MAX_RETRIES: u32 = 10;
        const DNS_MAX_RETRIES: u32 = 120; // ~60 seconds at 500ms per tick

        match &self.network_state {
            // Step 1: Enable the interface
            NetworkState::Offline => {
                let op = Operation::EnableInterface {
                    interface: iface_name.clone(),
                };
                let result = executor.execute(&op);
                self.action_manifest.record(op, result.to_outcome());
                if result.is_error() {
                    self.network_state =
                        NetworkState::Error(format!("Failed to enable {}", iface_name));
                    self.error_message = Some(format!("Failed to enable {}", iface_name));
                } else {
                    self.network_state = NetworkState::DeviceEnabled;
                    self.connectivity_retries = 0;
                }
                true
            }

            // Step 2: Check link availability
            NetworkState::DeviceEnabled => {
                let op = Operation::CheckLinkAvailability {
                    interface: iface_name.clone(),
                };
                let result = executor.execute(&op);
                self.action_manifest.record(op, result.to_outcome());
                if result.is_error() {
                    self.connectivity_retries += 1;
                    if self.connectivity_retries >= MAX_RETRIES {
                        self.network_state =
                            NetworkState::Error(format!("No link on {}", iface_name));
                        self.error_message = Some(format!("No link on {}", iface_name));
                    }
                } else {
                    self.network_state = NetworkState::DhcpConfiguring;
                    self.connectivity_retries = 0;
                }
                true
            }

            // Step 2b: Wifi connected — proceed to DHCP
            NetworkState::Connected => {
                self.network_state = NetworkState::DhcpConfiguring;
                self.connectivity_retries = 0;
                true
            }

            // Step 3: Configure DHCP
            NetworkState::DhcpConfiguring => {
                // Check if we already have an IP
                let ip_op = Operation::CheckIpAddress {
                    interface: iface_name.clone(),
                };
                let ip_result = executor.execute(&ip_op);
                self.action_manifest.record(ip_op, ip_result.to_outcome());

                if let OperationResult::IpAssigned(ip) = &ip_result {
                    if let Some(iface) =
                        self.interfaces.iter_mut().find(|i| i.name == iface_name)
                    {
                        iface.ip_address = Some(ip.clone());
                    }
                    self.network_state = NetworkState::IpAssigned;
                    self.connectivity_retries = 0;
                } else {
                    // No IP — run DHCP
                    let dhcp_op = Operation::ConfigureDhcp {
                        interface: iface_name.clone(),
                    };
                    let dhcp_result = executor.execute(&dhcp_op);
                    self.action_manifest
                        .record(dhcp_op, dhcp_result.to_outcome());
                    if dhcp_result.is_error() {
                        self.network_state =
                            NetworkState::Error(format!("DHCP failed on {}", iface_name));
                        self.error_message = Some("DHCP configuration failed".to_string());
                    } else {
                        // Re-check IP after DHCP
                        let ip_op2 = Operation::CheckIpAddress {
                            interface: iface_name.clone(),
                        };
                        let ip_result2 = executor.execute(&ip_op2);
                        self.action_manifest
                            .record(ip_op2, ip_result2.to_outcome());
                        if let OperationResult::IpAssigned(ip) = &ip_result2 {
                            if let Some(iface) =
                                self.interfaces.iter_mut().find(|i| i.name == iface_name)
                            {
                                iface.ip_address = Some(ip.clone());
                            }
                        }
                        self.network_state = NetworkState::IpAssigned;
                        self.connectivity_retries = 0;
                    }
                }
                true
            }

            // Step 4: Check upstream router
            NetworkState::IpAssigned => {
                let op = Operation::CheckUpstreamRouter {
                    interface: iface_name,
                };
                let result = executor.execute(&op);
                self.action_manifest.record(op, result.to_outcome());
                if result.is_error() {
                    self.connectivity_retries += 1;
                    if self.connectivity_retries >= MAX_RETRIES {
                        self.network_state =
                            NetworkState::Error("No upstream router found".to_string());
                        self.error_message = Some("No upstream router found".to_string());
                    }
                } else {
                    self.network_state = NetworkState::CheckingRouter;
                    self.connectivity_retries = 0;
                }
                true
            }

            // Step 5: Check internet routability
            NetworkState::CheckingRouter => {
                let op = Operation::CheckInternetRoutability {
                    interface: iface_name,
                };
                let result = executor.execute(&op);
                self.action_manifest.record(op, result.to_outcome());
                if result.is_error() {
                    self.connectivity_retries += 1;
                    if self.connectivity_retries >= MAX_RETRIES {
                        self.network_state =
                            NetworkState::Error("Internet not reachable".to_string());
                        self.error_message = Some("Internet not reachable".to_string());
                    }
                } else {
                    self.network_state = NetworkState::CheckingInternet;
                    self.connectivity_retries = 0;
                }
                true
            }

            // Step 6: Check DNS — retries for up to 60 seconds
            NetworkState::CheckingInternet => {
                let op = Operation::CheckDnsResolution {
                    interface: iface_name,
                    hostname: "example.com".to_string(),
                };
                let result = executor.execute(&op);
                self.action_manifest.record(op, result.to_outcome());
                if result.is_error() {
                    self.connectivity_retries += 1;
                    if self.connectivity_retries >= DNS_MAX_RETRIES {
                        self.network_state =
                            NetworkState::Error("DNS resolution failed".to_string());
                        self.error_message =
                            Some("DNS resolution failed".to_string());
                    }
                } else {
                    self.network_state = NetworkState::CheckingDns;
                    self.connectivity_retries = 0;
                }
                true
            }

            // Step 7: Select primary interface and go online
            NetworkState::CheckingDns => {
                // Select primary and go online
                let op = Operation::SelectPrimaryInterface {
                    interface: iface_name.clone(),
                };
                let result = executor.execute(&op);
                self.action_manifest.record(op, result.to_outcome());

                // Shutdown non-primary interfaces
                for sop in self.shutdown_other_interfaces(&iface_name) {
                    let sr = executor.execute(&sop);
                    self.action_manifest.record(sop, sr.to_outcome());
                }

                self.network_state = NetworkState::Online;
                true
            }
            _ => false, // Terminal or pre-IP states — don't advance
        }
    }

    pub fn connect_wifi_qr(
        &mut self,
        qr_data: String,
        executor: &mut dyn OperationExecutor,
    ) -> Option<ScreenId> {
        let iface_name = match &self.selected_interface {
            Some(name) => name.clone(),
            None => return None,
        };

        let op = Operation::ConfigureWifiQrCode {
            interface: iface_name.clone(),
            qr_data,
        };
        let result = executor.execute(&op);
        self.action_manifest.record(op, result.to_outcome());

        if result.is_error() {
            self.error_message = Some("QR code configuration failed".to_string());
            self.current_screen = ScreenId::WifiSelect;
            return Some(ScreenId::WifiSelect);
        }

        self.network_state = NetworkState::Connected;

        // Do connectivity checks
        let remaining_ops = [
            Operation::ConfigureDhcp {
                interface: iface_name.clone(),
            },
            Operation::CheckIpAddress {
                interface: iface_name.clone(),
            },
            Operation::CheckUpstreamRouter {
                interface: iface_name.clone(),
            },
            Operation::CheckInternetRoutability {
                interface: iface_name.clone(),
            },
            Operation::CheckDnsResolution {
                interface: iface_name.clone(),
                hostname: "example.com".to_string(),
            },
            Operation::SelectPrimaryInterface {
                interface: iface_name.clone(),
            },
        ];

        let states = [
            NetworkState::DhcpConfiguring,
            NetworkState::IpAssigned,
            NetworkState::CheckingRouter,
            NetworkState::CheckingInternet,
            NetworkState::CheckingDns,
            NetworkState::Online,
        ];

        for (op, state) in remaining_ops.iter().zip(states.iter()) {
            let result = executor.execute(op);
            self.action_manifest.record(op.clone(), result.to_outcome());

            if result.is_error() {
                self.network_state = NetworkState::Error(format!("{:?}", result));
                self.error_message = Some(format!("Network error: {:?}", result));
                self.current_screen = ScreenId::NetworkProgress;
                return Some(ScreenId::NetworkProgress);
            }

            self.network_state = state.clone();
        }

        self.current_screen = ScreenId::NetworkProgress;
        Some(ScreenId::NetworkProgress)
    }
}
