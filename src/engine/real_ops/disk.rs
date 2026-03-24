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
/// For btrfs, runs `btrfs device scan` first so RAID members are discovered.
pub fn mount_filesystem(device: &str, mount_point: &str, fs_type: &str, options: Option<&str>) -> OperationResult {
    if let Err(e) = fs::create_dir_all(mount_point) {
        return OperationResult::Error(format!(
            "failed to create mount point {}: {}",
            mount_point, e
        ));
    }

    // For btrfs RAID arrays, scan for member devices before mounting
    if fs_type == "btrfs" {
        let _ = run_cmd("btrfs", &["device", "scan"]);
    }

    let result = if let Some(opts) = options {
        run_cmd("mount", &["-t", fs_type, "-o", opts, device, mount_point])
    } else {
        run_cmd("mount", &["-t", fs_type, device, mount_point])
    };

    match result {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "failed to mount {} at {}: {}",
            device, mount_point, e
        )),
    }
}

/// Generate a systemd service unit to mount the btrfs volume at boot.
/// Written to `<etc_prefix>/systemd/system/` so the installed system
/// mounts the volume automatically. Uses a service (not a .mount unit)
/// to avoid systemd's path-escaping issues with hyphens.
///
/// The `mount_point` parameter here is actually the etc_prefix path
/// (the directory that maps to /etc on the installed system).
pub fn generate_fstab(mount_point: &str, device: &str, fs_type: &str) -> OperationResult {
    let unit_dir = format!("{}/systemd/system", mount_point);
    if let Err(e) = fs::create_dir_all(&unit_dir) {
        return OperationResult::Error(format!("failed to create {}: {}", unit_dir, e));
    }

    let service_name = "mount-town-os.service";
    let service_content = generate_mount_service(mount_point, device, fs_type);
    let service_path = format!("{}/{}", unit_dir, service_name);
    if let Err(e) = fs::write(&service_path, &service_content) {
        return OperationResult::Error(format!("failed to write {}: {}", service_path, e));
    }

    // Enable the service by creating a symlink in local-fs.target.wants
    let wants_dir = format!("{}/local-fs.target.wants", unit_dir);
    if let Err(e) = fs::create_dir_all(&wants_dir) {
        return OperationResult::Error(format!("failed to create {}: {}", wants_dir, e));
    }

    let symlink_path = format!("{}/{}", wants_dir, service_name);
    // Remove existing symlink if present
    let _ = fs::remove_file(&symlink_path);
    if let Err(e) = std::os::unix::fs::symlink(
        format!("/etc/systemd/system/{}", service_name),
        &symlink_path,
    ) {
        return OperationResult::Error(format!(
            "failed to enable {}: {}",
            service_name, e
        ));
    }

    OperationResult::Success
}

/// Generate the systemd service unit content for mounting btrfs at boot.
pub fn generate_mount_service(mount_point: &str, device: &str, fs_type: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Mount Town OS volume at {mount_point}\n\
         DefaultDependencies=no\n\
         After=local-fs-pre.target\n\
         Before=local-fs.target multi-user.target\n\
         \n\
         ConditionPathIsMountPoint=!{mount_point}\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=yes\n\
         ExecStartPre=-/usr/bin/mkdir -p {mount_point}\n\
         ExecStartPre=-/usr/bin/btrfs device scan\n\
         ExecStart=/usr/bin/mount -t {fs_type} -o defaults,subvol=@ {device} {mount_point}\n\
         ExecStop=/usr/bin/umount {mount_point}\n\
         \n\
         [Install]\n\
         WantedBy=local-fs.target\n",
        mount_point = mount_point,
        device = device,
        fs_type = fs_type,
    )
}

/// Recursively unmount a mount point. Best-effort: returns Success even on
/// failure since the mount may not exist.
pub fn cleanup_unmount(mount_point: &str) -> OperationResult {
    let _ = run_cmd("umount", &["-R", mount_point]);
    OperationResult::Success
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_mount_service_content() {
        let content = generate_mount_service("/town-os", "/dev/sda1", "btrfs");
        assert!(content.contains("mount -t btrfs"), "missing mount command");
        assert!(content.contains("/dev/sda1"), "missing device");
        assert!(content.contains("/town-os"), "missing mount point");
        assert!(content.contains("subvol=@"), "missing subvol option");
        assert!(content.contains("Before=local-fs.target multi-user.target"), "missing ordering");
        assert!(content.contains("btrfs device scan"), "missing device scan");
        assert!(content.contains("mkdir -p /town-os"), "missing mkdir");
        assert!(
            content.contains("ConditionPathIsMountPoint=!/town-os"),
            "missing already-mounted check"
        );
        assert!(
            content.contains("ExecStartPre=-"),
            "ExecStartPre should ignore failures with - prefix"
        );
    }

    #[test]
    fn test_generate_fstab_creates_service() {
        let tmp = std::env::temp_dir().join("ttyforce-mount-svc-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let result = generate_fstab(tmp.to_str().unwrap(), "/dev/sda1", "btrfs");
        assert!(result.is_success(), "generate_fstab failed: {:?}", result);

        let svc = std::fs::read_to_string(
            tmp.join("systemd/system/mount-town-os.service"),
        )
        .unwrap();
        assert!(svc.contains("/dev/sda1"));
        assert!(svc.contains("btrfs"));

        // Check symlink exists for enabling (use symlink_metadata since target is absolute
        // and won't exist in the test environment — exists() follows symlinks)
        let link = tmp.join("systemd/system/local-fs.target.wants/mount-town-os.service");
        assert!(link.symlink_metadata().is_ok(), "enable symlink missing");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_generate_fstab_idempotent() {
        let tmp = std::env::temp_dir().join("ttyforce-mount-svc-idem-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        generate_fstab(tmp.to_str().unwrap(), "/dev/sda1", "btrfs");
        generate_fstab(tmp.to_str().unwrap(), "/dev/sda1", "btrfs");

        // Should still work (overwrites cleanly)
        let svc = std::fs::read_to_string(
            tmp.join("systemd/system/mount-town-os.service"),
        )
        .unwrap();
        assert!(svc.contains("/dev/sda1"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_partition_path_scsi() {
        assert_eq!(partition_path("/dev/sda"), "/dev/sda1");
        assert_eq!(partition_path("/dev/sdb"), "/dev/sdb1");
    }

    #[test]
    fn test_partition_path_nvme() {
        assert_eq!(partition_path("/dev/nvme0n1"), "/dev/nvme0n1p1");
    }

    #[test]
    fn test_partition_path_mmcblk() {
        assert_eq!(partition_path("/dev/mmcblk0"), "/dev/mmcblk0p1");
    }

    #[test]
    fn test_partition_path_virtio() {
        assert_eq!(partition_path("/dev/vda"), "/dev/vda1");
    }
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
