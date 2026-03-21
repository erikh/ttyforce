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

/// Recursively unmount a mount point. Best-effort: returns Success even on
/// failure since the mount may not exist.
pub fn cleanup_unmount(mount_point: &str) -> OperationResult {
    let _ = run_cmd("umount", &["-R", mount_point]);
    OperationResult::Success
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
