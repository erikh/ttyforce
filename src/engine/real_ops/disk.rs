use crate::engine::feedback::OperationResult;

use super::run_cmd;

/// Partition a disk with GPT and a single primary partition.
pub fn partition_disk(device: &str) -> OperationResult {
    match run_cmd(
        "parted",
        &["-s", device, "mklabel", "gpt", "mkpart", "primary", "1MiB", "100%"],
    ) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("failed to partition {}: {}", device, e)),
    }
}

/// Create a btrfs filesystem on one or more devices.
pub fn mkfs_btrfs(devices: &[String]) -> OperationResult {
    let mut args = vec!["-f"];
    let dev_refs: Vec<&str> = devices.iter().map(|d| d.as_str()).collect();
    args.extend(dev_refs);

    match run_cmd("mkfs.btrfs", &args) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "mkfs.btrfs failed on {}: {}",
            devices.join(", "),
            e
        )),
    }
}

/// Create a btrfs subvolume.
pub fn create_btrfs_subvolume(mount_point: &str, name: &str) -> OperationResult {
    let subvol_path = format!("{}/{}", mount_point, name);
    match run_cmd("btrfs", &["subvolume", "create", &subvol_path]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "failed to create btrfs subvolume {}: {}",
            subvol_path, e
        )),
    }
}

/// Set up btrfs with RAID.
pub fn btrfs_raid_setup(devices: &[String], raid_level: &str) -> OperationResult {
    let mut args = vec!["-f", "-d", raid_level, "-m", raid_level];
    let dev_refs: Vec<&str> = devices.iter().map(|d| d.as_str()).collect();
    args.extend(dev_refs);

    match run_cmd("mkfs.btrfs", &args) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "btrfs raid setup ({}) failed on {}: {}",
            raid_level,
            devices.join(", "),
            e
        )),
    }
}

/// Create a ZFS pool.
pub fn create_zpool(name: &str, devices: &[String], raid_level: &str) -> OperationResult {
    let mut args = vec!["create", name];

    // Map raid level name to zpool vdev type
    match raid_level {
        "mirror" => args.push("mirror"),
        "raidz" | "raidz1" => args.push("raidz"),
        "raidz2" => args.push("raidz2"),
        "raidz3" => args.push("raidz3"),
        "stripe" | "" => {} // no vdev keyword for stripe
        other => args.push(other),
    }

    let dev_refs: Vec<&str> = devices.iter().map(|d| d.as_str()).collect();
    args.extend(dev_refs);

    match run_cmd("zpool", &args) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "zpool create {} ({}) failed: {}",
            name, raid_level, e
        )),
    }
}

/// Create a ZFS dataset.
pub fn create_zfs_dataset(pool: &str, name: &str) -> OperationResult {
    let dataset = format!("{}/{}", pool, name);
    match run_cmd("zfs", &["create", &dataset]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("zfs create {} failed: {}", dataset, e)),
    }
}
