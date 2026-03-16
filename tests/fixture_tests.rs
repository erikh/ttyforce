use ttyforce::disk::{DiskGroup, DiskInfo, FilesystemType, RaidConfig};
use ttyforce::engine::executor::TestExecutor;
use ttyforce::engine::state_machine::{InstallerStateMachine, ScreenId, UserInput};
use ttyforce::manifest::{HardwareManifest, InterfaceKind};

fn load_hardware(name: &str) -> HardwareManifest {
    HardwareManifest::load(&format!("fixtures/hardware/{}.toml", name)).unwrap()
}

// === Hardware Loading Tests ===

#[test]
fn test_load_ethernet_4disk_same() {
    let hw = load_hardware("ethernet_4disk_same");
    assert_eq!(hw.network.interfaces.len(), 1);
    assert_eq!(hw.network.interfaces[0].kind, InterfaceKind::Ethernet);
    assert!(hw.network.interfaces[0].has_link);
    assert!(hw.network.interfaces[0].has_carrier);
    assert_eq!(hw.disks.len(), 4);
    // All same make/model
    assert!(hw.disks.iter().all(|d| d.make == "Samsung" && d.model == "870 EVO"));
}

#[test]
fn test_load_ethernet_1disk() {
    let hw = load_hardware("ethernet_1disk");
    assert_eq!(hw.network.interfaces.len(), 1);
    assert_eq!(hw.disks.len(), 1);
    assert_eq!(hw.disks[0].make, "Western Digital");
}

#[test]
fn test_load_wifi_1disk() {
    let hw = load_hardware("wifi_1disk");
    assert_eq!(hw.network.interfaces.len(), 1);
    assert_eq!(hw.network.interfaces[0].kind, InterfaceKind::Wifi);
    assert!(hw.network.wifi_environment.is_some());
    let wifi = hw.network.wifi_environment.as_ref().unwrap();
    assert_eq!(wifi.available_networks.len(), 2);
    assert_eq!(hw.disks.len(), 1);
}

#[test]
fn test_load_wifi_crowded_1disk() {
    let hw = load_hardware("wifi_crowded_1disk");
    let wifi = hw.network.wifi_environment.as_ref().unwrap();
    assert_eq!(wifi.available_networks.len(), 10);
    // Verify some are unreachable
    let unreachable_count = wifi.available_networks.iter().filter(|n| !n.reachable).count();
    assert!(unreachable_count >= 2);
}

#[test]
fn test_load_wifi_ethernet_4disk() {
    let hw = load_hardware("wifi_ethernet_4disk");
    assert_eq!(hw.network.interfaces.len(), 2);
    let eth = hw.ethernet_interfaces();
    let wifi = hw.wifi_interfaces();
    assert_eq!(eth.len(), 1);
    assert_eq!(wifi.len(), 1);
    assert!(eth[0].has_link);
    assert_eq!(hw.disks.len(), 4);
}

#[test]
fn test_load_wifi_ethernet_1disk() {
    let hw = load_hardware("wifi_ethernet_1disk");
    assert_eq!(hw.network.interfaces.len(), 2);
    assert_eq!(hw.disks.len(), 1);
    let connected = hw.connected_ethernet();
    assert_eq!(connected.len(), 1);
}

#[test]
fn test_load_wifi_dead_ethernet_1disk() {
    let hw = load_hardware("wifi_dead_ethernet_1disk");
    assert_eq!(hw.network.interfaces.len(), 2);
    let connected = hw.connected_ethernet();
    assert_eq!(connected.len(), 0); // dead ethernet
    let wifi = hw.wifi_interfaces();
    assert_eq!(wifi.len(), 1);
}

#[test]
fn test_load_wifi_dead_ethernet_4disk() {
    let hw = load_hardware("wifi_dead_ethernet_4disk");
    let connected = hw.connected_ethernet();
    assert_eq!(connected.len(), 0);
    assert_eq!(hw.disks.len(), 4);
}

// === Disk Grouping Tests ===

#[test]
fn test_disk_grouping_all_same() {
    let hw = load_hardware("ethernet_4disk_same");
    let disks: Vec<DiskInfo> = hw.disks.iter().map(DiskInfo::from).collect();
    let groups = DiskGroup::from_disks(&disks);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].disk_count(), 4);
}

#[test]
fn test_disk_grouping_single() {
    let hw = load_hardware("ethernet_1disk");
    let disks: Vec<DiskInfo> = hw.disks.iter().map(DiskInfo::from).collect();
    let groups = DiskGroup::from_disks(&disks);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].disk_count(), 1);
}

// === RAID Option Tests ===

#[test]
fn test_raid_options_1disk_btrfs() {
    let options = RaidConfig::for_disk_count(1, &FilesystemType::Btrfs);
    assert_eq!(options.len(), 1);
    assert_eq!(options[0], RaidConfig::Single);
}

#[test]
fn test_raid_options_2disk_btrfs() {
    let options = RaidConfig::for_disk_count(2, &FilesystemType::Btrfs);
    assert_eq!(options.len(), 2);
    assert!(options.contains(&RaidConfig::Single));
    assert!(options.contains(&RaidConfig::BtrfsRaid1));
}

#[test]
fn test_raid_options_4disk_btrfs() {
    let options = RaidConfig::for_disk_count(4, &FilesystemType::Btrfs);
    assert_eq!(options.len(), 3);
    assert!(options.contains(&RaidConfig::Single));
    assert!(options.contains(&RaidConfig::BtrfsRaid1));
    assert!(options.contains(&RaidConfig::BtrfsRaid5));
}

#[test]
fn test_raid_options_1disk_zfs() {
    let options = RaidConfig::for_disk_count(1, &FilesystemType::Zfs);
    assert_eq!(options.len(), 1);
    assert_eq!(options[0], RaidConfig::Single);
}

#[test]
fn test_raid_options_2disk_zfs() {
    let options = RaidConfig::for_disk_count(2, &FilesystemType::Zfs);
    assert_eq!(options.len(), 2);
    assert!(options.contains(&RaidConfig::Single));
    assert!(options.contains(&RaidConfig::Mirror));
}

#[test]
fn test_raid_options_4disk_zfs() {
    let options = RaidConfig::for_disk_count(4, &FilesystemType::Zfs);
    assert_eq!(options.len(), 3);
    assert!(options.contains(&RaidConfig::Single));
    assert!(options.contains(&RaidConfig::Mirror));
    assert!(options.contains(&RaidConfig::RaidZ));
}

#[test]
fn test_raid_recommended() {
    assert_eq!(
        RaidConfig::recommended_for_count(1, &FilesystemType::Btrfs),
        RaidConfig::Single
    );
    assert_eq!(
        RaidConfig::recommended_for_count(2, &FilesystemType::Btrfs),
        RaidConfig::BtrfsRaid1
    );
    assert_eq!(
        RaidConfig::recommended_for_count(4, &FilesystemType::Btrfs),
        RaidConfig::BtrfsRaid5
    );
    assert_eq!(
        RaidConfig::recommended_for_count(2, &FilesystemType::Zfs),
        RaidConfig::Mirror
    );
    assert_eq!(
        RaidConfig::recommended_for_count(4, &FilesystemType::Zfs),
        RaidConfig::RaidZ
    );
}

#[test]
fn test_raid_usable_capacity() {
    let total = 4_000_000_000_000u64; // 4TB total
    assert_eq!(RaidConfig::Single.usable_capacity(total, 4), total);
    assert_eq!(RaidConfig::Mirror.usable_capacity(total, 2), total / 2);
    assert_eq!(RaidConfig::RaidZ.usable_capacity(total, 4), total * 3 / 4);
    assert_eq!(RaidConfig::BtrfsRaid1.usable_capacity(total, 2), total / 2);
    assert_eq!(RaidConfig::BtrfsRaid5.usable_capacity(total, 4), total * 3 / 4);
}

// === Initial State Tests ===

#[test]
fn test_initial_state_ethernet() {
    let hw = load_hardware("ethernet_4disk_same");
    let sm = InstallerStateMachine::new(hw);
    assert_eq!(sm.current_screen, ScreenId::NetworkConfig);
    assert_eq!(sm.interfaces.len(), 1);
    assert_eq!(sm.disk_groups.len(), 1);
    assert_eq!(sm.disk_groups[0].disk_count(), 4);
}

#[test]
fn test_initial_state_wifi() {
    let hw = load_hardware("wifi_1disk");
    let sm = InstallerStateMachine::new(hw);
    assert_eq!(sm.current_screen, ScreenId::NetworkConfig);
    assert_eq!(sm.interfaces.len(), 1);
    assert_eq!(sm.wifi_networks.len(), 2);
    assert_eq!(sm.disk_groups.len(), 1);
}

#[test]
fn test_initial_state_mixed() {
    let hw = load_hardware("wifi_ethernet_4disk");
    let sm = InstallerStateMachine::new(hw);
    assert_eq!(sm.interfaces.len(), 2);
    assert_eq!(sm.disk_groups.len(), 1); // all same make/model
}

// === Network State Machine Tests ===

#[test]
fn test_ethernet_auto_detect() {
    let hw = load_hardware("ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![]);

    let result = sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(result, Some(ScreenId::NetworkProgress));
    assert!(sm.network_state.is_online());
    assert_eq!(sm.selected_interface, Some("eth0".to_string()));
}

#[test]
fn test_wifi_auto_detect_goes_to_wifi_select() {
    let hw = load_hardware("wifi_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![]);

    let result = sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(result, Some(ScreenId::WifiSelect));
    assert_eq!(sm.selected_interface, Some("wlan0".to_string()));
}

#[test]
fn test_ethernet_preferred_over_wifi() {
    let hw = load_hardware("wifi_ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![]);

    let result = sm.process_input(UserInput::Confirm, &mut executor);
    assert_eq!(result, Some(ScreenId::NetworkProgress));
    assert_eq!(sm.selected_interface, Some("eth0".to_string()));
    assert!(sm.network_state.is_online());
}

#[test]
fn test_dead_ethernet_falls_back_to_wifi() {
    let hw = load_hardware("wifi_dead_ethernet_1disk");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = TestExecutor::new(vec![]);

    let result = sm.process_input(UserInput::Confirm, &mut executor);
    // Should go to wifi select since ethernet is dead
    assert_eq!(result, Some(ScreenId::WifiSelect));
    assert_eq!(sm.selected_interface, Some("wlan0".to_string()));
}

// === Filesystem Tests ===

#[test]
fn test_filesystem_default_is_btrfs() {
    assert_eq!(FilesystemType::default(), FilesystemType::Btrfs);
    assert!(FilesystemType::Btrfs.is_default());
    assert!(!FilesystemType::Zfs.is_default());
}

#[test]
fn test_disk_size_display() {
    let hw = load_hardware("ethernet_4disk_same");
    let disk = &hw.disks[0];
    let human = disk.size_human();
    assert!(human.contains("GB") || human.contains("TB"));
}
