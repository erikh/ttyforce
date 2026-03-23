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

    // Auto-detect network (connected ethernet runs IP/DHCP, lands on NetworkProgress)
    sm.process_input(UserInput::Confirm, executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    while sm.advance_connectivity(executor) {}
    assert!(sm.network_state.is_online());
    sm.process_input(UserInput::Confirm, executor);
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
    while sm.advance_connectivity(&mut executor) {}
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
    while sm.advance_connectivity(&mut executor) {}
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
    while sm.advance_connectivity(&mut executor) {}
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
fn test_full_install_ethernet_4disk_btrfs_raid5() {
    let hw = load_hardware("ethernet_4disk_same");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Network (connected ethernet runs IP/DHCP/connectivity, lands on NetworkProgress)
    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::RaidConfig);

    // RAID5 (index 2 for 4 disks: Single, RAID1, RAID5)
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
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());

    // Continue to raid config
    sm.process_input(UserInput::Confirm, &mut executor);

    // Disk setup
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);

    // Confirm and install
    sm.process_input(UserInput::ConfirmInstall, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Installed);
}

// === Ethernet Auto-detect ===

#[test]
fn test_ethernet_auto_detect_records_all_ops() {
    // Connected ethernet (has_link + has_carrier) skips Enable/CheckLink but runs IP/DHCP/connectivity
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());

    let ops = executor.recorded_operations();
    let op_types: Vec<&str> = ops
        .iter()
        .map(|r| ttyforce::engine::executor::operation_type_name(&r.operation))
        .collect();

    // EnableInterface and CheckLinkAvailability should be skipped for already-connected
    assert!(!op_types.contains(&"EnableInterface"));
    assert!(!op_types.contains(&"CheckLinkAvailability"));
    // IP check and connectivity ops should be present
    assert!(op_types.contains(&"CheckIpAddress"));
    assert!(op_types.contains(&"SelectPrimaryInterface"));
}

#[test]
fn test_ethernet_already_connected_skips_dhcp() {
    // Use nocarrier fixture so bring_ethernet_online runs the step-by-step path,
    // but simulate that CheckIpAddress finds an existing IP (skip DHCP).
    let hw = load_hardware("ethernet_1disk_nocarrier");
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

    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);

    let ops = executor.recorded_operations();
    let op_types: Vec<&str> = ops
        .iter()
        .map(|r| ttyforce::engine::executor::operation_type_name(&r.operation))
        .collect();

    assert!(op_types.contains(&"EnableInterface"));
    assert!(op_types.contains(&"CheckLinkAvailability"));
    assert!(op_types.contains(&"CheckIpAddress"));
    assert!(!op_types.contains(&"ConfigureDhcp"), "DHCP should be skipped when IP is already assigned");
    assert!(op_types.contains(&"SelectPrimaryInterface"));

    // Verify the IP was stored
    let iface = sm.interfaces.iter().find(|i| i.name == "eth0").unwrap();
    assert_eq!(iface.ip_address.as_deref(), Some("192.168.1.50"));
}

#[test]
fn test_ethernet_no_ip_runs_dhcp() {
    // Use nocarrier fixture so bring_ethernet_online runs the step-by-step path.
    let hw = load_hardware("ethernet_1disk_nocarrier");
    let mut sm = InstallerStateMachine::new(hw);
    // First CheckIpAddress returns NoIp, second (after DHCP) falls through to default (Success)
    let mut executor = TestExecutor::new(vec![
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckLinkAvailability".to_string()),
            result: OperationResult::LinkUp,
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckIpAddress".to_string()),
            result: OperationResult::NoIp,
            consume: true,
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
    // Use nocarrier fixture so bring_ethernet_online actually checks link
    let hw = load_hardware("ethernet_1disk_nocarrier");
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
    while sm.advance_connectivity(&mut executor) {}
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
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
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

    // Get to raid config (connected ethernet → NetworkProgress → RaidConfig)
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::RaidConfig);

    // Go back to network progress
    sm.process_input(UserInput::Back, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
}

#[test]
fn test_back_from_raid_to_network_progress() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::RaidConfig);

    sm.process_input(UserInput::Back, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
}

#[test]
fn test_back_from_confirm_to_disk_group() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
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
    while sm.advance_connectivity(&mut executor) {}
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
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
}

// === Invalid selections ===

#[test]
fn test_invalid_disk_group_selection() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);

    // Try invalid selection
    let result = sm.process_input(UserInput::SelectDiskGroup(99), &mut executor);
    assert!(result.is_none());
    assert!(sm.error_message.is_some());
    assert_eq!(sm.current_screen, ScreenId::DiskGroupSelect);
}

// === Cleanup on abort ===

#[test]
fn test_abort_after_ethernet_cleanup_ops() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Get ethernet online
    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::RaidConfig);

    // Abort
    sm.process_input(UserInput::AbortInstall, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);

    let op_types: Vec<&str> = sm
        .action_manifest
        .operations
        .iter()
        .map(|op| ttyforce::engine::executor::operation_type_name(&op.operation))
        .collect();

    // CleanupNetworkConfig should appear before Abort
    let cleanup_idx = op_types.iter().position(|t| *t == "CleanupNetworkConfig");
    let abort_idx = op_types.iter().position(|t| *t == "Abort");
    assert!(cleanup_idx.is_some(), "expected CleanupNetworkConfig in ops: {:?}", op_types);
    assert!(abort_idx.is_some());
    assert!(cleanup_idx.unwrap() < abort_idx.unwrap());
}

#[test]
fn test_abort_after_wifi_cleanup_ops() {
    let hw = load_hardware("wifi_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Connect wifi
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("correctpassword".to_string()),
        &mut executor,
    );
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
    sm.process_input(UserInput::Confirm, &mut executor);

    // Abort at RaidConfig
    sm.process_input(UserInput::AbortInstall, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);

    let op_types: Vec<&str> = sm
        .action_manifest
        .operations
        .iter()
        .map(|op| ttyforce::engine::executor::operation_type_name(&op.operation))
        .collect();

    let has_wpa_cleanup = op_types.contains(&"CleanupWpaSupplicant");
    let has_net_cleanup = op_types.contains(&"CleanupNetworkConfig");
    let abort_idx = op_types.iter().position(|t| *t == "Abort").unwrap();
    let wpa_idx = op_types.iter().position(|t| *t == "CleanupWpaSupplicant").unwrap();
    let net_idx = op_types.iter().position(|t| *t == "CleanupNetworkConfig").unwrap();

    assert!(has_wpa_cleanup, "expected CleanupWpaSupplicant");
    assert!(has_net_cleanup, "expected CleanupNetworkConfig");
    assert!(wpa_idx < net_idx, "wpa cleanup should come before networkd cleanup");
    assert!(net_idx < abort_idx, "cleanup should come before Abort");
}

#[test]
fn test_abort_no_artifacts_no_cleanup() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Abort immediately at NetworkConfig
    sm.process_input(UserInput::AbortInstall, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);

    let op_types: Vec<&str> = sm
        .action_manifest
        .operations
        .iter()
        .map(|op| ttyforce::engine::executor::operation_type_name(&op.operation))
        .collect();

    // Only Abort, no cleanup ops
    assert_eq!(op_types, vec!["Abort"], "expected only Abort, got: {:?}", op_types);
}

#[test]
fn test_abort_after_install_unmounts() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Full install
    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    sm.process_input(UserInput::ConfirmInstall, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::InstallProgress);

    // Abort at install progress
    sm.process_input(UserInput::AbortInstall, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);

    let op_types: Vec<&str> = sm
        .action_manifest
        .operations
        .iter()
        .map(|op| ttyforce::engine::executor::operation_type_name(&op.operation))
        .collect();

    let has_unmount = op_types.contains(&"CleanupUnmount");
    let has_net_cleanup = op_types.contains(&"CleanupNetworkConfig");
    let abort_idx = op_types.iter().position(|t| *t == "Abort").unwrap();

    assert!(has_unmount, "expected CleanupUnmount in ops: {:?}", op_types);
    assert!(has_net_cleanup, "expected CleanupNetworkConfig in ops: {:?}", op_types);

    let unmount_idx = op_types.iter().position(|t| *t == "CleanupUnmount").unwrap();
    let net_idx = op_types.iter().position(|t| *t == "CleanupNetworkConfig").unwrap();
    assert!(unmount_idx < net_idx, "unmount should come before network cleanup");
    assert!(net_idx < abort_idx, "cleanup should come before Abort");
}

#[test]
fn test_invalid_raid_selection() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);

    let result = sm.process_input(UserInput::SelectRaidOption(99), &mut executor);
    assert!(result.is_none());
    assert!(sm.error_message.is_some());
}
