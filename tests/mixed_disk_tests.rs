use ttyforce::disk::{DiskGroup, DiskInfo, RaidConfig};
use ttyforce::engine::executor::TestExecutor;
use ttyforce::engine::state_machine::{InstallerStateMachine, ScreenId, UserInput};
use ttyforce::engine::OperationExecutor;
use ttyforce::manifest::{HardwareManifest, InstallerFinalState, OperationOutcome};
use ttyforce::operations::Operation;

fn load_hardware(name: &str) -> HardwareManifest {
    HardwareManifest::load(&format!("fixtures/hardware/{}.toml", name)).unwrap()
}

fn success_executor() -> TestExecutor {
    TestExecutor::new(vec![])
}

/// Drive the state machine through network, raid, disk group, confirm, install
fn run_install(
    sm: &mut InstallerStateMachine,
    executor: &mut TestExecutor,
    raid_idx: usize,
    disk_group_idx: usize,
) {
    // Network auto-detect (connected ethernet goes straight to RaidConfig)
    sm.process_input(UserInput::Confirm, executor);
    assert_eq!(sm.current_screen, ScreenId::RaidConfig);

    // RAID
    sm.process_input(UserInput::SelectRaidOption(raid_idx), executor);
    assert_eq!(sm.current_screen, ScreenId::DiskGroupSelect);

    // Disk group
    sm.process_input(UserInput::SelectDiskGroup(disk_group_idx), executor);
    assert_eq!(sm.current_screen, ScreenId::Confirm);

    // Install
    sm.process_input(UserInput::ConfirmInstall, executor);
    assert_eq!(sm.current_screen, ScreenId::InstallProgress);
    assert_eq!(
        sm.action_manifest.final_state,
        InstallerFinalState::Installed
    );
}

// =====================================================
// Workstation: 3 groups
//   0: Crucial MX500 (4x 500GB SATA SSD)
//   1: Samsung 990 PRO (2x 1TB NVMe)
//   2: Western Digital Red Plus (2x 4TB HDD)
// =====================================================

#[test]
fn test_workstation_disk_grouping() {
    let hw = load_hardware("mixed_drives_workstation");
    let disks: Vec<DiskInfo> = hw.disks.iter().map(DiskInfo::from).collect();
    let groups = DiskGroup::from_disks(&disks);

    assert_eq!(groups.len(), 3);

    // BTreeMap sorts by (make, model), so:
    // Crucial MX500, Samsung 990 PRO, Western Digital Red Plus
    assert_eq!(groups[0].make, "Crucial");
    assert_eq!(groups[0].model, "MX500");
    assert_eq!(groups[0].disk_count(), 4);

    assert_eq!(groups[1].make, "Samsung");
    assert_eq!(groups[1].model, "990 PRO");
    assert_eq!(groups[1].disk_count(), 2);

    assert_eq!(groups[2].make, "Western Digital");
    assert_eq!(groups[2].model, "Red Plus");
    assert_eq!(groups[2].disk_count(), 2);
}

#[test]
fn test_workstation_select_crucial_btrfs_raid5() {
    let hw = load_hardware("mixed_drives_workstation");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // RAID5 (2: Single/RAID1/RAID5 for max 4 disks), Crucial group (0)
    run_install(&mut sm, &mut executor, 2, 0);

    let ops = executor.recorded_operations();
    // Should partition 4 Crucial drives
    let partitions: Vec<_> = ops
        .iter()
        .filter_map(|r| match &r.operation {
            Operation::PartitionDisk { device } => Some(device.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(partitions, vec!["/dev/sda", "/dev/sdb", "/dev/sdc", "/dev/sdd"]);

    // Should have btrfs raid setup
    let has_btrfs_raid = ops.iter().any(|r| match &r.operation {
        Operation::BtrfsRaidSetup { devices, raid_level } => {
            devices.len() == 4 && raid_level == "raid5"
        }
        _ => false,
    });
    assert!(has_btrfs_raid);
}

#[test]
fn test_workstation_select_samsung_btrfs_mirror() {
    let hw = load_hardware("mixed_drives_workstation");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // RAID1 (1: Single/RAID1/RAID5 for max 4), Samsung group
    // Samsung is group index 1 in disk_groups, but in compatible_disk_groups
    // for RAID1 (needs >=2), all 3 groups qualify, so compatible index 1 = Samsung
    run_install(&mut sm, &mut executor, 1, 1);

    let ops = executor.recorded_operations();
    let partitions: Vec<_> = ops
        .iter()
        .filter_map(|r| match &r.operation {
            Operation::PartitionDisk { device } => Some(device.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(partitions, vec!["/dev/nvme0n1", "/dev/nvme1n1"]);

    let has_btrfs_raid = ops.iter().any(|r| match &r.operation {
        Operation::BtrfsRaidSetup { devices, .. } => devices.len() == 2,
        _ => false,
    });
    assert!(has_btrfs_raid);
}

#[test]
fn test_workstation_select_wd_btrfs_mirror() {
    let hw = load_hardware("mixed_drives_workstation");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // RAID1 (1), WD group (compatible index 2)
    run_install(&mut sm, &mut executor, 1, 2);

    let ops = executor.recorded_operations();
    let partitions: Vec<_> = ops
        .iter()
        .filter_map(|r| match &r.operation {
            Operation::PartitionDisk { device } => Some(device.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(partitions, vec!["/dev/sde", "/dev/sdf"]);

    let has_btrfs_raid = ops.iter().any(|r| match &r.operation {
        Operation::BtrfsRaidSetup { devices, raid_level } => {
            devices.len() == 2 && raid_level == "raid1"
        }
        _ => false,
    });
    assert!(has_btrfs_raid);
}

#[test]
fn test_workstation_select_crucial_single() {
    let hw = load_hardware("mixed_drives_workstation");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Single (0), disk index 2 = /dev/sda (first Crucial MX500)
    // Workstation all_disks order: nvme0n1, nvme1n1, sda, sdb, sdc, sdd, sde, sdf
    run_install(&mut sm, &mut executor, 0, 2);

    let ops = executor.recorded_operations();
    // Single mode: only 1 disk
    let has_mkfs = ops.iter().any(|r| match &r.operation {
        Operation::MkfsBtrfs { devices } => devices == &vec!["/dev/sda".to_string()],
        _ => false,
    });
    assert!(has_mkfs);

    let has_subvol = ops
        .iter()
        .filter(|r| matches!(&r.operation, Operation::CreateBtrfsSubvolume { .. }))
        .count();
    assert_eq!(has_subvol, 3); // @, @home, @snapshots
}

// =====================================================
// Server: 2 groups
//   0: Intel Optane P5800X (2x 400GB NVMe)
//   1: Seagate Exos X18 (6x 16TB HDD)
// =====================================================

#[test]
fn test_server_disk_grouping() {
    let hw = load_hardware("mixed_drives_server");
    let disks: Vec<DiskInfo> = hw.disks.iter().map(DiskInfo::from).collect();
    let groups = DiskGroup::from_disks(&disks);

    assert_eq!(groups.len(), 2);

    assert_eq!(groups[0].make, "Intel");
    assert_eq!(groups[0].model, "Optane P5800X");
    assert_eq!(groups[0].disk_count(), 2);

    assert_eq!(groups[1].make, "Seagate");
    assert_eq!(groups[1].model, "Exos X18");
    assert_eq!(groups[1].disk_count(), 6);
}

#[test]
fn test_server_select_seagate_btrfs_raid5() {
    let hw = load_hardware("mixed_drives_server");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // RAID5 (2: Single/RAID1/RAID5 for max 6), Seagate group
    // Compatible groups for RAID5 (needs >=3): only Seagate (6 disks), so index 0
    run_install(&mut sm, &mut executor, 2, 0);

    let ops = executor.recorded_operations();
    let partitions: Vec<_> = ops
        .iter()
        .filter_map(|r| match &r.operation {
            Operation::PartitionDisk { device } => Some(device.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        partitions,
        vec!["/dev/sda", "/dev/sdb", "/dev/sdc", "/dev/sdd", "/dev/sde", "/dev/sdf"]
    );

    let has_btrfs_raid = ops.iter().any(|r| match &r.operation {
        Operation::BtrfsRaidSetup {
            devices,
            raid_level,
        } => devices.len() == 6 && raid_level == "raid5",
        _ => false,
    });
    assert!(has_btrfs_raid);

    let subvol_count = ops
        .iter()
        .filter(|r| matches!(&r.operation, Operation::CreateBtrfsSubvolume { .. }))
        .count();
    assert_eq!(subvol_count, 3); // @, @home, @snapshots
}

#[test]
fn test_server_select_intel_btrfs_mirror() {
    let hw = load_hardware("mixed_drives_server");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // RAID1 (1), Intel group
    // Compatible for RAID1 (needs >=2): Intel (2) index 0, Seagate (6) index 1
    run_install(&mut sm, &mut executor, 1, 0);

    let ops = executor.recorded_operations();
    let partitions: Vec<_> = ops
        .iter()
        .filter_map(|r| match &r.operation {
            Operation::PartitionDisk { device } => Some(device.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(partitions, vec!["/dev/nvme0n1", "/dev/nvme1n1"]);

    let has_btrfs_raid = ops.iter().any(|r| match &r.operation {
        Operation::BtrfsRaidSetup {
            devices,
            raid_level,
        } => devices.len() == 2 && raid_level == "raid1",
        _ => false,
    });
    assert!(has_btrfs_raid);
}

#[test]
fn test_server_raidz_filters_small_groups() {
    let hw = load_hardware("mixed_drives_server");
    let sm = InstallerStateMachine::new(hw);

    // For BtrfsRaid5 (needs >=3), Intel (2 disks) should be filtered out
    let mut sm_copy = sm;
    sm_copy.selected_raid = Some(RaidConfig::BtrfsRaid5);
    let compatible = sm_copy.compatible_disk_groups();
    assert_eq!(compatible.len(), 1); // only Seagate
    assert_eq!(sm_copy.disk_groups[compatible[0]].make, "Seagate");
}

// =====================================================
// Homelab: 4 groups, all different sizes
//   0: Samsung 970 EVO Plus (1x 250GB NVMe)
//   1: Seagate Barracuda (1x 2TB SATA)
//   2: Toshiba N300 (2x 8TB SATA)
//   3: Western Digital Black SN770 (1x 1TB NVMe)
// =====================================================

#[test]
fn test_homelab_disk_grouping() {
    let hw = load_hardware("mixed_drives_homelab");
    let disks: Vec<DiskInfo> = hw.disks.iter().map(DiskInfo::from).collect();
    let groups = DiskGroup::from_disks(&disks);

    assert_eq!(groups.len(), 4);

    assert_eq!(groups[0].make, "Samsung");
    assert_eq!(groups[0].disk_count(), 1);

    assert_eq!(groups[1].make, "Seagate");
    assert_eq!(groups[1].disk_count(), 1);

    assert_eq!(groups[2].make, "Toshiba");
    assert_eq!(groups[2].disk_count(), 2);

    assert_eq!(groups[3].make, "Western Digital");
    assert_eq!(groups[3].disk_count(), 1);
}

#[test]
fn test_homelab_select_toshiba_btrfs_mirror() {
    let hw = load_hardware("mixed_drives_homelab");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // RAID1 (1: Single/RAID1 for max 2 disks), Toshiba group
    // Compatible for RAID1 (needs >=2): only Toshiba (2 disks), so index 0
    run_install(&mut sm, &mut executor, 1, 0);

    let ops = executor.recorded_operations();
    let partitions: Vec<_> = ops
        .iter()
        .filter_map(|r| match &r.operation {
            Operation::PartitionDisk { device } => Some(device.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(partitions, vec!["/dev/sda", "/dev/sdb"]);

    let has_raid = ops.iter().any(|r| match &r.operation {
        Operation::BtrfsRaidSetup {
            devices,
            raid_level,
        } => devices.len() == 2 && raid_level == "raid1",
        _ => false,
    });
    assert!(has_raid);
}

#[test]
fn test_homelab_select_wd_single_btrfs() {
    let hw = load_hardware("mixed_drives_homelab");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    // Single (0), disk index 1 = /dev/nvme1n1 (WD Black SN770)
    // Homelab all_disks order: nvme0n1 (Samsung), nvme1n1 (WD), sda (Toshiba), sdb (Toshiba), sdc (Seagate)
    run_install(&mut sm, &mut executor, 0, 1);

    let ops = executor.recorded_operations();
    let partitions: Vec<_> = ops
        .iter()
        .filter_map(|r| match &r.operation {
            Operation::PartitionDisk { device } => Some(device.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(partitions, vec!["/dev/nvme1n1"]);

    let has_mkfs = ops.iter().any(|r| match &r.operation {
        Operation::MkfsBtrfs { devices } => devices == &vec!["/dev/nvme1n1".to_string()],
        _ => false,
    });
    assert!(has_mkfs);

    // Install target should be /town-os
    let has_install = ops.iter().any(|r| match &r.operation {
        Operation::InstallBaseSystem { target } => target == "/town-os",
        _ => false,
    });
    assert!(has_install);
}

#[test]
fn test_homelab_mirror_filters_single_disk_groups() {
    let hw = load_hardware("mixed_drives_homelab");
    let sm = InstallerStateMachine::new(hw);

    let mut sm_copy = sm;
    sm_copy.selected_raid = Some(RaidConfig::BtrfsRaid1);
    let compatible = sm_copy.compatible_disk_groups();
    // Only Toshiba has 2 disks; Samsung, Seagate, WD have 1 each
    assert_eq!(compatible.len(), 1);
    assert_eq!(sm_copy.disk_groups[compatible[0]].make, "Toshiba");
}

// =====================================================
// Action manifest output tests
// =====================================================

#[test]
fn test_workstation_crucial_raid5_manifest_output() {
    let hw = load_hardware("mixed_drives_workstation");
    let mut sm = InstallerStateMachine::new(hw);
    let mut executor = success_executor();

    run_install(&mut sm, &mut executor, 2, 0);

    // Verify manifest is well-formed
    assert!(!sm.action_manifest.operations.is_empty());
    assert_eq!(
        sm.action_manifest.final_state,
        InstallerFinalState::Installed
    );

    // All operations should be success
    for op in &sm.action_manifest.operations {
        assert_eq!(op.result, OperationOutcome::Success);
    }

    // Verify sequential numbering
    for (i, op) in sm.action_manifest.operations.iter().enumerate() {
        assert_eq!(op.sequence, i as u64);
    }

    // Save and reload to verify serialization round-trip
    let serialized = toml::to_string_pretty(&sm.action_manifest).unwrap();
    assert!(serialized.contains("PartitionDisk"));
    assert!(serialized.contains("BtrfsRaidSetup"));
    assert!(serialized.contains("InstallBaseSystem"));
}
