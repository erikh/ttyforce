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
    InstallModeSelect,
    NetworkConfig,
    WifiSelect,
    WifiPassword,
    WpsPrompt,
    WpsWaiting,
    NetworkProgress,
    WifiQrDisplay,
    DiskGroupSelect,
    RaidConfig,
    Confirm,
    SshKeyImport,
    InstallProgress,
    Reboot,
}

/// High-level installation style chosen at the start of the installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InstallMode {
    /// Auto-select wired network (if carrier present) and most-redundant
    /// RAID layout from the largest same-make/model disk group. Falls back
    /// to manual interface selection when no wired carrier is detected.
    /// Wifi is never auto-selected.
    Easy,
    /// Full manual flow: pick network interface, RAID level, and disk group.
    Advanced,
}

impl InstallMode {
    pub fn display_name(&self) -> &'static str {
        match self {
            InstallMode::Easy => "Easy",
            InstallMode::Advanced => "Advanced",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            InstallMode::Easy => {
                "Detect a wired connection and pick the most redundant disk layout. \
                 Falls back to asking when there's no cable plugged in. Wifi is never auto-selected."
            }
            InstallMode::Advanced => {
                "Choose network interface, RAID level, and disk group manually."
            }
        }
    }
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

    // Install mode
    SelectInstallMode(InstallMode),

    // Text input
    TextInput(String),

    // Wifi
    RefreshWifiScan,
    SelectWifiNetwork(usize),
    EnterWifiPassword(String),
    InitiateWps,
    WpsAccept,
    WpsDecline,
    ShowWifiQr,

    // Disk
    SelectDiskGroup(usize),
    SelectRaidOption(usize),

    // SSH keys
    ImportSshKeys(String),
    SkipSshKeys,

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
    pub wifi_password: Option<String>,
    pub selected_disk_group: Option<usize>,
    pub selected_disk: Option<usize>,
    pub selected_filesystem: FilesystemType,
    pub selected_raid: Option<crate::disk::RaidConfig>,
    pub action_manifest: ActionManifest,
    pub hardware: HardwareManifest,
    pub error_message: Option<String>,
    pub mount_point: String,
    /// Target directory for /etc config files. If None, uses mount_point.
    pub etc_prefix: Option<String>,
    connectivity_retries: u32,
    pub wps_start_time: Option<std::time::Instant>,
    pub ssh_users: Vec<String>,
    pub ssh_keys: std::collections::BTreeMap<String, Vec<String>>,
    pub ssh_current_user_idx: usize,
    /// When true, exit after network setup (skip disk/install screens).
    pub network_only: bool,
    /// Installation style chosen at the start of the flow. Defaults to Advanced
    /// for direct constructor callers; `new_with_mode_select` starts on the
    /// InstallModeSelect screen and resets this to whichever mode the user
    /// picks.
    pub install_mode: InstallMode,
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
            wifi_password: None,
            selected_disk_group: None,
            selected_disk: None,
            selected_filesystem: FilesystemType::default(),
            selected_raid: None,
            action_manifest: ActionManifest::new(),
            hardware,
            error_message: None,
            mount_point: "/town-os".to_string(),
            etc_prefix: None,
            connectivity_retries: 0,
            wps_start_time: None,
            ssh_users: Vec::new(),
            ssh_keys: std::collections::BTreeMap::new(),
            ssh_current_user_idx: 0,
            network_only: false,
            install_mode: InstallMode::Advanced,
        }
    }

    /// Construct a state machine that starts on the install-mode-select screen.
    /// This is the real installer entry point; `new` skips it for tests and
    /// reconfigure flows that don't need to prompt for a style.
    pub fn new_with_mode_select(hardware: HardwareManifest) -> Self {
        let mut sm = Self::new(hardware);
        sm.current_screen = ScreenId::InstallModeSelect;
        sm
    }

    pub fn with_mount_point(mut self, mp: String) -> Self {
        self.mount_point = mp;
        self
    }

    /// The target directory that corresponds to /etc on the installed system.
    /// Defaults to `<mount_point>/@etc` (the Town OS @etc subvolume).
    /// When set via `--etc-prefix`, uses that path directly.
    /// Files are written directly under this path (e.g., `<etc_prefix>/systemd/network/`).
    pub fn etc_prefix(&self) -> String {
        match &self.etc_prefix {
            Some(prefix) => prefix.clone(),
            None => format!("{}/@etc", self.mount_point),
        }
    }

    /// Persist network config only (for network-only reconfigure mode).
    /// Writes the networkd unit and wpa config to etc_prefix.
    pub fn persist_network_only(&mut self, executor: &mut dyn OperationExecutor) {
        let etc = self.etc_prefix();
        if let Some(ref iface_name) = self.selected_interface {
            let mac = self
                .interfaces
                .iter()
                .find(|i| &i.name == iface_name)
                .map(|i| i.mac.clone())
                .unwrap_or_default();
            let op = Operation::PersistNetworkConfig {
                mount_point: etc,
                interface: iface_name.clone(),
                mac_address: mac,
            };
            let persist_result = executor.execute(&op);
            self.action_manifest
                .record(op, persist_result.to_outcome());
        }
    }

    pub fn process_input(
        &mut self,
        input: UserInput,
        executor: &mut dyn OperationExecutor,
    ) -> Option<ScreenId> {
        self.error_message = None;

        match (&self.current_screen, input) {
            // === Install Mode Select Screen ===
            (ScreenId::InstallModeSelect, UserInput::SelectInstallMode(mode)) => {
                self.install_mode = mode;
                match mode {
                    InstallMode::Easy => self.start_easy_mode(executor),
                    InstallMode::Advanced => {
                        self.current_screen = ScreenId::NetworkConfig;
                        Some(ScreenId::NetworkConfig)
                    }
                }
            }
            (ScreenId::InstallModeSelect, UserInput::Select(idx)) => {
                let mode = match idx {
                    0 => InstallMode::Easy,
                    1 => InstallMode::Advanced,
                    _ => {
                        self.error_message = Some("Invalid install mode selection".to_string());
                        return None;
                    }
                };
                self.install_mode = mode;
                match mode {
                    InstallMode::Easy => self.start_easy_mode(executor),
                    InstallMode::Advanced => {
                        self.current_screen = ScreenId::NetworkConfig;
                        Some(ScreenId::NetworkConfig)
                    }
                }
            }
            (ScreenId::InstallModeSelect, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at install mode select".to_string())
            }

            // === Network Config Screen ===
            (ScreenId::NetworkConfig, UserInput::Confirm) => {
                self.auto_detect_network(executor)
            }
            (ScreenId::NetworkConfig, UserInput::Select(idx)) => {
                self.select_interface(idx, executor)
            }
            (ScreenId::NetworkConfig, UserInput::Back) => {
                // Only meaningful when reached via the install-mode-select
                // screen — otherwise there's nothing to go back to.
                self.current_screen = ScreenId::InstallModeSelect;
                Some(ScreenId::InstallModeSelect)
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
            (ScreenId::WifiSelect, UserInput::InitiateWps) => {
                self.start_wps(executor)
            }
            (ScreenId::WifiSelect, UserInput::Back) => {
                self.current_screen = ScreenId::NetworkConfig;
                Some(ScreenId::NetworkConfig)
            }
            (ScreenId::WifiSelect, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at wifi select".to_string())
            }

            // === WPS Prompt Screen ===
            (ScreenId::WpsPrompt, UserInput::WpsAccept) => {
                self.start_wps(executor)
            }
            (ScreenId::WpsPrompt, UserInput::WpsDecline) => {
                self.current_screen = ScreenId::WifiPassword;
                Some(ScreenId::WifiPassword)
            }
            (ScreenId::WpsPrompt, UserInput::Back) => {
                self.current_screen = ScreenId::WifiSelect;
                Some(ScreenId::WifiSelect)
            }
            (ScreenId::WpsPrompt, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at WPS prompt".to_string())
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

            // === WPS Waiting Screen ===
            (ScreenId::WpsWaiting, UserInput::Back) => {
                // Cancel WPS — clean up wpa_supplicant
                if let Some(ref iface) = self.selected_interface {
                    let op = Operation::CleanupWpaSupplicant {
                        interface: iface.clone(),
                    };
                    let result = executor.execute(&op);
                    self.action_manifest.record(op, result.to_outcome());
                }
                self.wps_start_time = None;
                self.network_state = NetworkState::Scanning;
                self.current_screen = ScreenId::WifiSelect;
                Some(ScreenId::WifiSelect)
            }
            (ScreenId::WpsWaiting, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted during WPS".to_string())
            }

            // === Network Progress Screen ===
            (ScreenId::NetworkProgress, UserInput::Confirm) => {
                if self.network_state.is_online() {
                    if self.network_only {
                        self.persist_network_only(executor);
                        // Signal completion via ExitInstaller
                        self.current_screen = ScreenId::Reboot;
                        Some(ScreenId::Reboot)
                    } else if self.install_mode == InstallMode::Easy {
                        // Easy mode: auto-pick raid + disk group and jump
                        // straight to confirm.
                        if self.apply_easy_disk_defaults() {
                            self.current_screen = ScreenId::Confirm;
                            Some(ScreenId::Confirm)
                        } else {
                            // No usable disks — fall back to manual flow
                            self.current_screen = ScreenId::RaidConfig;
                            Some(ScreenId::RaidConfig)
                        }
                    } else {
                        self.current_screen = ScreenId::RaidConfig;
                        Some(ScreenId::RaidConfig)
                    }
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
            (ScreenId::NetworkProgress, UserInput::ShowWifiQr) => {
                if self.network_state.is_online() && self.selected_ssid.is_some() {
                    self.current_screen = ScreenId::WifiQrDisplay;
                    Some(ScreenId::WifiQrDisplay)
                } else {
                    None
                }
            }
            (ScreenId::NetworkProgress, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at network progress".to_string())
            }

            // === WiFi QR Display ===
            (ScreenId::WifiQrDisplay, UserInput::Back) => {
                self.current_screen = ScreenId::NetworkProgress;
                Some(ScreenId::NetworkProgress)
            }
            (ScreenId::WifiQrDisplay, UserInput::Confirm) => {
                self.current_screen = ScreenId::NetworkProgress;
                Some(ScreenId::NetworkProgress)
            }
            (ScreenId::WifiQrDisplay, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at WiFi QR display".to_string())
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
                if self.ssh_users.is_empty() {
                    self.run_install(executor)
                } else {
                    self.ssh_current_user_idx = 0;
                    self.current_screen = ScreenId::SshKeyImport;
                    Some(ScreenId::SshKeyImport)
                }
            }
            (ScreenId::Confirm, UserInput::Back) => {
                if self.install_mode == InstallMode::Easy {
                    // In Easy mode the user never saw the RAID/disk screens,
                    // so going back drops them on the mode-select screen.
                    self.selected_raid = None;
                    self.selected_disk_group = None;
                    self.selected_disk = None;
                    self.current_screen = ScreenId::InstallModeSelect;
                    Some(ScreenId::InstallModeSelect)
                } else {
                    self.current_screen = ScreenId::DiskGroupSelect;
                    Some(ScreenId::DiskGroupSelect)
                }
            }
            (ScreenId::Confirm, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at confirmation".to_string())
            }

            // === SSH Key Import ===
            (ScreenId::SshKeyImport, UserInput::ImportSshKeys(username)) => {
                if let Some(current_user) = self.ssh_users.get(self.ssh_current_user_idx).cloned() {
                    self.ssh_keys.entry(current_user).or_default().push(username);
                }
                Some(ScreenId::SshKeyImport) // stay on screen for more usernames
            }
            (ScreenId::SshKeyImport, UserInput::SkipSshKeys) => {
                self.ssh_current_user_idx += 1;
                if self.ssh_current_user_idx >= self.ssh_users.len() {
                    self.run_install(executor)
                } else {
                    Some(ScreenId::SshKeyImport)
                }
            }
            (ScreenId::SshKeyImport, UserInput::Back) => {
                if self.ssh_current_user_idx > 0 {
                    self.ssh_current_user_idx -= 1;
                    Some(ScreenId::SshKeyImport)
                } else {
                    self.ssh_keys.clear();
                    self.current_screen = ScreenId::Confirm;
                    Some(ScreenId::Confirm)
                }
            }
            (ScreenId::SshKeyImport, UserInput::AbortInstall) => {
                self.abort(executor, "User aborted at SSH key import".to_string())
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
                executor.execute(&Operation::Reboot);
                None
            }
            (ScreenId::Reboot, UserInput::ExitInstaller) => {
                self.action_manifest
                    .record(Operation::Exit, OperationOutcome::Success);
                self.action_manifest.final_state = InstallerFinalState::Exited;
                executor.execute(&Operation::Exit);
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

    /// Kick off the easy-mode network flow. Wifi is never auto-selected: if
    /// no ethernet interface has carrier, the user is dropped on the
    /// NetworkConfig screen to choose manually.
    fn start_easy_mode(&mut self, executor: &mut dyn OperationExecutor) -> Option<ScreenId> {
        let connected_eth: Option<String> = self
            .interfaces
            .iter()
            .find(|i| i.kind == InterfaceKind::Ethernet && i.has_link && i.has_carrier)
            .map(|i| i.name.clone());

        if let Some(eth_name) = connected_eth {
            self.selected_interface = Some(eth_name.clone());
            self.bring_ethernet_online(eth_name, executor)
        } else {
            // No wired carrier — prompt the user (wifi must not be picked
            // automatically in Easy mode).
            self.current_screen = ScreenId::NetworkConfig;
            self.error_message = Some(
                "No wired connection detected — select an interface".to_string(),
            );
            Some(ScreenId::NetworkConfig)
        }
    }

    /// Pick the most redundant RAID layout and the largest compatible disk
    /// group automatically. Returns false when no disks are available so the
    /// caller can fall back to the manual flow.
    pub fn apply_easy_disk_defaults(&mut self) -> bool {
        if self.disk_groups.is_empty() && self.all_disks.is_empty() {
            return false;
        }

        // Largest group by disk count, tie-broken by total bytes, then index
        // so the choice is deterministic.
        let best = self
            .disk_groups
            .iter()
            .enumerate()
            .max_by(|(ai, a), (bi, b)| {
                a.disk_count()
                    .cmp(&b.disk_count())
                    .then_with(|| a.total_bytes().cmp(&b.total_bytes()))
                    .then_with(|| bi.cmp(ai)) // prefer lower index on tie
            });

        let Some((group_idx, group)) = best else {
            return false;
        };

        let count = group.disk_count();
        if count == 0 {
            return false;
        }

        let raid = RaidConfig::recommended_for_count(count);

        self.selected_raid = Some(raid.clone());
        if matches!(raid, RaidConfig::Single) {
            // Single mode uses the per-disk path — pick the first disk of the
            // largest group so the Confirm screen has something concrete.
            let first_device = &group.disks[0].device;
            let disk_idx = self
                .all_disks
                .iter()
                .position(|d| &d.device == first_device)
                .unwrap_or(0);
            self.selected_disk = Some(disk_idx);
            self.selected_disk_group = None;
        } else {
            self.selected_disk_group = Some(group_idx);
            self.selected_disk = None;
        }
        true
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
            if let Some(wifi_iface) = self
                .interfaces
                .iter()
                .find(|i| i.kind == InterfaceKind::Wifi)
            {
                let wifi_name = wifi_iface.name.clone();
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
                if let Some(eth_iface) = self
                    .interfaces
                    .iter()
                    .find(|i| i.kind == InterfaceKind::Ethernet)
                {
                    // Try the first ethernet anyway
                    let eth_name = eth_iface.name.clone();
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
            // No password needed for open networks — skip WPS prompt too
            self.current_screen = ScreenId::WifiPassword;
            Some(ScreenId::WifiPassword)
        } else {
            // Ask if user wants to use WPS before password entry
            self.current_screen = ScreenId::WpsPrompt;
            Some(ScreenId::WpsPrompt)
        }
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

        self.wifi_password = Some(password.clone());

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

    fn start_wps(&mut self, executor: &mut dyn OperationExecutor) -> Option<ScreenId> {
        let iface_name = match &self.selected_interface {
            Some(name) => name.clone(),
            None => {
                self.error_message = Some("No wifi interface selected".to_string());
                return None;
            }
        };

        let op = Operation::WpsPbcStart {
            interface: iface_name.clone(),
        };
        let result = executor.execute(&op);
        self.action_manifest.record(op, result.to_outcome());

        if result.is_error() {
            self.error_message = Some(format!("Failed to start WPS: {:?}", result));
            return None;
        }

        self.wps_start_time = Some(std::time::Instant::now());
        self.network_state = NetworkState::WpsWaiting;
        self.current_screen = ScreenId::WpsWaiting;
        Some(ScreenId::WpsWaiting)
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

    /// Generate WiFi QR code string for the currently connected network.
    /// Format: WIFI:T:<security>;S:<ssid>;P:<password>;;
    pub fn wifi_qr_string(&self) -> Option<String> {
        let ssid = self.selected_ssid.as_ref()?;

        // Look up security type from wifi_networks
        let security = self
            .wifi_networks
            .iter()
            .find(|n| &n.ssid == ssid)
            .map(|n| &n.security);

        let sec_str = match security {
            Some(crate::manifest::WifiSecurity::Open) => "nopass",
            Some(crate::manifest::WifiSecurity::Wep) => "WEP",
            _ => "WPA", // WPA2, WPA3, or unknown default to WPA
        };

        // Escape special characters in SSID and password per WiFi QR spec
        let escaped_ssid = wifi_qr_escape(ssid);

        if sec_str == "nopass" {
            Some(format!("WIFI:T:nopass;S:{};;", escaped_ssid))
        } else {
            let password = self.wifi_password.as_deref().unwrap_or("");
            let escaped_pass = wifi_qr_escape(password);
            Some(format!(
                "WIFI:T:{};S:{};P:{};;",
                sec_str, escaped_ssid, escaped_pass
            ))
        }
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
            options: None,
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

        // Create subvolumes that Town OS expects
        for name in &["@etc", "@var"] {
            let op = Operation::CreateBtrfsSubvolume {
                mount_point: self.mount_point.clone(),
                name: name.to_string(),
            };
            let result = executor.execute(&op);
            self.action_manifest.record(op, result.to_outcome());
        }

        // Generate mount service and persist network config BEFORE install
        // so they are written even if InstallBaseSystem fails
        let etc = self.etc_prefix().to_string();
        crate::engine::real_ops::cmd_log_append(format!(
            "  etc_prefix={} mount_point={}",
            etc, self.mount_point
        ));
        let fstab_device = super::real_ops::disk::partition_path(&devices[0]);
        let fstab_op = Operation::GenerateFstab {
            mount_point: etc.clone(),
            device: fstab_device,
            fs_type: "btrfs".to_string(),
        };
        let fstab_result = executor.execute(&fstab_op);
        self.action_manifest
            .record(fstab_op, fstab_result.to_outcome());

        // Persist network configuration
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

        // Import SSH keys from GitHub
        for (system_user, github_names) in &self.ssh_keys.clone() {
            for github_name in github_names {
                let op = Operation::ImportSshKeys {
                    mount_point: self.mount_point.clone(),
                    system_user: system_user.clone(),
                    github_username: github_name.clone(),
                };
                let result = executor.execute(&op);
                self.action_manifest.record(op, result.to_outcome());
            }
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

            // Step 2b: WPS waiting — poll for completion
            NetworkState::WpsWaiting => {
                // Check for WPS timeout (120 seconds)
                if let Some(start) = self.wps_start_time {
                    if start.elapsed() > std::time::Duration::from_secs(120) {
                        self.wps_start_time = None;
                        self.error_message = Some("WPS timed out — no router responded".to_string());
                        self.network_state = NetworkState::Scanning;
                        self.current_screen = ScreenId::WifiSelect;
                        return true;
                    }
                }

                let op = Operation::WpsPbcStatus {
                    interface: iface_name.clone(),
                };
                let result = executor.execute(&op);
                self.action_manifest.record(op, result.to_outcome());

                match result {
                    OperationResult::WpsCompleted => {
                        self.wps_start_time = None;
                        self.connectivity_retries = 0;
                        self.network_state = NetworkState::Connected;
                        self.current_screen = ScreenId::NetworkProgress;
                    }
                    OperationResult::WpsPending => {
                        // Still waiting — keep polling
                    }
                    _ => {
                        // Error or unexpected result — keep polling until timeout
                    }
                }
                true
            }

            // Step 2c: Wifi connected — proceed to DHCP
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

/// Escape special characters in WiFi QR code fields.
/// Per the WiFi QR spec, these characters must be backslash-escaped:
/// \, ;, ,, ", and :
fn wifi_qr_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | ';' | ',' | '"' | ':' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}
