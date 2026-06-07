use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::manifest::DiskSpec;

/// Read the set of whole-disk device paths that back the currently running
/// system (root, boot, and — on Town OS — the squashfs data partition). These
/// disks must never be offered as installation targets: the device that is
/// managing ttyforce is off-limits even if it has unused space, and a USB
/// stick the machine booted from must not be wiped.
fn read_boot_disks() -> HashSet<String> {
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut set = boot_disks_from_mounts(&mounts);
    // /proc/mounts alone is not enough: in the Town OS initrd, ttyforce runs
    // BEFORE the boot device's partition is mounted (the squashfs root
    // mount_handler runs later), so the device we booted from may not appear in
    // /proc/mounts yet. The kernel `root=` parameter always names it — resolve it
    // and exclude that whole disk too, so we never wipe the USB/SD/disk the system
    // booted from even when nothing from it is mounted at detection time.
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    if let Some(disk) = boot_disk_from_cmdline(&cmdline) {
        set.insert(disk);
    }
    set
}

/// Resolve the kernel `root=` parameter to its parent whole-disk device path
/// (e.g. `root=UUID=…` → `/dev/sda`). Returns `None` if absent or unresolvable.
fn boot_disk_from_cmdline(cmdline: &str) -> Option<String> {
    let spec = root_spec_from_cmdline(cmdline)?;
    let dev = resolve_root_spec(&spec)?;
    parent_disk_of_dev(&dev)
}

/// Extract the value of the `root=` token from a kernel command line. Pure
/// function for testability.
fn root_spec_from_cmdline(cmdline: &str) -> Option<String> {
    cmdline
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("root="))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Resolve a `root=` spec to a concrete `/dev/...` path. Handles the common
/// forms: `UUID=`, `LABEL=`, `PARTUUID=` (via the udev-populated
/// `/dev/disk/by-*` symlinks) and a literal `/dev/...` path.
fn resolve_root_spec(spec: &str) -> Option<String> {
    let by_link = |subdir: &str, value: &str| -> Option<String> {
        let link = Path::new("/dev/disk").join(subdir).join(value);
        Some(fs::canonicalize(link).ok()?.to_string_lossy().to_string())
    };
    if let Some(uuid) = spec.strip_prefix("UUID=") {
        by_link("by-uuid", uuid)
    } else if let Some(label) = spec.strip_prefix("LABEL=") {
        by_link("by-label", label)
    } else if let Some(puuid) = spec.strip_prefix("PARTUUID=") {
        by_link("by-partuuid", puuid)
    } else if spec.starts_with("/dev/") {
        Some(spec.to_string())
    } else {
        None
    }
}

/// Parse `/proc/mounts` content into the set of parent whole-disk device paths
/// that are currently mounted. Pure function for testability.
fn boot_disks_from_mounts(mounts: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    for line in mounts.lines() {
        let source = match line.split_whitespace().next() {
            Some(s) => s,
            None => continue,
        };
        if let Some(parent) = parent_disk_of_dev(source) {
            set.insert(parent);
        }
    }
    set
}

/// Map a mount source like `/dev/sda1`, `/dev/nvme0n1p2`, or `/dev/mmcblk0p1`
/// to its parent whole-disk device path (`/dev/sda`, `/dev/nvme0n1`,
/// `/dev/mmcblk0`). Returns `None` for non-`/dev` sources (overlay, tmpfs,
/// proc) and nested paths (`/dev/mapper/...`, `/dev/disk/by-uuid/...`).
fn parent_disk_of_dev(source: &str) -> Option<String> {
    let name = source.strip_prefix("/dev/")?;
    // Ignore nested device paths (mapper/, disk/by-*/) — these are not the
    // simple whole-disk/partition names we filter on.
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let base = strip_partition_suffix(name);
    if base.is_empty() {
        return None;
    }
    Some(format!("/dev/{}", base))
}

/// Strip a partition suffix from a block device name to get the whole disk.
///
/// Devices whose base name ends in a digit (nvme0n1, mmcblk0, loop0, nbd0)
/// use a `p<N>` partition separator. Everything else (sd*, hd*, vd*, xvd*)
/// uses bare trailing digits.
fn strip_partition_suffix(name: &str) -> &str {
    let p_separated = name.starts_with("nvme")
        || name.starts_with("mmcblk")
        || name.starts_with("loop")
        || name.starts_with("nbd");
    if p_separated {
        // The partition separator is a 'p' preceded by a digit (e.g. nvme0n1p2,
        // loop0p1) — not the 'p' inside "loop" itself. Find the last such 'p'
        // whose tail is all digits and split there.
        let bytes = name.as_bytes();
        for i in (1..bytes.len()).rev() {
            if bytes[i] == b'p' && bytes[i - 1].is_ascii_digit() {
                let digits = &name[i + 1..];
                if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                    return &name[..i];
                }
            }
        }
        return name;
    }
    let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if trimmed.is_empty() {
        name
    } else {
        trimmed
    }
}

pub fn detect_disks() -> anyhow::Result<Vec<DiskSpec>> {
    use crate::engine::real_ops::cmd_log_append;
    cmd_log_append("$ detect disks (UDisks2 dbus → sysfs fallback)".to_string());
    // Try UDisks2 dbus first
    match detect_disks_udisks2() {
        Some(disks) if !disks.is_empty() => {
            cmd_log_append(format!(
                "  -> UDisks2 returned {} disk(s)",
                disks.len()
            ));
            return Ok(disks);
        }
        Some(_) => cmd_log_append("  -> UDisks2 returned no disks; falling back to sysfs".to_string()),
        None => cmd_log_append("  -> UDisks2 unavailable; falling back to sysfs".to_string()),
    }

    detect_disks_sysfs()
}

/// Detect disks via UDisks2 dbus (org.freedesktop.UDisks2).
fn detect_disks_udisks2() -> Option<Vec<DiskSpec>> {
    use crate::engine::real_ops::cmd_log_append;
    let conn = zbus::blocking::Connection::system().ok()?;

    // GetManagedObjects returns all UDisks2 objects with their interfaces/properties
    let reply = conn
        .call_method(
            Some("org.freedesktop.UDisks2"),
            "/org/freedesktop/UDisks2",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .ok()?;

    // Type: a{oa{sa{sv}}}
    let objects: HashMap<
        zbus::zvariant::OwnedObjectPath,
        HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>>,
    > = reply.body().deserialize().ok()?;

    // First pass: collect drive info (vendor, model, serial) keyed by drive path
    let mut drive_info: HashMap<String, DriveMetadata> = HashMap::new();
    for (path, interfaces) in &objects {
        let path_str = path.as_str();
        if !path_str.starts_with("/org/freedesktop/UDisks2/drives/") {
            continue;
        }
        if let Some(drive_props) = interfaces.get("org.freedesktop.UDisks2.Drive") {
            let vendor = get_string_prop(drive_props, "Vendor").unwrap_or_default();
            let model = get_string_prop(drive_props, "Model").unwrap_or_default();
            let serial = get_string_prop(drive_props, "Serial");
            let size = get_u64_prop(drive_props, "Size").unwrap_or(0);
            let connection_bus =
                get_string_prop(drive_props, "ConnectionBus").unwrap_or_default();

            drive_info.insert(
                path_str.to_string(),
                DriveMetadata {
                    vendor,
                    model,
                    serial,
                    size,
                    connection_bus,
                },
            );
        }
    }

    // Disks backing the running system must never be installation targets.
    let boot_disks = read_boot_disks();

    // Second pass: collect block devices and match to drives
    let mut disks = Vec::new();
    for (path, interfaces) in &objects {
        let path_str = path.as_str();
        if !path_str.starts_with("/org/freedesktop/UDisks2/block_devices/") {
            continue;
        }

        let block_props = match interfaces.get("org.freedesktop.UDisks2.Block") {
            Some(props) => props,
            None => continue,
        };

        // Skip partitions: they have a PartitionTable parent or Partition interface
        if interfaces.contains_key("org.freedesktop.UDisks2.Partition") {
            continue;
        }

        // Get device path (PreferredDevice is ay, null-terminated byte array)
        let device = get_device_path(block_props)?;

        // Skip non-real block devices
        let dev_name = device.trim_start_matches("/dev/");
        if !is_real_disk(dev_name) {
            continue;
        }

        let size = get_u64_prop(block_props, "Size").unwrap_or(0);

        // Skip tiny devices (< 1GB)
        if size < 1_000_000_000 {
            continue;
        }

        // Skip the disk the running system booted from (includes USB boot media).
        if boot_disks.contains(&device) {
            cmd_log_append(format!("  skip {} (boot/running-system disk)", device));
            continue;
        }

        // Get drive reference and metadata
        let drive_path = get_string_prop(block_props, "Drive").unwrap_or_default();

        let (make, model, serial, transport) =
            if let Some(drive) = drive_info.get(&drive_path) {
                let make = if drive.vendor.is_empty() {
                    extract_vendor_from_model(&drive.model)
                        .unwrap_or_else(|| "Unknown".to_string())
                } else {
                    drive.vendor.clone()
                };
                let model = if drive.model.is_empty() {
                    "Unknown Model".to_string()
                } else {
                    drive.model.clone()
                };
                let transport = udisks2_bus_to_transport(&drive.connection_bus, dev_name);
                (make, model, drive.serial.clone(), transport)
            } else {
                let transport = transport_from_device_name(dev_name);
                (
                    "Unknown".to_string(),
                    "Unknown Model".to_string(),
                    None,
                    transport,
                )
            };

        disks.push(DiskSpec {
            device,
            make,
            model,
            size_bytes: size,
            serial,
            transport,
        });
    }

    disks.sort_by(|a, b| a.device.cmp(&b.device));
    Some(disks)
}

struct DriveMetadata {
    vendor: String,
    model: String,
    serial: Option<String>,
    #[allow(dead_code)]
    size: u64,
    connection_bus: String,
}

/// Extract a string property from a dbus properties map.
fn get_string_prop(
    props: &HashMap<String, zbus::zvariant::OwnedValue>,
    key: &str,
) -> Option<String> {
    let value = props.get(key)?;
    let s: String = value.try_to_owned().ok()?.try_into().ok()?;
    if s.is_empty() {
        None
    } else {
        Some(s.trim().to_string())
    }
}

/// Extract a u64 property from a dbus properties map.
fn get_u64_prop(
    props: &HashMap<String, zbus::zvariant::OwnedValue>,
    key: &str,
) -> Option<u64> {
    let value = props.get(key)?;
    value.try_to_owned().ok()?.try_into().ok()
}

/// Extract device path from UDisks2 PreferredDevice property (ay — null-terminated byte array).
fn get_device_path(
    props: &HashMap<String, zbus::zvariant::OwnedValue>,
) -> Option<String> {
    let value = props.get("PreferredDevice")?;
    let bytes: Vec<u8> = value.try_to_owned().ok()?.try_into().ok()?;
    // Strip null terminator
    let path_bytes: Vec<u8> = bytes.into_iter().take_while(|&b| b != 0).collect();
    let path = String::from_utf8(path_bytes).ok()?;
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// Fallback: detect disks via sysfs.
pub fn detect_disks_sysfs() -> anyhow::Result<Vec<DiskSpec>> {
    use crate::engine::real_ops::cmd_log_append;

    let mut disks = Vec::new();
    let block_dir = Path::new("/sys/block");

    if !block_dir.exists() {
        cmd_log_append("  /sys/block does not exist".to_string());
        return Ok(disks);
    }

    cmd_log_append("$ scan /sys/block for disks".to_string());

    // Disks backing the running system must never be installation targets.
    let boot_disks = read_boot_disks();

    let entries: Vec<_> = fs::read_dir(block_dir)?.collect::<Result<_, _>>()?;
    cmd_log_append(format!(
        "  -> {} block device(s) in /sys/block",
        entries.len()
    ));

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let dev_path = entry.path();
        cmd_log_append(format!("  inspect {}", name));

        // Generic disk detection: accept real disks (sd*, nvme*, mmcblk*, …,
        // including USB-attached drives) and reject optical/floppy/virtual
        // devices (sr*, fd*, loop*, ram*, dm-*, zram*, nbd*, md*). USB sticks
        // are no longer filtered here — they are valid storage targets. The
        // device the system booted from is excluded separately below.
        if !is_real_disk(&name) {
            cmd_log_append(format!("    skip {} (not a real disk)", name));
            continue;
        }

        // Skip devices with no size
        let size_sectors = read_sysfs_u64(&dev_path.join("size")).unwrap_or(0);
        let size_bytes = size_sectors * 512;
        if size_bytes == 0 {
            cmd_log_append(format!("    skip {} (zero size)", name));
            continue;
        }

        // Skip virtual/pseudo block devices by checking for a real device backing
        // Real disks have /sys/block/<name>/device; loop/ram/dm do not
        if !dev_path.join("device").exists() {
            cmd_log_append(format!(
                "    skip {} (no device backing — virtual/loop/dm)",
                name
            ));
            continue;
        }

        cmd_log_append(format!(
            "    found {} ({} bytes / {} GB)",
            name,
            size_bytes,
            size_bytes / 1_000_000_000
        ));

        // Skip tiny devices (< 1GB) - likely USB boot media or similar
        if size_bytes < 1_000_000_000 {
            cmd_log_append(format!("    skip {} (< 1GB)", name));
            continue;
        }

        let device = format!("/dev/{}", name);

        // Skip the disk the running system booted from (includes USB boot media).
        if boot_disks.contains(&device) {
            cmd_log_append(format!("    skip {} (boot/running-system disk)", name));
            continue;
        }

        // Read model and vendor
        let model = read_disk_model(&dev_path, &name);
        let make = read_disk_vendor(&dev_path, &name);
        let serial = read_disk_serial(&dev_path, &name);
        let transport = detect_transport_sysfs(&dev_path, &name);

        cmd_log_append(format!(
            "    accept {} make={} model={} transport={}",
            name, make, model, transport
        ));

        disks.push(DiskSpec {
            device,
            make,
            model,
            size_bytes,
            serial,
            transport,
        });
    }

    disks.sort_by(|a, b| a.device.cmp(&b.device));
    cmd_log_append(format!("  -> sysfs scan accepted {} disk(s)", disks.len()));
    Ok(disks)
}

fn is_real_disk(name: &str) -> bool {
    // Skip known virtual/non-disk devices
    if name.starts_with("loop")
        || name.starts_with("ram")
        || name.starts_with("dm-")
        || name.starts_with("sr")
        || name.starts_with("fd")
        || name.starts_with("zram")
        || name.starts_with("nbd")
        || name.starts_with("md")
    {
        return false;
    }

    // Accept known disk prefixes
    if name.starts_with("sd")
        || name.starts_with("nvme")
        || name.starts_with("vd")
        || name.starts_with("hd")
        || name.starts_with("xvd")    // Xen virtual disks
        || name.starts_with("mmcblk")  // eMMC/SD cards
    {
        return true;
    }

    // For anything else, check if it's not removable and has a size > 0
    let dev_path = Path::new("/sys/block").join(name);
    let removable = read_sysfs_trimmed(&dev_path.join("removable"))
        .map(|v| v == "1")
        .unwrap_or(true);
    let size = read_sysfs_u64(&dev_path.join("size")).unwrap_or(0);

    !removable && size > 0
}

fn read_disk_model(dev_path: &Path, name: &str) -> String {
    // Try device/model first (works for SCSI/SATA)
    if let Some(model) = read_sysfs_trimmed(&dev_path.join("device/model")) {
        if !model.is_empty() {
            return model;
        }
    }

    // NVMe: /sys/block/nvme0n1/device/model
    if name.starts_with("nvme") {
        if let Some(model) = read_sysfs_trimmed(&dev_path.join("device/model")) {
            if !model.is_empty() {
                return model;
            }
        }
    }

    // Fallback: try /sys/block/<name>/device/id
    if let Some(model) = read_sysfs_trimmed(&dev_path.join("device/id")) {
        if !model.is_empty() {
            return model;
        }
    }

    "Unknown Model".to_string()
}

fn read_disk_vendor(dev_path: &Path, name: &str) -> String {
    // Try device/vendor first (SCSI/SATA)
    if let Some(vendor) = read_sysfs_trimmed(&dev_path.join("device/vendor")) {
        if !vendor.is_empty() {
            return vendor;
        }
    }

    // NVMe doesn't have a separate vendor file; parse from model
    if name.starts_with("nvme") {
        if let Some(model) = read_sysfs_trimmed(&dev_path.join("device/model")) {
            if let Some(vendor) = extract_vendor_from_model(&model) {
                return vendor;
            }
        }
    }

    // Virtio disks
    if name.starts_with("vd") {
        return "VirtIO".to_string();
    }

    "Unknown".to_string()
}

fn read_disk_serial(dev_path: &Path, _name: &str) -> Option<String> {
    // Try device/serial
    if let Some(serial) = read_sysfs_trimmed(&dev_path.join("device/serial")) {
        if !serial.is_empty() {
            return Some(serial);
        }
    }
    // Try device/nguid (NVMe)
    if let Some(nguid) = read_sysfs_trimmed(&dev_path.join("device/nguid")) {
        if !nguid.is_empty() && nguid != "00000000-0000-0000-0000-000000000000" {
            return Some(nguid);
        }
    }
    None
}

fn extract_vendor_from_model(model: &str) -> Option<String> {
    let known_vendors = [
        "Samsung", "Western Digital", "WD", "Seagate", "Toshiba", "Kingston",
        "Crucial", "Intel", "SK Hynix", "KIOXIA", "Micron", "SanDisk",
        "Sabrent", "ADATA", "PNY", "Corsair", "Transcend",
    ];
    let model_upper = model.to_uppercase();
    for vendor in &known_vendors {
        if model_upper.starts_with(&vendor.to_uppercase()) {
            return Some(vendor.to_string());
        }
    }
    None
}

fn read_sysfs_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn read_sysfs_u64(path: &Path) -> Option<u64> {
    read_sysfs_trimmed(path)?.parse::<u64>().ok()
}

/// Detect the transport/attachment type from sysfs.
///
/// Resolves the device symlink to determine the bus hierarchy:
/// - USB devices have "usb" in the resolved sysfs path
/// - NVMe, virtio, mmc, ide, xen are identified by device name prefix
/// - SCSI/SATA devices that aren't USB are classified as "sata"
fn detect_transport_sysfs(dev_path: &Path, name: &str) -> String {
    // NVMe, virtio, mmc, ide, xen are unambiguous from device name
    if name.starts_with("nvme") {
        return "nvme".to_string();
    }
    if name.starts_with("vd") {
        return "virtio".to_string();
    }
    if name.starts_with("mmcblk") {
        return "mmc".to_string();
    }
    if name.starts_with("hd") {
        return "ide".to_string();
    }
    if name.starts_with("xvd") {
        return "xen".to_string();
    }

    // For sd* devices, check if attached via USB by resolving the sysfs device symlink.
    // USB-attached disks have "usb" somewhere in their sysfs device path.
    if name.starts_with("sd") {
        let device_link = dev_path.join("device");
        if let Ok(resolved) = fs::canonicalize(&device_link) {
            let resolved_str = resolved.to_string_lossy();
            if resolved_str.contains("/usb") {
                return "usb".to_string();
            }
        }
        return "sata".to_string();
    }

    "unknown".to_string()
}

/// Convert UDisks2 ConnectionBus string to our transport name.
fn udisks2_bus_to_transport(connection_bus: &str, dev_name: &str) -> String {
    match connection_bus {
        "usb" => "usb".to_string(),
        "sdio" => "mmc".to_string(),
        _ => transport_from_device_name(dev_name),
    }
}

/// Fallback: infer transport from device name alone.
pub fn transport_from_device_name(dev_name: &str) -> String {
    if dev_name.starts_with("nvme") {
        "nvme".to_string()
    } else if dev_name.starts_with("vd") {
        "virtio".to_string()
    } else if dev_name.starts_with("mmcblk") {
        "mmc".to_string()
    } else if dev_name.starts_with("hd") {
        "ide".to_string()
    } else if dev_name.starts_with("xvd") {
        "xen".to_string()
    } else if dev_name.starts_with("sd") {
        "sata".to_string()
    } else {
        "unknown".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_real_disk() {
        // Known disk types
        assert!(is_real_disk("sda"));
        assert!(is_real_disk("sdb"));
        assert!(is_real_disk("nvme0n1"));
        assert!(is_real_disk("vda"));
        assert!(is_real_disk("hda"));
        assert!(is_real_disk("xvda"));
        assert!(is_real_disk("mmcblk0"));

        // Known non-disk types
        assert!(!is_real_disk("loop0"));
        assert!(!is_real_disk("dm-0"));
        assert!(!is_real_disk("ram0"));
        assert!(!is_real_disk("sr0"));
        assert!(!is_real_disk("fd0"));
        assert!(!is_real_disk("zram0"));
        assert!(!is_real_disk("nbd0"));
        assert!(!is_real_disk("md0"));
    }

    #[test]
    fn test_extract_vendor_from_model() {
        assert_eq!(
            extract_vendor_from_model("Samsung 980 PRO"),
            Some("Samsung".to_string())
        );
        assert_eq!(
            extract_vendor_from_model("WD Blue SN570"),
            Some("WD".to_string())
        );
        assert_eq!(
            extract_vendor_from_model("UNKNOWN_DRIVE_XYZ"),
            None
        );
    }

    #[test]
    fn test_usb_drives_are_real_disks() {
        // USB-attached drives enumerate as sd* and must be accepted as valid
        // storage targets now that the removable filter is gone.
        assert!(is_real_disk("sdb"));
        assert!(is_real_disk("sdc"));
        // Optical and floppy media must still be rejected.
        assert!(!is_real_disk("sr0"));
        assert!(!is_real_disk("fd0"));
    }

    #[test]
    fn test_strip_partition_suffix() {
        // SCSI/SATA/USB/virtio: bare trailing digits.
        assert_eq!(strip_partition_suffix("sda1"), "sda");
        assert_eq!(strip_partition_suffix("sda"), "sda");
        assert_eq!(strip_partition_suffix("sda12"), "sda");
        assert_eq!(strip_partition_suffix("vdb3"), "vdb");
        assert_eq!(strip_partition_suffix("xvda1"), "xvda");
        // NVMe / eMMC: 'p' partition separator (base name ends in a digit).
        assert_eq!(strip_partition_suffix("nvme0n1p2"), "nvme0n1");
        assert_eq!(strip_partition_suffix("nvme0n1"), "nvme0n1");
        assert_eq!(strip_partition_suffix("mmcblk0p1"), "mmcblk0");
        assert_eq!(strip_partition_suffix("mmcblk0"), "mmcblk0");
    }

    #[test]
    fn test_parent_disk_of_dev() {
        assert_eq!(parent_disk_of_dev("/dev/sda1"), Some("/dev/sda".to_string()));
        assert_eq!(parent_disk_of_dev("/dev/sda"), Some("/dev/sda".to_string()));
        assert_eq!(
            parent_disk_of_dev("/dev/nvme0n1p3"),
            Some("/dev/nvme0n1".to_string())
        );
        assert_eq!(
            parent_disk_of_dev("/dev/mmcblk0p1"),
            Some("/dev/mmcblk0".to_string())
        );
        // Non-/dev sources and nested device paths are ignored.
        assert_eq!(parent_disk_of_dev("overlay"), None);
        assert_eq!(parent_disk_of_dev("tmpfs"), None);
        assert_eq!(parent_disk_of_dev("proc"), None);
        assert_eq!(parent_disk_of_dev("/dev/mapper/cryptroot"), None);
        assert_eq!(parent_disk_of_dev("/dev/disk/by-uuid/abcd"), None);
    }

    #[test]
    fn test_boot_disks_from_mounts() {
        // A representative Town OS initrd /proc/mounts: the squashfs data
        // partition lives on /dev/sda (booted from a USB stick), the running
        // root is an overlay, plus the usual virtual filesystems.
        let mounts = "\
proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0
sysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0
udev /dev devtmpfs rw,nosuid,relatime 0 0
/dev/sda1 /.town/data ext4 ro,relatime 0 0
/dev/loop0 /.town/root squashfs ro,relatime 0 0
overlay / overlay rw,relatime 0 0
tmpfs /run tmpfs rw,nosuid,nodev 0 0
";
        let boot = boot_disks_from_mounts(mounts);
        // The USB boot disk and its loop device are both excluded; an internal
        // NVMe drive (/dev/nvme0n1) is absent here, so it stays installable.
        assert!(boot.contains("/dev/sda"));
        assert!(boot.contains("/dev/loop0"));
        assert!(!boot.contains("/dev/nvme0n1"));
    }

    #[test]
    fn test_boot_disks_excludes_whole_disk_with_free_space() {
        // Boot partition on /dev/nvme0n1p1 must exclude the WHOLE disk
        // /dev/nvme0n1, even though the rest of the disk is unpartitioned and
        // has free space available.
        let mounts = "/dev/nvme0n1p1 /.town/data ext4 ro 0 0\n";
        let boot = boot_disks_from_mounts(mounts);
        assert!(boot.contains("/dev/nvme0n1"));
    }

    #[test]
    fn test_boot_disks_empty_mounts() {
        assert!(boot_disks_from_mounts("").is_empty());
    }

    #[test]
    fn test_root_spec_from_cmdline() {
        // Town OS GRUB form: root=UUID=…
        assert_eq!(
            root_spec_from_cmdline(
                "BOOT_IMAGE=/boot/Image root=UUID=f771ccda-3b0b-42f7-99ee-7655aa2d373c rootwait rw console=ttyAMA0,115200"
            ),
            Some("UUID=f771ccda-3b0b-42f7-99ee-7655aa2d373c".to_string())
        );
        // Literal device form.
        assert_eq!(
            root_spec_from_cmdline("root=/dev/nvme0n1p2 rw"),
            Some("/dev/nvme0n1p2".to_string())
        );
        // No root= present, and an empty root= value.
        assert_eq!(root_spec_from_cmdline("rw quiet splash"), None);
        assert_eq!(root_spec_from_cmdline("root= rw"), None);
    }

    #[test]
    fn test_boot_disk_from_cmdline_literal_dev() {
        // A literal /dev path resolves to its parent whole disk without touching
        // the filesystem (UUID/LABEL forms need udev symlinks, so aren't unit-tested).
        assert_eq!(
            boot_disk_from_cmdline("root=/dev/sda3 rw"),
            Some("/dev/sda".to_string())
        );
        assert_eq!(
            boot_disk_from_cmdline("root=/dev/nvme0n1p2 rw"),
            Some("/dev/nvme0n1".to_string())
        );
    }
}
