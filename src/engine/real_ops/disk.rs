use std::fs;

use crate::engine::feedback::OperationResult;

use super::run_cmd;

/// Partition a disk with GPT and a single primary partition.
/// Runs `partprobe` and `udevadm settle` afterward so the kernel picks up the
/// new partition table and the partition device node (e.g. `/dev/sda1`) exists
/// before any subsequent mkfs call.
pub fn partition_disk(device: &str) -> OperationResult {
    if let Err(e) = run_cmd(
        "parted",
        &["-s", device, "mklabel", "gpt", "mkpart", "primary", "1MiB", "100%"],
    ) {
        return OperationResult::Error(format!("failed to partition {}: {}", device, e));
    }

    // Re-read partition table
    let _ = run_cmd("partprobe", &[device]);
    let _ = run_cmd("udevadm", &["settle", "--timeout=5"]);

    OperationResult::Success
}

/// Return the first partition device path for a disk.
/// NVMe devices use a `p` separator (e.g. `nvme0n1p1`), while SCSI/virtio/IDE
/// devices append the number directly (e.g. `sda1`).
pub fn partition_path(device: &str) -> String {
    let base = device.rsplit('/').next().unwrap_or(device);
    if base.starts_with("nvme") || base.starts_with("mmcblk") || base.starts_with("loop") {
        format!("{}p1", device)
    } else {
        format!("{}1", device)
    }
}

/// Create a btrfs filesystem on one or more devices.
/// Automatically converts raw disk paths to their first partition path.
pub fn mkfs_btrfs(devices: &[String]) -> OperationResult {
    let part_devices: Vec<String> = devices.iter().map(|d| partition_path(d)).collect();
    let mut args = vec!["-f"];
    let dev_refs: Vec<&str> = part_devices.iter().map(|d| d.as_str()).collect();
    args.extend(dev_refs);

    match run_cmd("mkfs.btrfs", &args) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "mkfs.btrfs failed on {}: {}",
            part_devices.join(", "),
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

/// Mount a filesystem at the given mount point, creating the directory if needed.
pub fn mount_filesystem(device: &str, mount_point: &str, fs_type: &str) -> OperationResult {
    if let Err(e) = fs::create_dir_all(mount_point) {
        return OperationResult::Error(format!(
            "failed to create mount point {}: {}",
            mount_point, e
        ));
    }

    match run_cmd("mount", &["-t", fs_type, device, mount_point]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "failed to mount {} at {}: {}",
            device, mount_point, e
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
/// Automatically converts raw disk paths to their first partition path.
pub fn btrfs_raid_setup(devices: &[String], raid_level: &str) -> OperationResult {
    let part_devices: Vec<String> = devices.iter().map(|d| partition_path(d)).collect();
    let mut args = vec!["-f", "-d", raid_level, "-m", raid_level];
    let dev_refs: Vec<&str> = part_devices.iter().map(|d| d.as_str()).collect();
    args.extend(dev_refs);

    match run_cmd("mkfs.btrfs", &args) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "btrfs raid setup ({}) failed on {}: {}",
            raid_level,
            part_devices.join(", "),
            e
        )),
    }
}
