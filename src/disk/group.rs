use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::info::DiskInfo;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGroup {
    pub make: String,
    pub model: String,
    pub disks: Vec<DiskInfo>,
}

impl DiskGroup {
    pub fn from_disks(disks: &[DiskInfo]) -> Vec<DiskGroup> {
        let mut groups: BTreeMap<(String, String), Vec<DiskInfo>> = BTreeMap::new();
        for disk in disks {
            groups
                .entry(disk.group_key())
                .or_default()
                .push(disk.clone());
        }
        groups
            .into_iter()
            .map(|((make, model), disks)| DiskGroup { make, model, disks })
            .collect()
    }

    pub fn disk_count(&self) -> usize {
        self.disks.len()
    }

    pub fn total_bytes(&self) -> u64 {
        self.disks.iter().map(|d| d.size_bytes).sum()
    }

    pub fn total_human(&self) -> String {
        let gb = self.total_bytes() as f64 / 1_073_741_824.0;
        if gb >= 1024.0 {
            format!("{:.1} TB", gb / 1024.0)
        } else {
            format!("{:.1} GB", gb)
        }
    }

    pub fn display_name(&self) -> String {
        format!(
            "{} {} ({}x, {} total)",
            self.make,
            self.model,
            self.disk_count(),
            self.total_human()
        )
    }

    pub fn device_paths(&self) -> Vec<String> {
        self.disks.iter().map(|d| d.device.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_disk(device: &str, make: &str, model: &str, size: u64) -> DiskInfo {
        DiskInfo {
            device: device.to_string(),
            make: make.to_string(),
            model: model.to_string(),
            size_bytes: size,
            serial: None,
        }
    }

    #[test]
    fn test_group_same_disks() {
        let disks = vec![
            make_disk("/dev/sda", "Samsung", "870 EVO", 1_000_000_000_000),
            make_disk("/dev/sdb", "Samsung", "870 EVO", 1_000_000_000_000),
            make_disk("/dev/sdc", "Samsung", "870 EVO", 1_000_000_000_000),
        ];
        let groups = DiskGroup::from_disks(&disks);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].disk_count(), 3);
        assert_eq!(groups[0].total_bytes(), 3_000_000_000_000);
    }

    #[test]
    fn test_group_mixed_disks() {
        let disks = vec![
            make_disk("/dev/sda", "Samsung", "870 EVO", 1_000_000_000_000),
            make_disk("/dev/sdb", "Samsung", "870 EVO", 1_000_000_000_000),
            make_disk("/dev/sdc", "WD", "Blue", 500_000_000_000),
        ];
        let groups = DiskGroup::from_disks(&disks);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn test_group_empty() {
        let groups = DiskGroup::from_disks(&[]);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_total_human_gb() {
        let disks = vec![make_disk("/dev/sda", "Test", "Drive", 500_000_000_000)];
        let groups = DiskGroup::from_disks(&disks);
        let human = groups[0].total_human();
        assert!(human.contains("GB"));
    }

    #[test]
    fn test_total_human_tb() {
        let disks = vec![make_disk("/dev/sda", "Test", "Drive", 2_000_000_000_000)];
        let groups = DiskGroup::from_disks(&disks);
        let human = groups[0].total_human();
        assert!(human.contains("TB"));
    }

    #[test]
    fn test_device_paths() {
        let disks = vec![
            make_disk("/dev/sda", "Samsung", "870 EVO", 1_000_000_000_000),
            make_disk("/dev/sdb", "Samsung", "870 EVO", 1_000_000_000_000),
        ];
        let groups = DiskGroup::from_disks(&disks);
        let paths = groups[0].device_paths();
        assert_eq!(paths, vec!["/dev/sda", "/dev/sdb"]);
    }
}
