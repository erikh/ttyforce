use serde::{Deserialize, Serialize};

use crate::manifest::DiskSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskInfo {
    pub device: String,
    pub make: String,
    pub model: String,
    pub size_bytes: u64,
    pub serial: Option<String>,
    /// How the disk is attached: "sata", "nvme", "usb", "virtio", etc.
    pub transport: String,
}

impl From<&DiskSpec> for DiskInfo {
    fn from(spec: &DiskSpec) -> Self {
        Self {
            device: spec.device.clone(),
            make: spec.make.clone(),
            model: spec.model.clone(),
            size_bytes: spec.size_bytes,
            serial: spec.serial.clone(),
            transport: spec.transport.clone(),
        }
    }
}

/// 10 GB threshold for considering disks "similar size" for RAID grouping.
pub const SIZE_SIMILARITY_THRESHOLD: u64 = 10 * 1_073_741_824;

impl DiskInfo {
    pub fn size_human(&self) -> String {
        let gb = self.size_bytes as f64 / 1_073_741_824.0;
        if gb >= 1024.0 {
            format!("{:.1} TB", gb / 1024.0)
        } else {
            format!("{:.1} GB", gb)
        }
    }

    pub fn group_key(&self) -> (String, String) {
        (self.make.clone(), self.model.clone())
    }

    /// The attachment/transport type for this disk (e.g., "sata", "nvme", "usb").
    /// Used for grouping — only disks with the same transport can be RAID'd together.
    pub fn device_type(&self) -> &str {
        &self.transport
    }

    /// Check if this disk is similar enough in size to another for RAID grouping.
    pub fn similar_size(&self, other: &DiskInfo) -> bool {
        let diff = self.size_bytes.abs_diff(other.size_bytes);
        diff <= SIZE_SIMILARITY_THRESHOLD
    }
}
