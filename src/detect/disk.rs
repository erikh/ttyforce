use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::manifest::DiskSpec;

pub fn detect_disks() -> anyhow::Result<Vec<DiskSpec>> {
    // Try UDisks2 dbus first
    if let Some(disks) = detect_disks_udisks2() {
        if !disks.is_empty() {
            return Ok(disks);
        }
    }

    // Fallback: sysfs
    detect_disks_sysfs()
}

/// Detect disks via UDisks2 dbus (org.freedesktop.UDisks2).
fn detect_disks_udisks2() -> Option<Vec<DiskSpec>> {
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
            let removable = get_bool_prop(drive_props, "Removable").unwrap_or(false);
            let size = get_u64_prop(drive_props, "Size").unwrap_or(0);

            drive_info.insert(
                path_str.to_string(),
                DriveMetadata {
                    vendor,
                    model,
                    serial,
                    removable,
                    size,
                },
            );
        }
    }

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

        // Get drive reference and metadata
        let drive_path = get_string_prop(block_props, "Drive").unwrap_or_default();

        let (make, model, serial) = if let Some(drive) = drive_info.get(&drive_path) {
            // Skip removable drives (USB sticks, etc.)
            if drive.removable {
                continue;
            }

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
            (make, model, drive.serial.clone())
        } else {
            ("Unknown".to_string(), "Unknown Model".to_string(), None)
        };

        disks.push(DiskSpec {
            device,
            make,
            model,
            size_bytes: size,
            serial,
        });
    }

    disks.sort_by(|a, b| a.device.cmp(&b.device));
    Some(disks)
}

struct DriveMetadata {
    vendor: String,
    model: String,
    serial: Option<String>,
    removable: bool,
    #[allow(dead_code)]
    size: u64,
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

/// Extract a boolean property from a dbus properties map.
fn get_bool_prop(
    props: &HashMap<String, zbus::zvariant::OwnedValue>,
    key: &str,
) -> Option<bool> {
    let value = props.get(key)?;
    value.try_to_owned().ok()?.try_into().ok()
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

    for entry in fs::read_dir(block_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let dev_path = entry.path();

        // Generic disk detection: check sysfs properties instead of name prefixes
        // Skip removable devices (USB sticks, CD-ROMs, floppies)
        let removable = read_sysfs_trimmed(&dev_path.join("removable"))
            .map(|v| v == "1")
            .unwrap_or(false);
        if removable {
            continue;
        }

        // Skip devices with no size
        let size_sectors = read_sysfs_u64(&dev_path.join("size")).unwrap_or(0);
        let size_bytes = size_sectors * 512;
        if size_bytes == 0 {
            continue;
        }

        // Skip virtual/pseudo block devices by checking for a real device backing
        // Real disks have /sys/block/<name>/device; loop/ram/dm do not
        if !dev_path.join("device").exists() {
            continue;
        }

        cmd_log_append(format!(
            "  found {} ({} bytes / {} GB)",
            name,
            size_bytes,
            size_bytes / 1_000_000_000
        ));

        // Skip tiny devices (< 1GB) - likely USB boot media or similar
        if size_bytes < 1_000_000_000 {
            cmd_log_append(format!("  skipping {} (< 1GB)", name));
            continue;
        }

        let device = format!("/dev/{}", name);

        // Read model and vendor
        let model = read_disk_model(&dev_path, &name);
        let make = read_disk_vendor(&dev_path, &name);
        let serial = read_disk_serial(&dev_path, &name);

        disks.push(DiskSpec {
            device,
            make,
            model,
            size_bytes,
            serial,
        });
    }

    disks.sort_by(|a, b| a.device.cmp(&b.device));
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
}
