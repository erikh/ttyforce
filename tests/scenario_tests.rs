use ttyforce::disk::RaidConfig;
use ttyforce::engine::executor::{OperationMatcher, SimulatedResponse, TestExecutor};
use ttyforce::engine::feedback::OperationResult;
use ttyforce::engine::state_machine::{InstallMode, InstallerStateMachine, ScreenId, UserInput};
use ttyforce::engine::OperationExecutor;
use ttyforce::manifest::{HardwareManifest, InstallerFinalState, OperationOutcome};
use ttyforce::network::wifi::WifiNetwork;
use ttyforce::network::NetworkState;
use ttyforce::operations::Operation;

fn load_hardware(name: &str) -> Result<HardwareManifest, String> {
    HardwareManifest::load(&format!("fixtures/hardware/{}.toml", name)).map_err(|e| e.to_string())
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

    // Confirm install -> install (ssh_users is empty, skips SshKeyImport)
    sm.process_input(UserInput::ConfirmInstall, executor);
    assert_eq!(sm.current_screen, ScreenId::InstallProgress);

    // Continue to reboot
    sm.process_input(UserInput::Confirm, executor);
    assert_eq!(sm.current_screen, ScreenId::Reboot);

    sm
}

// === Wifi Selection and Connection Scenarios ===

#[test]
fn test_wifi_select_and_connect() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Auto-detect -> wifi select
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);

    // Select first network
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WpsPrompt);
    sm.process_input(UserInput::WpsDecline, &mut executor);
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
    Ok(())
}

#[test]
fn test_wifi_select_refresh() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
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
    Ok(())
}

#[test]
fn test_wifi_wrong_password() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("AuthenticateWifi".to_string()),
        result: OperationResult::WifiAuthFailed("Incorrect password".to_string()),
        consume: true,
    }]);

    // Go to wifi select
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);

    // Enter wrong password
    sm.process_input(
        UserInput::EnterWifiPassword("wrongpassword".to_string()),
        &mut executor,
    );

    // Should stay on wifi password screen with error
    assert_eq!(sm.current_screen, ScreenId::WifiPassword);
    assert!(sm.error_message.is_some());
    assert!(sm.error_message.as_ref().is_some_and(|m| m.contains("Authentication failed")));

    // Verify auth error was recorded in action manifest
    let has_auth_error = sm.action_manifest.operations.iter().any(|op| {
        matches!(&op.operation, Operation::WifiAuthError { .. })
    });
    assert!(has_auth_error);
    Ok(())
}

#[test]
fn test_wifi_signal_timeout() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("AuthenticateWifi".to_string()),
        result: OperationResult::WifiTimeout,
        consume: true,
    }]);

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("somepassword".to_string()),
        &mut executor,
    );

    // Should go back to wifi select
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
    assert!(sm.error_message.is_some());
    assert!(sm.error_message.as_ref().is_some_and(|m| m.contains("timed out")));

    // Verify timeout was recorded in action manifest
    let has_timeout = sm.action_manifest.operations.iter().any(|op| {
        matches!(&op.operation, Operation::WifiConnectionTimeout { .. })
    });
    assert!(has_timeout);
    Ok(())
}

#[test]
fn test_wifi_successful_connect_with_ip() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
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
    sm.process_input(UserInput::WpsDecline, &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("correctpassword".to_string()),
        &mut executor,
    );

    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());

    // Verify IP was assigned to interface
    let wlan = sm.interfaces.iter().find(|i| i.name == "wlan0").ok_or("wlan0 interface not found")?;
    assert_eq!(wlan.ip_address, Some("192.168.1.100".to_string()));
    Ok(())
}

// === Default Device Priority ===

#[test]
fn test_default_device_ethernet_first() -> Result<(), String> {
    let hw = load_hardware("wifi_ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Auto-detect should pick ethernet
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.selected_interface, Some("eth0".to_string()));
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
    Ok(())
}

#[test]
fn test_default_device_wifi_fallback() -> Result<(), String> {
    let hw = load_hardware("wifi_dead_ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Auto-detect should fall back to wifi since ethernet is dead
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.selected_interface, Some("wlan0".to_string()));
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
    Ok(())
}

#[test]
fn test_default_device_user_choice() -> Result<(), String> {
    let hw = load_hardware("wifi_ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // User explicitly selects wifi (index 1)
    sm.process_input(UserInput::Select(1), &mut executor);
    assert_eq!(sm.selected_interface, Some("wlan0".to_string()));
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
    Ok(())
}

// === Full Install Scenarios ===

#[test]
fn test_full_install_ethernet_4disk_btrfs_raid5() -> Result<(), String> {
    let hw = load_hardware("ethernet_4disk_same")?;
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
    Ok(())
}

#[test]
fn test_full_install_ethernet_1disk() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
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
    Ok(())
}

#[test]
fn test_full_install_wifi_1disk() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Network: wifi flow
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);

    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);
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
    Ok(())
}

// === Ethernet Auto-detect ===

#[test]
fn test_ethernet_auto_detect_records_all_ops() -> Result<(), String> {
    // Connected ethernet (has_link + has_carrier) skips Enable/CheckLink but runs IP/DHCP/connectivity
    let hw = load_hardware("ethernet_1disk")?;
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

    // EnableInterface should be skipped for already-connected (starts at DeviceEnabled)
    assert!(!op_types.contains(&"EnableInterface"));
    // Link check, IP check and connectivity ops should be present
    assert!(op_types.contains(&"CheckLinkAvailability"));
    assert!(op_types.contains(&"CheckIpAddress"));
    assert!(op_types.contains(&"SelectPrimaryInterface"));
    Ok(())
}

#[test]
fn test_ethernet_already_connected_skips_dhcp() -> Result<(), String> {
    // Use nocarrier fixture so bring_ethernet_online runs the step-by-step path,
    // but simulate that CheckIpAddress finds an existing IP (skip DHCP).
    let hw = load_hardware("ethernet_1disk_nocarrier")?;
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
    let iface = sm.interfaces.iter().find(|i| i.name == "eth0").ok_or("eth0 interface not found")?;
    assert_eq!(iface.ip_address.as_deref(), Some("192.168.1.50"));
    Ok(())
}

#[test]
fn test_ethernet_no_ip_runs_dhcp() -> Result<(), String> {
    // Use nocarrier fixture so bring_ethernet_online runs the step-by-step path.
    let hw = load_hardware("ethernet_1disk_nocarrier")?;
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
    while sm.advance_connectivity(&mut executor) {}

    let ops = executor.recorded_operations();
    let op_types: Vec<&str> = ops
        .iter()
        .map(|r| ttyforce::engine::executor::operation_type_name(&r.operation))
        .collect();

    assert!(op_types.contains(&"EnableInterface"));
    assert!(op_types.contains(&"CheckLinkAvailability"));
    assert!(op_types.contains(&"ConfigureDhcp"), "DHCP should run when no IP is assigned");
    assert!(op_types.contains(&"CheckIpAddress"));
    Ok(())
}

#[test]
fn test_ethernet_link_failure() -> Result<(), String> {
    // Use nocarrier fixture so bring_ethernet_online actually checks link
    let hw = load_hardware("ethernet_1disk_nocarrier")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("CheckLinkAvailability".to_string()),
        result: OperationResult::LinkDown,
        consume: false,
    }]);

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    assert!(matches!(sm.network_state, NetworkState::Error(_)));
    assert!(sm.error_message.is_some());
    Ok(())
}

#[test]
fn test_ethernet_link_retry_budget_is_order_of_magnitude_longer() -> Result<(), String> {
    // Regression test: the link/carrier check must keep polling for at
    // least 10x the other connectivity checks. Real hardware can need
    // tens of seconds to negotiate (STP on managed switches, delayed
    // phy autonegotiation, etc.).
    let hw = load_hardware("ethernet_1disk_nocarrier")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("CheckLinkAvailability".to_string()),
        result: OperationResult::LinkDown,
        consume: false,
    }]);

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    assert!(matches!(sm.network_state, NetworkState::Error(_)));

    let link_checks = executor
        .recorded_operations()
        .iter()
        .filter(|r| matches!(&r.operation, Operation::CheckLinkAvailability { .. }))
        .count();
    assert!(
        link_checks >= 100,
        "expected >= 100 link checks before giving up, saw {}",
        link_checks
    );
    Ok(())
}

#[test]
fn test_ethernet_link_recovers_after_long_wait() -> Result<(), String> {
    // Simulate a slow-to-come-up port: the first ~50 link checks return
    // LinkDown, then the port comes up and the flow proceeds to DHCP.
    // This would have failed before the retry-budget bump.
    let hw = load_hardware("ethernet_1disk_nocarrier")?;
    let mut sm = InstallerStateMachine::new(hw);

    let mut responses: Vec<SimulatedResponse> = (0..50)
        .map(|_| SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckLinkAvailability".to_string()),
            result: OperationResult::LinkDown,
            consume: true,
        })
        .collect();
    responses.push(SimulatedResponse {
        operation_match: OperationMatcher::ByType("CheckLinkAvailability".to_string()),
        result: OperationResult::LinkUp,
        consume: false,
    });
    let mut executor = TestExecutor::new(responses);

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    assert!(
        sm.network_state.is_online(),
        "slow-carrier port should eventually come online, got {:?}",
        sm.network_state
    );
    Ok(())
}

// === QR Code Wifi ===

#[test]
fn test_wifi_qr_code_connection() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
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
    Ok(())
}

// === WiFi QR Display Screen ===

#[test]
fn test_wifi_qr_display_from_network_progress() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Get to WifiSelect via auto-detect
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);

    // Select network, decline WPS, enter password
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("mypassword".to_string()),
        &mut executor,
    );
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);

    // Advance to online
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());

    // Show QR code
    let result = sm.process_input(UserInput::ShowWifiQr, &mut executor);
    assert_eq!(result, Some(ScreenId::WifiQrDisplay));
    assert_eq!(sm.current_screen, ScreenId::WifiQrDisplay);

    // Verify QR string is generated
    let qr = sm.wifi_qr_string().ok_or("wifi_qr_string returned None")?;
    assert!(qr.starts_with("WIFI:T:WPA;S:HomeNetwork;P:mypassword;;"));

    // Back returns to NetworkProgress
    let result = sm.process_input(UserInput::Back, &mut executor);
    assert_eq!(result, Some(ScreenId::NetworkProgress));
    Ok(())
}

#[test]
fn test_wifi_qr_display_enter_returns_to_network_progress() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("pass".to_string()),
        &mut executor,
    );
    while sm.advance_connectivity(&mut executor) {}

    sm.process_input(UserInput::ShowWifiQr, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiQrDisplay);

    // Enter also goes back
    let result = sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(result, Some(ScreenId::NetworkProgress));
    Ok(())
}

#[test]
fn test_wifi_qr_not_available_when_not_online() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("pass".to_string()),
        &mut executor,
    );
    // Don't advance connectivity - not online yet

    let result = sm.process_input(UserInput::ShowWifiQr, &mut executor);
    assert_eq!(result, None); // Should not navigate
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    Ok(())
}

#[test]
fn test_wifi_qr_not_available_for_ethernet() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Ethernet auto-detect
    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
    assert!(sm.selected_ssid.is_none()); // No WiFi SSID

    // ShowWifiQr should not navigate (no SSID)
    let result = sm.process_input(UserInput::ShowWifiQr, &mut executor);
    assert_eq!(result, None);
    Ok(())
}

#[test]
fn test_wifi_qr_string_open_network() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);

    // Simulate selecting an open network
    sm.selected_ssid = Some("OpenCafe".to_string());
    sm.wifi_networks.push(WifiNetwork {
        ssid: "OpenCafe".to_string(),
        signal_strength: -50,
        frequency_mhz: 2437,
        security: ttyforce::manifest::WifiSecurity::Open,
        reachable: true,
    });

    let qr = sm.wifi_qr_string().ok_or("wifi_qr_string returned None")?;
    assert_eq!(qr, "WIFI:T:nopass;S:OpenCafe;;");
    Ok(())
}

#[test]
fn test_wifi_qr_string_escapes_special_chars() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);

    sm.selected_ssid = Some("My:Net;work".to_string());
    sm.wifi_password = Some("pass;word".to_string());
    sm.wifi_networks.push(WifiNetwork {
        ssid: "My:Net;work".to_string(),
        signal_strength: -50,
        frequency_mhz: 2437,
        security: ttyforce::manifest::WifiSecurity::Wpa2,
        reachable: true,
    });

    let qr = sm.wifi_qr_string().ok_or("wifi_qr_string returned None")?;
    assert_eq!(qr, "WIFI:T:WPA;S:My\\:Net\\;work;P:pass\\;word;;");
    Ok(())
}

#[test]
fn test_wifi_qr_abort_from_display() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("pass".to_string()),
        &mut executor,
    );
    while sm.advance_connectivity(&mut executor) {}

    sm.process_input(UserInput::ShowWifiQr, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiQrDisplay);

    let result = sm.process_input(UserInput::AbortInstall, &mut executor);
    assert_eq!(result, Some(ScreenId::Reboot));
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);
    Ok(())
}

#[test]
fn test_wifi_qr_password_stored_after_connect() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);

    assert!(sm.wifi_password.is_none());
    sm.process_input(
        UserInput::EnterWifiPassword("secretpass".to_string()),
        &mut executor,
    );
    assert_eq!(sm.wifi_password, Some("secretpass".to_string()));
    Ok(())
}

// === Abort and Reboot ===

#[test]
fn test_abort_install() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::AbortInstall, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Reboot);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);

    // Verify abort operation was recorded
    let ops = executor.recorded_operations();
    let has_abort = ops.iter().any(|r| matches!(&r.operation, Operation::Abort { .. }));
    assert!(has_abort);
    Ok(())
}

#[test]
fn test_reboot_after_install() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut executor = success_executor();
    let mut sm = run_ethernet_single_disk_install(hw, &mut executor);

    // Reboot
    sm.process_input(UserInput::RebootSystem, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Rebooted);

    let ops = executor.recorded_operations();
    let has_reboot = ops.iter().any(|r| matches!(&r.operation, Operation::Reboot));
    assert!(has_reboot);
    Ok(())
}

#[test]
fn test_abort_at_confirmation() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
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
    Ok(())
}

// === Navigation Tests ===

#[test]
fn test_back_navigation() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
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
    Ok(())
}

#[test]
fn test_back_from_raid_to_network_progress() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::RaidConfig);

    sm.process_input(UserInput::Back, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    Ok(())
}

#[test]
fn test_back_from_confirm_to_disk_group() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
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
    Ok(())
}

// === Crowded Wifi ===

#[test]
fn test_crowded_wifi_select() -> Result<(), String> {
    let hw = load_hardware("wifi_crowded_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
    assert_eq!(sm.wifi_networks.len(), 10);

    // Select the home network (index 0) — goes to WPS prompt for secured networks
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    assert_eq!(sm.selected_ssid, Some("HomeNetwork".to_string()));
    assert_eq!(sm.current_screen, ScreenId::WpsPrompt);
    Ok(())
}

// === Mixed Hardware Configs ===

#[test]
fn test_wifi_ethernet_4disk_prefers_ethernet() -> Result<(), String> {
    let hw = load_hardware("wifi_ethernet_4disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.selected_interface, Some("eth0".to_string()));
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
    Ok(())
}

#[test]
fn test_wifi_dead_ethernet_4disk_falls_to_wifi() -> Result<(), String> {
    let hw = load_hardware("wifi_dead_ethernet_4disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.selected_interface, Some("wlan0".to_string()));
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
    Ok(())
}

// === Action Manifest Recording ===

#[test]
fn test_action_manifest_records_all_operations() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
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
    Ok(())
}

#[test]
fn test_action_manifest_records_errors() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("AuthenticateWifi".to_string()),
        result: OperationResult::WifiAuthFailed("bad password".to_string()),
        consume: true,
    }]);

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);
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
    Ok(())
}

// === Quit ===

#[test]
fn test_quit_from_any_screen() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Quit, &mut executor);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);
    Ok(())
}

// === Network shutdown of non-primary ===

#[test]
fn test_non_primary_interfaces_shut_down() -> Result<(), String> {
    let hw = load_hardware("wifi_ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Auto-detect picks ethernet
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.selected_interface, Some("eth0".to_string()));

    // Note: shutdown only happens for enabled interfaces, and wifi wasn't enabled
    // This verifies the logic runs without error
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
    Ok(())
}

// === Invalid selections ===

#[test]
fn test_invalid_disk_group_selection() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
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
    Ok(())
}

// === Cleanup on abort ===

#[test]
fn test_abort_after_ethernet_cleanup_ops() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
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
    let cleanup_idx = op_types.iter().position(|t| *t == "CleanupNetworkConfig")
        .ok_or(format!("expected CleanupNetworkConfig in ops: {:?}", op_types))?;
    let abort_idx = op_types.iter().position(|t| *t == "Abort")
        .ok_or("expected Abort in ops")?;
    assert!(cleanup_idx < abort_idx);
    Ok(())
}

#[test]
fn test_abort_after_wifi_cleanup_ops() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Connect wifi
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);
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

    let abort_idx = op_types.iter().position(|t| *t == "Abort")
        .ok_or("expected Abort in ops")?;
    let wpa_idx = op_types.iter().position(|t| *t == "CleanupWpaSupplicant")
        .ok_or("expected CleanupWpaSupplicant")?;
    let net_idx = op_types.iter().position(|t| *t == "CleanupNetworkConfig")
        .ok_or("expected CleanupNetworkConfig")?;

    assert!(wpa_idx < net_idx, "wpa cleanup should come before networkd cleanup");
    assert!(net_idx < abort_idx, "cleanup should come before Abort");
    Ok(())
}

#[test]
fn test_abort_no_artifacts_no_cleanup() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
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
    Ok(())
}

#[test]
fn test_abort_after_install_unmounts() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
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

    let abort_idx = op_types.iter().position(|t| *t == "Abort")
        .ok_or("expected Abort in ops")?;
    let unmount_idx = op_types.iter().position(|t| *t == "CleanupUnmount")
        .ok_or(format!("expected CleanupUnmount in ops: {:?}", op_types))?;
    let net_idx = op_types.iter().position(|t| *t == "CleanupNetworkConfig")
        .ok_or(format!("expected CleanupNetworkConfig in ops: {:?}", op_types))?;
    assert!(unmount_idx < net_idx, "unmount should come before network cleanup");
    assert!(net_idx < abort_idx, "cleanup should come before Abort");
    Ok(())
}

#[test]
fn test_invalid_raid_selection() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);

    let result = sm.process_input(UserInput::SelectRaidOption(99), &mut executor);
    assert!(result.is_none());
    assert!(sm.error_message.is_some());
    Ok(())
}

// === advance_connectivity tests ===

#[test]
fn test_advance_connectivity_full_flow() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Start ethernet — goes to NetworkProgress
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);

    // Drive through all steps
    let mut steps = 0;
    while sm.advance_connectivity(&mut executor) {
        steps += 1;
        assert!(steps < 20, "advance_connectivity looping too many times");
    }
    assert!(sm.network_state.is_online());
    assert!(steps > 0, "should have taken at least one step");
    Ok(())
}

#[test]
fn test_advance_connectivity_retries_router() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);

    // Router fails first 3 times, then succeeds
    let mut executor = TestExecutor::new(vec![
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckUpstreamRouter".to_string()),
            result: OperationResult::NoRouter,
            consume: true,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckUpstreamRouter".to_string()),
            result: OperationResult::NoRouter,
            consume: true,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckUpstreamRouter".to_string()),
            result: OperationResult::NoRouter,
            consume: true,
        },
    ]);

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}

    // Should eventually succeed (default Success after consumed responses)
    assert!(sm.network_state.is_online());
    Ok(())
}

#[test]
fn test_advance_connectivity_router_max_retries_error() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);

    // Router always fails
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("CheckUpstreamRouter".to_string()),
        result: OperationResult::NoRouter,
        consume: false,
    }]);

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}

    assert!(matches!(sm.network_state, NetworkState::Error(_)));
    assert!(sm.error_message.is_some());
    assert!(sm.error_message.as_ref().is_some_and(|m| m.contains("router")));
    Ok(())
}

#[test]
fn test_advance_connectivity_dns_retries_after_ping() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);

    // DNS fails 5 times then succeeds
    let mut responses = Vec::new();
    for _ in 0..5 {
        responses.push(SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckDnsResolution".to_string()),
            result: OperationResult::DnsFailed("timeout".to_string()),
            consume: true,
        });
    }
    let mut executor = TestExecutor::new(responses);

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}

    // Should succeed — DNS retries until consumed responses are gone, then default Success
    assert!(sm.network_state.is_online());
    Ok(())
}

#[test]
fn test_advance_connectivity_wifi_flow() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("ScanWifiNetworks".to_string()),
            result: OperationResult::WifiScanResults(vec![WifiNetwork {
                ssid: "TestNet".to_string(),
                signal_strength: -45,
                frequency_mhz: 5180,
                security: ttyforce::manifest::WifiSecurity::Wpa2,
                reachable: true,
            }]),
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("AuthenticateWifi".to_string()),
            result: OperationResult::WifiAuthenticated,
            consume: true,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("CheckIpAddress".to_string()),
            result: OperationResult::IpAssigned("10.0.0.5".to_string()),
            consume: false,
        },
    ]);

    // Select wifi
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);

    // Select network and enter password
    sm.process_input(UserInput::SelectWifiNetwork(0), &mut executor);
    sm.process_input(UserInput::WpsDecline, &mut executor);
    sm.process_input(
        UserInput::EnterWifiPassword("pass".to_string()),
        &mut executor,
    );
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);

    // Drive connectivity — wifi starts at Connected, then DHCP, then checks
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
    Ok(())
}

#[test]
fn test_advance_connectivity_no_interface_returns_false() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();
    // Don't select any interface
    assert!(!sm.advance_connectivity(&mut executor));
    Ok(())
}

#[test]
fn test_advance_connectivity_online_returns_false() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());

    // Should return false — already online
    assert!(!sm.advance_connectivity(&mut executor));
    Ok(())
}

// === Root partition protection tests ===

#[test]
fn test_install_never_targets_root_partition() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Drive through full install
    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    sm.process_input(UserInput::ConfirmInstall, &mut executor);

    // Verify no operation targets "/" or the root filesystem
    for entry in &sm.action_manifest.operations {
        match &entry.operation {
            Operation::PartitionDisk { device } => {
                assert!(!device.is_empty(), "device must not be empty");
                assert!(device.starts_with("/dev/"), "device must be a /dev/ path");
            }
            Operation::MountFilesystem { mount_point, .. } => {
                assert_ne!(mount_point, "/", "mount point must never be /");
                assert!(
                    mount_point.starts_with("/town-os") || mount_point.starts_with("/mnt"),
                    "mount point {} is not under /town-os",
                    mount_point
                );
            }
            Operation::InstallBaseSystem { target } => {
                assert_ne!(target, "/", "install target must never be /");
            }
            Operation::GenerateFstab { mount_point, .. } => {
                assert_ne!(mount_point, "/", "fstab must be written inside mount point, not /");
            }
            Operation::PersistNetworkConfig { mount_point, .. } => {
                assert_ne!(mount_point, "/", "network config must never write to /etc");
            }
            _ => {}
        }
    }
    Ok(())
}

#[test]
fn test_default_mount_point_is_town_os() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let sm = InstallerStateMachine::new(hw);
    assert_eq!(sm.mount_point, "/town-os");
    Ok(())
}

#[test]
fn test_default_etc_prefix_is_at_etc_subvolume() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let sm = InstallerStateMachine::new(hw);
    assert_eq!(sm.etc_prefix(), "/town-os/@etc");
    Ok(())
}

#[test]
fn test_custom_etc_prefix_overrides_default() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    sm.etc_prefix = Some("/overlays/etc".to_string());
    assert_eq!(sm.etc_prefix(), "/overlays/etc");
    Ok(())
}

#[test]
fn test_persist_network_config_targets_etc_subvolume() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut executor = success_executor();
    let sm = run_ethernet_single_disk_install(hw, &mut executor);

    // PersistNetworkConfig should target the @etc subvolume, not mount_point/etc/
    let persist_op = sm
        .action_manifest
        .operations
        .iter()
        .find(|op| matches!(&op.operation, Operation::PersistNetworkConfig { .. }));
    let op = persist_op.ok_or("PersistNetworkConfig operation missing from manifest")?;
    if let Operation::PersistNetworkConfig { mount_point, .. } = &op.operation {
        assert_eq!(mount_point, "/town-os/@etc",
            "PersistNetworkConfig should write to @etc subvolume, got: {}", mount_point);
    } else {
        return Err("expected PersistNetworkConfig operation".to_string());
    }
    Ok(())
}

#[test]
fn test_generate_fstab_targets_etc_subvolume() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut executor = success_executor();
    let sm = run_ethernet_single_disk_install(hw, &mut executor);

    let fstab_op = sm
        .action_manifest
        .operations
        .iter()
        .find(|op| matches!(&op.operation, Operation::GenerateFstab { .. }));
    let op = fstab_op.ok_or("GenerateFstab operation missing from manifest")?;
    if let Operation::GenerateFstab { mount_point, .. } = &op.operation {
        assert_eq!(mount_point, "/town-os/@etc",
            "GenerateFstab should write to @etc subvolume, got: {}", mount_point);
    } else {
        return Err("expected GenerateFstab operation".to_string());
    }
    Ok(())
}

#[test]
fn test_cleanup_unmount_after_successful_install() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Full install
    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    sm.process_input(UserInput::ConfirmInstall, &mut executor);

    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Installed);

    // Verify CleanupUnmount was emitted after install
    let op_types: Vec<&str> = sm
        .action_manifest
        .operations
        .iter()
        .map(|op| ttyforce::engine::executor::operation_type_name(&op.operation))
        .collect();

    assert!(
        op_types.contains(&"CleanupUnmount"),
        "CleanupUnmount should run after successful install, got: {:?}",
        op_types
    );

    // The final CleanupUnmount should come after InstallBaseSystem
    let install_pos = op_types.iter().position(|t| *t == "InstallBaseSystem")
        .ok_or("expected InstallBaseSystem in ops")?;
    let unmount_pos = op_types.iter().rposition(|t| *t == "CleanupUnmount")
        .ok_or("expected CleanupUnmount in ops")?;
    assert!(
        unmount_pos > install_pos,
        "final CleanupUnmount should come after InstallBaseSystem"
    );
    Ok(())
}

#[test]
fn test_generate_fstab_in_install_flow() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    sm.process_input(UserInput::ConfirmInstall, &mut executor);

    let op_types: Vec<&str> = sm
        .action_manifest
        .operations
        .iter()
        .map(|op| ttyforce::engine::executor::operation_type_name(&op.operation))
        .collect();

    assert!(
        op_types.contains(&"GenerateFstab"),
        "GenerateFstab should be emitted during install, got: {:?}",
        op_types
    );
    Ok(())
}

#[test]
fn test_btrfs_device_scan_before_mount() -> Result<(), String> {
    // The mount_filesystem function in real_ops runs btrfs device scan
    // for btrfs. Verify the MountFilesystem operation is present and
    // uses btrfs as fs_type.
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    sm.process_input(UserInput::ConfirmInstall, &mut executor);

    let mount_ops: Vec<_> = sm
        .action_manifest
        .operations
        .iter()
        .filter(|op| matches!(&op.operation, Operation::MountFilesystem { .. }))
        .collect();

    assert!(!mount_ops.is_empty(), "MountFilesystem should be emitted");
    for op in &mount_ops {
        if let Operation::MountFilesystem { fs_type, .. } = &op.operation {
            assert_eq!(fs_type, "btrfs", "fs_type should be btrfs");
        }
    }
    Ok(())
}

#[test]
fn test_subvolumes_are_etc_and_var() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    sm.process_input(UserInput::ConfirmInstall, &mut executor);

    let subvol_names: Vec<String> = sm
        .action_manifest
        .operations
        .iter()
        .filter_map(|op| {
            if let Operation::CreateBtrfsSubvolume { name, .. } = &op.operation {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    assert_eq!(subvol_names, vec!["@etc", "@var"]);
    Ok(())
}

// === WPS Push Button Tests ===

fn wps_executor() -> TestExecutor {
    TestExecutor::new(vec![
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("WpsPbcStart".to_string()),
            result: OperationResult::Success,
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("WpsPbcStatus".to_string()),
            result: OperationResult::WpsCompleted,
            consume: false,
        },
    ])
}

#[test]
fn test_wps_start_from_wifi_select() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = wps_executor();

    // Auto-detect goes to WifiSelect
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);

    // Initiate WPS
    sm.process_input(UserInput::InitiateWps, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WpsWaiting);
    assert_eq!(sm.network_state, NetworkState::WpsWaiting);
    assert!(sm.wps_start_time.is_some());
    Ok(())
}

#[test]
fn test_wps_completed_advances_to_network_progress() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = wps_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::InitiateWps, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WpsWaiting);

    // Poll — executor returns WpsCompleted
    sm.advance_connectivity(&mut executor);
    assert_eq!(sm.network_state, NetworkState::Connected);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    Ok(())
}

#[test]
fn test_wps_cancel_returns_to_wifi_select() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = wps_executor();

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::InitiateWps, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WpsWaiting);

    // Cancel WPS
    sm.process_input(UserInput::Back, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::WifiSelect);
    assert!(sm.wps_start_time.is_none());

    // CleanupWpaSupplicant should have been executed
    let has_cleanup = executor
        .recorded_operations()
        .iter()
        .any(|r| matches!(&r.operation, Operation::CleanupWpaSupplicant { .. }));
    assert!(has_cleanup, "WPS cancel should clean up wpa_supplicant");
    Ok(())
}

#[test]
fn test_wps_pending_keeps_polling() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("WpsPbcStart".to_string()),
            result: OperationResult::Success,
            consume: false,
        },
        SimulatedResponse {
            operation_match: OperationMatcher::ByType("WpsPbcStatus".to_string()),
            result: OperationResult::WpsPending,
            consume: false,
        },
    ]);

    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::InitiateWps, &mut executor);

    // Poll — should stay on WpsWaiting
    let should_continue = sm.advance_connectivity(&mut executor);
    assert!(should_continue);
    assert_eq!(sm.network_state, NetworkState::WpsWaiting);
    assert_eq!(sm.current_screen, ScreenId::WpsWaiting);
    Ok(())
}

#[test]
fn test_wps_full_install_flow() -> Result<(), String> {
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = wps_executor();

    // Network setup via WPS
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::InitiateWps, &mut executor);
    sm.advance_connectivity(&mut executor); // WpsCompleted -> Connected

    // Drive connectivity checks to completion
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());

    // Disk setup
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    sm.process_input(UserInput::ConfirmInstall, &mut executor);

    assert_eq!(
        sm.action_manifest.final_state,
        InstallerFinalState::Installed
    );

    // Verify WPS operations in manifest
    let op_types: Vec<&str> = sm
        .action_manifest
        .operations
        .iter()
        .map(|op| ttyforce::engine::executor::operation_type_name(&op.operation))
        .collect();
    assert!(op_types.contains(&"WpsPbcStart"), "manifest should contain WpsPbcStart");
    assert!(op_types.contains(&"WpsPbcStatus"), "manifest should contain WpsPbcStatus");
    Ok(())
}

// === Network-Only Mode (Reconfigure) ===

#[test]
fn test_network_only_returns_to_reboot_screen_on_completion() -> Result<(), String> {
    let hw = load_hardware("ethernet_4disk_same")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.network_only = true;

    // Auto-detect network (connected ethernet)
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);

    // Advance connectivity until online
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());

    // Confirm network config — in network_only mode this should persist
    // config and transition to Reboot (signaling completion)
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(
        sm.current_screen,
        ScreenId::Reboot,
        "network_only mode must set current_screen to Reboot after persisting config"
    );

    Ok(())
}

#[test]
fn test_network_only_abort_returns_to_reboot_screen() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    sm.network_only = true;

    // Abort from the initial network config screen
    sm.process_input(UserInput::AbortInstall, &mut executor);
    assert_eq!(
        sm.current_screen,
        ScreenId::Reboot,
        "aborting in network_only mode must reach Reboot screen"
    );

    Ok(())
}

// === Install Mode Select ===

#[test]
fn test_new_with_mode_select_starts_on_install_mode_screen() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let sm = InstallerStateMachine::new_with_mode_select(hw);
    assert_eq!(sm.current_screen, ScreenId::InstallModeSelect);
    assert_eq!(sm.install_mode, InstallMode::Advanced);
    Ok(())
}

#[test]
fn test_new_preserves_legacy_entry_screen() -> Result<(), String> {
    // Plain `new()` must keep starting on NetworkConfig so existing tests and
    // reconfigure flows don't see the mode-select screen.
    let hw = load_hardware("ethernet_1disk")?;
    let sm = InstallerStateMachine::new(hw);
    assert_eq!(sm.current_screen, ScreenId::NetworkConfig);
    assert_eq!(sm.install_mode, InstallMode::Advanced);
    Ok(())
}

#[test]
fn test_install_mode_advanced_leads_to_network_config() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Advanced),
        &mut executor,
    );
    assert_eq!(sm.current_screen, ScreenId::NetworkConfig);
    assert_eq!(sm.install_mode, InstallMode::Advanced);
    // No network action should have happened yet — the user has to press
    // enter on NetworkConfig to kick off detection.
    assert!(sm.selected_interface.is_none());
    Ok(())
}

#[test]
fn test_install_mode_select_via_index_maps_to_enum() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    // Index 1 = Advanced (matches list order in the renderer).
    sm.process_input(UserInput::Select(1), &mut executor);
    assert_eq!(sm.install_mode, InstallMode::Advanced);
    assert_eq!(sm.current_screen, ScreenId::NetworkConfig);
    Ok(())
}

#[test]
fn test_install_mode_select_invalid_index_errors() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    let before = sm.current_screen.clone();
    sm.process_input(UserInput::Select(99), &mut executor);
    assert_eq!(sm.current_screen, before);
    assert!(sm.error_message.is_some());
    Ok(())
}

#[test]
fn test_install_mode_easy_with_carrier_auto_picks_ethernet() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );

    // Carrier present -> jumps straight to NetworkProgress with the wired
    // interface selected.
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    assert_eq!(sm.selected_interface, Some("eth0".to_string()));
    Ok(())
}

#[test]
fn test_install_mode_easy_no_ethernet_hardware_drops_to_network_config() -> Result<(), String> {
    // No ethernet hardware at all — Easy mode has nothing to poll and
    // drops the user straight to NetworkConfig. Wifi must not be
    // auto-selected in Easy mode.
    let hw = load_hardware("wifi_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );

    assert_eq!(sm.current_screen, ScreenId::NetworkConfig);
    assert!(sm.selected_interface.is_none());
    assert!(sm.carrier_candidates.is_empty());
    assert!(sm.carrier_wait_start.is_none());
    assert!(
        sm.error_message
            .as_deref()
            .is_some_and(|m| m.contains("ethernet"))
    );
    Ok(())
}

#[test]
fn test_install_mode_easy_dead_ethernet_enters_carrier_wait() -> Result<(), String> {
    // Ethernet is unplugged — Easy mode should land on NetworkProgress
    // in WaitingForCarrier state, polling the interface rather than
    // immediately giving up.
    let hw = load_hardware("wifi_dead_ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );

    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    assert_eq!(sm.network_state, NetworkState::WaitingForCarrier);
    assert_eq!(sm.carrier_candidates, vec!["eth0".to_string()]);
    assert!(sm.carrier_wait_start.is_some());
    // Wifi must NOT have been picked.
    assert!(sm.selected_interface.is_none());
    // The interface should have been brought up.
    let enabled_ops = executor
        .recorded_operations()
        .iter()
        .filter(|r| {
            matches!(
                &r.operation,
                Operation::EnableInterface { interface } if interface == "eth0"
            )
        })
        .count();
    assert_eq!(enabled_ops, 1);
    Ok(())
}

#[test]
fn test_install_mode_easy_carrier_wait_picks_first_live_ethernet() -> Result<(), String> {
    // Dead ethernet at start — executor returns success for
    // CheckLinkAvailability, simulating a cable plugged in mid-wait.
    // advance_connectivity should commit the interface and jump to DHCP.
    let hw = load_hardware("wifi_dead_ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );
    assert_eq!(sm.network_state, NetworkState::WaitingForCarrier);

    // One tick — success_executor() returns Success for every op, so the
    // first poll commits eth0.
    sm.advance_connectivity(&mut executor);
    assert_eq!(sm.selected_interface, Some("eth0".to_string()));
    assert_eq!(sm.network_state, NetworkState::DhcpConfiguring);
    assert!(sm.carrier_candidates.is_empty());
    assert!(sm.carrier_wait_start.is_none());

    // Finish the bring-up and make sure it goes online cleanly.
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());
    Ok(())
}

#[test]
fn test_install_mode_easy_carrier_wait_times_out_to_network_config() -> Result<(), String> {
    // Ethernet never gets carrier — after the 30s wait window elapses,
    // Easy mode should drop the user on NetworkConfig.
    let hw = load_hardware("wifi_dead_ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("CheckLinkAvailability".to_string()),
        result: OperationResult::Error("no carrier".to_string()),
        consume: false,
    }]);

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );
    assert_eq!(sm.network_state, NetworkState::WaitingForCarrier);

    // Fast-forward the deadline past the 30s window.
    sm.carrier_wait_start =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(31));
    sm.advance_connectivity(&mut executor);

    assert_eq!(sm.current_screen, ScreenId::NetworkConfig);
    assert_eq!(sm.network_state, NetworkState::Offline);
    assert!(sm.carrier_candidates.is_empty());
    assert!(sm.carrier_wait_start.is_none());
    assert!(sm.selected_interface.is_none());
    assert!(
        sm.error_message
            .as_deref()
            .is_some_and(|m| m.contains("wired"))
    );
    // Poll candidates were brought down so the user's manual pick
    // isn't racing them.
    let shutdown_ops = executor
        .recorded_operations()
        .iter()
        .filter(|r| {
            matches!(
                &r.operation,
                Operation::ShutdownInterface { interface } if interface == "eth0"
            )
        })
        .count();
    assert_eq!(shutdown_ops, 1);
    Ok(())
}

#[test]
fn test_install_mode_easy_polls_every_ethernet_candidate() -> Result<(), String> {
    // Two ethernet interfaces present, both starting without carrier.
    // Easy mode should enable both and poll both.
    let mut hw = load_hardware("wifi_dead_ethernet_1disk")?;
    hw.network.interfaces.push(ttyforce::manifest::NetworkInterfaceSpec {
        name: "eth1".to_string(),
        kind: ttyforce::manifest::InterfaceKind::Ethernet,
        mac: "aa:bb:cc:11:22:33".to_string(),
        has_link: false,
        has_carrier: false,
    });
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = TestExecutor::new(vec![SimulatedResponse {
        operation_match: OperationMatcher::ByType("CheckLinkAvailability".to_string()),
        result: OperationResult::Error("no carrier".to_string()),
        consume: false,
    }]);

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );
    assert_eq!(sm.network_state, NetworkState::WaitingForCarrier);
    assert_eq!(sm.carrier_candidates, vec!["eth0".to_string(), "eth1".to_string()]);

    // One tick polls both candidates.
    sm.advance_connectivity(&mut executor);
    let link_checks: Vec<&str> = executor
        .recorded_operations()
        .iter()
        .filter_map(|r| match &r.operation {
            Operation::CheckLinkAvailability { interface } => Some(interface.as_str()),
            _ => None,
        })
        .collect();
    assert!(link_checks.contains(&"eth0"));
    assert!(link_checks.contains(&"eth1"));
    Ok(())
}

#[test]
fn test_install_mode_easy_prefers_ethernet_over_wifi_when_both_present() -> Result<(), String> {
    // wifi_ethernet_1disk has both with ethernet carrier live.
    let hw = load_hardware("wifi_ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );

    assert_eq!(sm.selected_interface, Some("eth0".to_string()));
    assert_eq!(sm.current_screen, ScreenId::NetworkProgress);
    Ok(())
}

#[test]
fn test_install_mode_easy_picks_most_redundant_raid_4disks() -> Result<(), String> {
    let hw = load_hardware("ethernet_4disk_same")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );
    // Bring network online
    while sm.advance_connectivity(&mut executor) {}
    assert!(sm.network_state.is_online());

    // Confirm on NetworkProgress triggers the easy-mode disk defaults and
    // jumps straight past RaidConfig / DiskGroupSelect.
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);
    assert_eq!(sm.selected_raid, Some(RaidConfig::BtrfsRaid5));
    assert_eq!(sm.selected_disk_group, Some(0));
    assert!(sm.selected_disk.is_none());
    Ok(())
}

#[test]
fn test_install_mode_easy_picks_single_for_one_disk() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);
    assert_eq!(sm.selected_raid, Some(RaidConfig::Single));
    // Single mode picks an explicit disk, not a group.
    assert_eq!(sm.selected_disk, Some(0));
    assert!(sm.selected_disk_group.is_none());
    Ok(())
}

#[test]
fn test_apply_easy_disk_defaults_two_disks_picks_mirror() -> Result<(), String> {
    let hw = load_hardware("ethernet_4disk_same")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);

    // Shrink to two disks to exercise the 2-disk -> mirror branch.
    sm.all_disks.truncate(2);
    sm.disk_groups = ttyforce::disk::DiskGroup::from_disks(&sm.all_disks);

    assert!(sm.apply_easy_disk_defaults());
    assert_eq!(sm.selected_raid, Some(RaidConfig::BtrfsRaid1));
    assert_eq!(sm.selected_disk_group, Some(0));
    Ok(())
}

#[test]
fn test_install_mode_easy_full_install_flow() -> Result<(), String> {
    let hw = load_hardware("ethernet_4disk_same")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    // Step through the happy path: easy -> network online -> confirm ->
    // install -> reboot.
    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);

    sm.process_input(UserInput::ConfirmInstall, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::InstallProgress);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Installed);

    // RAID5 on 4 identical disks means BtrfsRaidSetup should have been used.
    let ops = executor.recorded_operations();
    let has_raid_setup = ops.iter().any(|r| {
        matches!(
            &r.operation,
            Operation::BtrfsRaidSetup { raid_level, .. } if raid_level == "raid5"
        )
    });
    assert!(
        has_raid_setup,
        "easy mode on 4 disks must run BtrfsRaidSetup at raid5"
    );
    Ok(())
}

#[test]
fn test_install_mode_easy_back_from_confirm_returns_to_mode_select() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Easy),
        &mut executor,
    );
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);

    sm.process_input(UserInput::Back, &mut executor);
    // Easy mode skipped raid/disk screens, so back goes all the way to mode
    // select, clearing the auto-picked values.
    assert_eq!(sm.current_screen, ScreenId::InstallModeSelect);
    assert!(sm.selected_raid.is_none());
    assert!(sm.selected_disk.is_none());
    assert!(sm.selected_disk_group.is_none());
    Ok(())
}

#[test]
fn test_install_mode_advanced_back_from_confirm_goes_to_disk_select() -> Result<(), String> {
    // Advanced mode: Back from Confirm must NOT jump to mode-select; it
    // should step back one screen to DiskGroupSelect like the legacy flow.
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Advanced),
        &mut executor,
    );
    sm.process_input(UserInput::Confirm, &mut executor);
    while sm.advance_connectivity(&mut executor) {}
    sm.process_input(UserInput::Confirm, &mut executor);
    sm.process_input(UserInput::SelectRaidOption(0), &mut executor);
    sm.process_input(UserInput::SelectDiskGroup(0), &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);

    sm.process_input(UserInput::Back, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::DiskGroupSelect);
    Ok(())
}

#[test]
fn test_install_mode_select_back_from_network_config() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(
        UserInput::SelectInstallMode(InstallMode::Advanced),
        &mut executor,
    );
    assert_eq!(sm.current_screen, ScreenId::NetworkConfig);

    sm.process_input(UserInput::Back, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::InstallModeSelect);
    Ok(())
}

#[test]
fn test_install_mode_select_abort() -> Result<(), String> {
    let hw = load_hardware("ethernet_1disk")?;
    let mut sm = InstallerStateMachine::new_with_mode_select(hw);
    let mut executor = success_executor();

    sm.process_input(UserInput::AbortInstall, &mut executor);
    assert_eq!(sm.current_screen, ScreenId::Reboot);
    assert_eq!(sm.action_manifest.final_state, InstallerFinalState::Aborted);
    Ok(())
}
