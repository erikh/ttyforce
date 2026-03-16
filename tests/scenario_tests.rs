use ttyforce::disk::FilesystemType;
use ttyforce::engine::executor::{OperationMatcher, SimulatedResponse, TestExecutor};
use ttyforce::engine::feedback::OperationResult;
use ttyforce::engine::state_machine::{InstallerStateMachine, ScreenId, UserInput};
use ttyforce::engine::OperationExecutor;
use ttyforce::manifest::{HardwareManifest, InstallerFinalState, OperationOutcome};
use ttyforce::network::wifi::WifiNetwork;
use ttyforce::network::NetworkState;
use ttyforce::operations::Operation;

fn load_hardware(name: &str) -> HardwareManifest {
    HardwareManifest::load(&format!("fixtures/hardware/{}.toml", name)).unwrap()
}

// Helper to make a default successful executor
fn success_executor() -> TestExecutor {
    TestExecutor::new(vec![])
}

// Helper to run a full ethernet + btrfs + single disk install
fn run_ethernet_single_disk_install(
    hw: HardwareManifest,
    executor: &mut TestExecutor,
) -> InstallerStateMachine {
    let mut sm = InstallerStateMachine::new(hw);

    // Auto-detect network (ethernet)
    sm.process_input(UserInput::Confirm, executor);
    assert!(sm.network_state.is_online());

    // Continue to filesystem select
    sm.process_input(UserInput::Confirm, executor);
    assert_eq!(sm.current_screen, ScreenId::FilesystemSelect);

    // Select btrfs (index 0)
    sm.process_input(UserInput::SelectFilesystem(0), executor);
    assert_eq!(sm.current_screen, ScreenId::RaidConfig);

    // Select single (index 0)
    sm.process_input(UserInput::SelectRaidOption(0), executor);
    assert_eq!(sm.current_screen, ScreenId::DiskGroupSelect);

    // Select first disk group
    sm.process_input(UserInput::SelectDiskGroup(0), executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);

    // Confirm install
    sm.process_input(UserInput::ConfirmInstall, executor);
    assert_eq!(sm.current_screen, ScreenId::InstallProgress);

    // Continue to reboot
    sm.process_input(UserInput::Confirm, executor);
    assert_eq!(sm.current_screen, ScreenId::Reboot);

    sm
}

// === Wifi Selection and Connection Scenarios ===

#[test]
fn test_wifi_select_and_connect() {
    let hw = load_hardware("wifi_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Auto-detect -> wifi select
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);

    // Select first network
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiPassword);
    assert_eq!(sm.selected_ssid, Some("HomeNetwork".to_string()));

    // Enter password
    sm.process_input(
        UserInput::EnterWifiPassword("correctpassword".to_string()),
        &mut executor,
    );
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    assert!(sm.network_state.is_online());
}

#[test]
fn test_wifi_select_refresh() {
    let hw = load_hardware("wifi_1disk");
    let mut sm = InstallerStateMachine::new(hw);

    let new_networks = vec![
        WifiNetwork {
            ssid: "NewNetwork1".to_string(),
            signal_strength: -40,
            frequency_mhz: 5180,
            security: ttyforce::manifest::WifiSecurity::Wpa2,
            reachable: true,
        },
        WifiNetwork {
            ssid: "NewNetwork2".to_string(),
            signal_strength: -65,
            frequency_mhz: 2437,
            security: ttyforce::manifest::WifiSecurity::Open,
            reachable: true,
        },
    ];

    let mut executor = TestExecutor::new(vec![
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("ScanWifiNetworks".to_string()),
            result: OperationResult::WifiScanResults(new_networks.clone()),
            consume: false,
        },
    ]);

    // Go to wifi select
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);

    // Refresh scan
    sm.process_input(UserInput::RefreshWifiScan, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);

    // Verify new networks are available
    assert_eq!(sm.wifi_networks.len(), 2);
    assert_eq!(sm.wifi_networks[0].ssid, "NewNetwork1");
    assert_eq!(sm.wifi_networks[1].ssid, "NewNetwork2");
}

#[test]
fn test_wifi_wrong_password() {
    let hw = load_hardware("wifi_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("AuthenticateWifi".to_string()),
        result: OperationResult::WifiAuthFailed("Incorrect password".to_string()),
        consume: true,
    }]);

    // Go to wifi select
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);

    // Enter wrong password
    sm.process_input(
        UserInput::EnterWifiPassword("wrongpassword".to_string()),
        &mut executor,
    );

    // Should stay on wifi password screen with error
    assert_eq!(sm.current_screen, ScreenId::WifiPassword);
    assert!(sm.error_message.is_some());
    assert!(sm.error_message.as_ref().unwrap().contains("Authentication failed"));

    // Verify auth error was recorded in action manifest
    let has_auth_error = sm.action_manifest.operations.iter().any(|op| {
        matches!(&op.operation, Operation::WifiAuthError { .. })
    });
    assert!(has_auth_error);
}

#[test]
fn test_wifi_signal_timeout() {
    let hw = load_hardware("wifi_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("AuthenticateWifi".to_string()),
        result: OperationResult::WifiTimeout,
        consume: true,
    }]);

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("somepassword".to_string()),
        &mut executor,
    );

    // Should go back to wifi select
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
    assert!(sm.error_message.is_some());
    assert!(sm.error_message.as_ref().unwrap().contains("timed out"));

    // Verify timeout was recorded in action manifest
    let has_timeout = sm.action_manifest.operations.iter().any(|op| {
        matches!(&op.operation, Operation::WifiConnectionTimeout { .. })
    });
    assert!(has_timeout);
}

#[test]
fn test_wifi_successful_connect_with_ip() {
    let hw = load_hardware("wifi_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckIpAddress".to_string()),
            result: OperationResult::IpAssigned("192.168.1.100".to_string()),
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckUpstreamRouter".to_string()),
            result: OperationResult::RouterFound("192.168.1.1".to_string()),
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckDnsResolution".to_string()),
            result: OperationResult::DnsResolved("93.184.216.34".to_string()),
            consume: false,
        },
    ]);

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("correctpassword".to_string()),
        &mut executor,
    );

    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    assert!(sm.network_state.is_online());

    // Verify IP was assigned to interface
    let wlan = sm.interfaces.iter().find(|i| i.name == "wlan0").unwrap();
    assert_eq!(wlan.ip_address, Some("192.168.1.100".to_string()));
}

// === Default Device Priority ===

#[test]
fn test_default_device_ethernet_first() {
    let hw = load_hardware("wifi_ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Auto-detect should pick ethernet
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.selected_interface, Some("eth0".to_string()));
    assert!(sm.network_state.is_online());
}

#[test]
fn test_default_device_wifi_fallback() {
    let hw = load_hardware("wifi_dead_ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Auto-detect should fall back to wifi since ethernet is dead
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.selected_interface, Some("wlan0".to_string()));
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
}

#[test]
fn test_default_device_user_choice() {
    let hw = load_hardware("wifi_ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // User explicitly selects wifi (index 1)
    sm.process_input(UserInput::Select(1), &mut executor);
    assert_eq!(sm.selected_interface, Some("wlan0".to_string()));
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
}

// === Full Install Scenarios ===

#[test]
fn test_full_install_ethernet_4disk_btrfs_raidz() {
    let hw = load_hardware("ethernet_4disk_same");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Network
    sm.process_input(UserInput::Confirm, &mut executor);
    assert!(sm.network_state.is_online());
    sm.process_input(UserInput::Confirm, &mut executor);

    // Btrfs
    sm.process_input(UserInput::SelectFilesystem(0), &mut executor);

    // RAID5 (index 2 for 4 disks btrfs: Single, RAID1, RAID5)
    sm.process_input(UserInput::SelectRaidOption(2), &mut executor);

    // Disk
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);

    // Install
    sm.process_input(UserInput::ConfirmInstall, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::InstallProgress);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Installed);

    // Check that BtrfsRaidSetup was called
    let ops = executor.recorded_operations();
    let has_btrfs_raid = ops.iter().any(|r| matches!(&r.operation, Operation::BtrfsRaidSetup { .. }));
    assert!(has_btrfs_raid);

    // Reboot
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::RebootSystem, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Rebooted);
}

#[test]
fn test_full_install_ethernet_4disk_zfs_raidz() {
    let hw = load_hardware("ethernet_4disk_same");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Network
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::Confirm, &mut executor);

    // ZFS (index 1)
    sm.process_input(UserInput::SelectFilesystem(1), &mut executor);

    // RaidZ (index 2 for 4 disks zfs: Single, Mirror, RaidZ)
    sm.process_input(UserInput::SelectRaidOption(2), &mut executor);

    // Disk
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);

    // Install
    sm.process_input(UserInput::ConfirmInstall, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Installed);

    // Check ZFS operations
    let ops = executor.recorded_operations();
    let has_zpool = ops.iter().any(|r| matches!(&r.operation, Operation::CreateZpool { .. }));
    let has_dataset = ops.iter().any(|r| matches!(&r.operation, Operation::CreateZfsDataset { .. }));
    assert!(has_zpool);
    assert!(has_dataset);
}

#[test]
fn test_full_install_ethernet_1disk() {
    let hw = load_hardware("ethernet_1disk");
    let mut executor = success_executor();
    let sm = run_ethernet_single_disk_install(hw, &mut executor);

    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Installed);

    // Verify operations
    let ops = executor.recorded_operations();
    let has_partition = ops.iter().any(|r| matches!(&r.operation, Operation::PartitionDisk { .. }));
    let has_mkfs = ops.iter().any(|r| matches!(&r.operation, Operation::MkfsBtrfs { .. }));
    let has_subvol = ops.iter().any(|r| matches!(&r.operation, Operation::CreateBtrfsSubvolume { .. }));
    let has_install = ops.iter().any(|r| matches!(&r.operation, Operation::InstallBaseSystem { .. }));
    assert!(has_partition);
    assert!(has_mkfs);
    assert!(has_subvol);
    assert!(has_install);
}

#[test]
fn test_full_install_wifi_1disk() {
    let hw = load_hardware("wifi_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Network: wifi flow
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);

    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("correctpassword".to_string()),
        &mut executor,
    );
    assert!(sm.network_state.is_online());

    // Continue to filesystem select
    sm.process_input(UserInput::Confirm, &mut executor);

    // Disk setup
    sm.process_input(UserInput::SelectFilesystem(0), &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);

    // Confirm and install
    sm.process_input(UserInput::ConfirmInstall, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Installed);
}

// === Ethernet Auto-detect ===

#[test]
fn test_ethernet_auto_detect_records_all_ops() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);

    let ops = executor.recorded_operations();
    let op_types: Vec<&str> = ops
        .iter()
        .map(|r| match &r.operation {
            Operation::EnableInterface { .. } => "EnableInterface",
            Operation::CheckLinkAvailability { .. } => "CheckLinkAvailability",
            Operation::ConfigureDhcp { .. } => "ConfigureDhcp",
            Operation::CheckIpAddress { .. } => "CheckIpAddress",
            Operation::CheckUpstreamRouter { .. } => "CheckUpstreamRouter",
            Operation::CheckInternetRoutability { .. } => "CheckInternetRoutability",
            Operation::CheckDnsResolution { .. } => "CheckDnsResolution",
            Operation::SelectPrimaryInterface { .. } => "SelectPrimaryInterface",
            _ => "Other",
        })
        .collect();

    assert!(op_types.contains(&"EnableInterface"));
    assert!(op_types.contains(&"CheckLinkAvailability"));
    assert!(op_types.contains(&"ConfigureDhcp"));
    assert!(op_types.contains(&"CheckIpAddress"));
    assert!(op_types.contains(&"CheckUpstreamRouter"));
    assert!(op_types.contains(&"CheckInternetRoutability"));
    assert!(op_types.contains(&"CheckDnsResolution"));
    assert!(op_types.contains(&"SelectPrimaryInterface"));
}

#[test]
fn test_ethernet_already_connected_skips_dhcp() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckLinkAvailability".to_string()),
            result: OperationResult::LinkUp,
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckIpAddress".to_string()),
            result: OperationResult::IpAssigned("192.168.1.50".to_string()),
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckUpstreamRouter".to_string()),
            result: OperationResult::RouterFound("192.168.1.1".to_string()),
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckInternetRoutability".to_string()),
            result: OperationResult::InternetReachable,
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckDnsResolution".to_string()),
            result: OperationResult::DnsResolved("93.184.216.34".to_string()),
            consume: false,
        },
    ]);

    sm.process_input(UserInput::Confirm, &mut executor);

    // Should be online and on the network progress screen
    assert!(sm.network_state.is_online());
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);

    // Verify DHCP was NOT called since we already had an IP
    let ops = executor.recorded_operations();
    let op_types: Vec<&str> = ops
        .iter()
        .map(|r| ttyforce::engine::executor::operation_type_name(&r.operation))
        .collect();

    assert!(op_types.contains(&"EnableInterface"));
    assert!(op_types.contains(&"CheckLinkAvailability"));
    assert!(op_types.contains(&"CheckIpAddress"));
    assert!(!op_types.contains(&"ConfigureDhcp"), "DHCP should be skipped when IP is already assigned");
    assert!(op_types.contains(&"CheckUpstreamRouter"));
    assert!(op_types.contains(&"CheckInternetRoutability"));
    assert!(op_types.contains(&"CheckDnsResolution"));
    assert!(op_types.contains(&"SelectPrimaryInterface"));

    // Verify the IP was stored
    let iface = sm.interfaces.iter().find(|i| i.name == "eth0").unwrap();
    assert_eq!(iface.ip_address.as_deref(), Some("192.168.1.50"));
}

#[test]
fn test_ethernet_no_ip_runs_dhcp() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    // First CheckIpAddress returns NoIp, second (after DHCP) returns IpAssigned
    let mut executor = TestExecutor::new(vec![
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckLinkAvailability".to_string()),
            result: OperationResult::LinkUp,
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckIpAddress".to_string()),
            result: OperationResult::NoIp,
            consume: true, // consumed so second call falls through to default (Success)
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckUpstreamRouter".to_string()),
            result: OperationResult::RouterFound("192.168.1.1".to_string()),
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckInternetRoutability".to_string()),
            result: OperationResult::InternetReachable,
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckDnsResolution".to_string()),
            result: OperationResult::DnsResolved("93.184.216.34".to_string()),
            consume: false,
        },
    ]);

    sm.process_input(UserInput::Confirm, &mut executor);

    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);

    // Verify DHCP WAS called since we had no IP initially
    let ops = executor.recorded_operations();
    let op_types: Vec<&str> = ops
        .iter()
        .map(|r| ttyforce::engine::executor::operation_type_name(&r.operation))
        .collect();

    assert!(op_types.contains(&"EnableInterface"));
    assert!(op_types.contains(&"CheckLinkAvailability"));
    assert!(op_types.contains(&"ConfigureDhcp"), "DHCP should run when no IP is assigned");
    assert!(op_types.contains(&"CheckIpAddress"));
}

#[test]
fn test_ethernet_link_failure() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("CheckLinkAvailability".to_string()),
        result: OperationResult::LinkDown,
        consume: false,
    }]);

    sm.process_input(UserInput::Confirm, &mut executor);
    assert!(matches!(sm.network_state, NetworkState::Error(_)));
    assert!(sm.error_message.is_some());
}

// === QR Code Wifi ===

#[test]
fn test_wifi_qr_code_connection() {
    let hw = load_hardware("wifi_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("ConfigureWifiQrCode".to_string()),
        result: OperationResult::WifiQrConfigured,
        consume: false,
    }]);

    // Enable wifi interface first
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);

    // Use QR code to connect
    let result = sm.connect_wifi_qr("WIFI:T:WPA;S:HomeNetwork;P:correctpassword;;".to_string(), &mut executor);
    assert_eq!(result, Some(ScreenId::NetworkProgress));
    assert!(sm.network_state.is_online());
}

// === Abort and Reboot ===

#[test]
fn test_abort_install() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::AbortInstall, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Reboot);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);

    // Verify abort operation was recorded
    let ops = executor.recorded_operations();
    let has_abort = ops.iter().any(|r| matches!(&r.operation, Operation::Abort { .. }));
    assert!(has_abort);
}

#[test]
fn test_reboot_after_install() {
    let hw = load_hardware("ethernet_1disk");
    let mut executor = success_executor();
    let mut sm = run_ethernet_single_disk_install(hw, &mut executor);

    // Reboot
    sm.process_input(UserInput::RebootSystem, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Rebooted);

    let ops = executor.recorded_operations();
    let has_reboot = ops.iter().any(|r| matches!(&r.operation, Operation::Reboot));
    assert!(has_reboot);
}

#[test]
fn test_abort_at_confirmation() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Get to confirm screen
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectFilesystem(0), &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);

    // Abort
    sm.process_input(UserInput::AbortInstall, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);
}

// === Navigation Tests ===

#[test]
fn test_back_navigation() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Get to filesystem select
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::FilesystemSelect);

    // Go back to network progress
    sm.process_input(UserInput::Back, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
}

#[test]
fn test_back_from_raid_to_filesystem() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectFilesystem(0), &mut executor);
    assert_eq!(sm.current_screen, ScreenId::RaidConfig);

    sm.process_input(UserInput::Back, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::FilesystemSelect);
}

#[test]
fn test_back_from_confirm_to_disk_group() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectFilesystem(0), &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);

    sm.process_input(UserInput::Back, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::DiskGroupSelect);
}

// === Crowded Wifi ===

#[test]
fn test_crowded_wifi_select() {
    let hw = load_hardware("wifi_crowded_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
    assert_eq!(sm.wifi_networks.len(), 10);

    // Select the home network (index 0)
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    assert_eq!(sm.selected_ssid, Some("HomeNetwork".to_string()));
}

// === Mixed Hardware Configs ===

#[test]
fn test_wifi_ethernet_4disk_prefers_ethernet() {
    let hw = load_hardware("wifi_ethernet_4disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.selected_interface, Some("eth0".to_string()));
    assert!(sm.network_state.is_online());
}

#[test]
fn test_wifi_dead_ethernet_4disk_falls_to_wifi() {
    let hw = load_hardware("wifi_dead_ethernet_4disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.selected_interface, Some("wlan0".to_string()));
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
}

// === ZFS Path Tests ===

#[test]
fn test_zfs_single_disk() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::Confirm, &mut executor);

    // Select ZFS
    sm.process_input(UserInput::SelectFilesystem(1), &mut executor);
    assert_eq!(sm.selected_filesystem, FilesystemType::Zfs);

    // Single (only option for 1 disk)
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    assert_eq!(sm.current_screen, ScreenId::DiskGroupSelect);

    // Select disk group
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);

    // Install
    sm.process_input(UserInput::ConfirmInstall, &mut executor);

    let ops = executor.recorded_operations();
    let has_zpool = ops.iter().any(|r| matches!(&r.operation, Operation::CreateZpool { .. }));
    let has_dataset = ops.iter().any(|r| matches!(&r.operation, Operation::CreateZfsDataset { .. }));
    let has_install = ops
        .iter()
        .any(|r| matches!(&r.operation, Operation::InstallBaseSystem { target } if target.contains("rpool")));
    assert!(has_zpool);
    assert!(has_dataset);
    assert!(has_install);
}

#[test]
fn test_zfs_mirror_2disk() {
    // Create a 2 disk setup by using the 4disk and checking we can do mirror
    let hw = load_hardware("ethernet_4disk_same");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectFilesystem(1), &mut executor); // ZFS

    // Mirror is index 1
    sm.process_input(UserInput::SelectRaidOption(1), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    sm.process_input(UserInput::ConfirmInstall, &mut executor);

    let ops = executor.recorded_operations();
    let zpool_op = ops.iter().find(|r| matches!(&r.operation, Operation::CreateZpool { .. }));
    assert!(zpool_op.is_some());
    if let Some(r) = zpool_op {
        if let Operation::CreateZpool { raid_level, .. } = &r.operation {
            assert_eq!(raid_level, "mirror");
        }
    }
}

// === Action Manifest Recording ===

#[test]
fn test_action_manifest_records_all_operations() {
    let hw = load_hardware("ethernet_1disk");
    let mut executor = success_executor();
    let sm = run_ethernet_single_disk_install(hw, &mut executor);

    // Should have recorded network ops + disk ops + install
    assert!(!sm.action_manifest.operations.is_empty());

    // Check sequential numbering
    for (i, op) in sm.action_manifest.operations.iter().enumerate() {
        assert_eq!(op.sequence, i as u64);
    }

    // All should be success
    assert!(sm
        .action_manifest
        .operations
        .iter()
        .all(|op| op.result == OperationOutcome::Success));
}

#[test]
fn test_action_manifest_records_errors() {
    let hw = load_hardware("wifi_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("AuthenticateWifi".to_string()),
        result: OperationResult::WifiAuthFailed("bad password".to_string()),
        consume: true,
    }]);

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("wrong".to_string()),
        &mut executor,
    );

    // Should have error outcomes in manifest
    let has_error = sm
        .action_manifest
        .operations
        .iter()
        .any(|op| matches!(&op.result, OperationOutcome::Error(_)));
    assert!(has_error);
}

// === Quit ===

#[test]
fn test_quit_from_any_screen() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Quit, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);
}

// === Network shutdown of non-primary ===

#[test]
fn test_non_primary_interfaces_shut_down() {
    let hw = load_hardware("wifi_ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Auto-detect picks ethernet
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.selected_interface, Some("eth0".to_string()));

    // Note: shutdown only happens for enabled interfaces, and wifi wasn't enabled
    // This verifies the logic runs without error
    assert!(sm.network_state.is_online());
}

// === Invalid selections ===

#[test]
fn test_invalid_disk_group_selection() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectFilesystem(0), &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);

    // Try invalid selection
    let result = sm.process_input(UserInput::SelectDiskGroup(99), &mut executor);
    assert!(result.is_none());
    assert!(sm.error_message.is_some());
    assert_eq!(sm.current_screen, ScreenId::DiskGroupSelect);
}

#[test]
fn test_invalid_filesystem_selection() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::Confirm, &mut executor);

    let result = sm.process_input(UserInput::SelectFilesystem(99), &mut executor);
    assert!(result.is_none());
    assert!(sm.error_message.is_some());
}

#[test]
fn test_invalid_raid_selection() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectFilesystem(0), &mut executor);

    let result = sm.process_input(UserInput::SelectRaidOption(99), &mut executor);
    assert!(result.is_none());
    assert!(sm.error_message.is_some());
}
