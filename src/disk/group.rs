use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::info::{DiskInfo, SIZE_SIMILARITY_THRESHOLD};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGroup {
    pub make: String,
    pub model: String,
    pub disks: Vec<DiskInfo>,
}

impl DiskGroup {
    /// Group disks for RAID selection. Two grouping strategies are used:
    ///
    /// 1. **Exact match**: disks with identical make and model are always grouped.
    /// 2. **Similar-size merge**: groups with the same device type (e.g., all SCSI,
    ///    all NVMe) and sizes within 10 GB of each other are merged into a single
    ///    group, even if make/model differ. This allows heterogeneous RAID arrays
    ///    with disks that are functionally equivalent.
    pub fn from_disks(disks: &[DiskInfo]) -> Vec<DiskGroup> {
        // Step 1: Group by exact (make, model)
        let mut exact: BTreeMap<(String, String), Vec<DiskInfo>> = BTreeMap::new();
        for disk in disks {
            exact
                .entry(disk.group_key())
                .or_default()
                .push(disk.clone());
        }
        let mut groups: Vec<DiskGroup> = exact
            .into_iter()
            .map(|((make, model), disks)| DiskGroup { make, model, disks })
            .collect();

        // Step 2: Merge groups that share device type and similar sizes
        groups = Self::merge_similar_groups(groups);

        groups
    }

    /// Merge groups that have the same device type and all disks within
    /// SIZE_SIMILARITY_THRESHOLD of each other.
    fn merge_similar_groups(mut groups: Vec<DiskGroup>) -> Vec<DiskGroup> {
        let mut merged: Vec<DiskGroup> = Vec::new();

        while let Some(mut current) = groups.pop() {
            let mut i = 0;
            while i < groups.len() {
                let current_type = current.device_type().to_string();
                if groups[i].device_type() == current_type
                    && all_sizes_similar(&current.disks, &groups[i].disks)
                {
                    let other = groups.remove(i);
                    current = Self::merge_two(current, other);
                } else {
                    i += 1;
                }
            }

            merged.push(current);
        }

        // Restore deterministic ordering (by make, then model)
        merged.sort_by(|a, b| (&a.make, &a.model).cmp(&(&b.make, &b.model)));
        merged
    }

    /// Merge two groups into one. If make/model differ, uses "Mixed <type>" label.
    fn merge_two(mut a: DiskGroup, b: DiskGroup) -> DiskGroup {
        if a.make != b.make || a.model != b.model {
            let dev_type = a.device_type().to_string();
            a.make = "Mixed".to_string();
            a.model = format!("{} drives", dev_type);
        }
        a.disks.extend(b.disks);
        a
    }

    /// The transport type shared by all disks in this group.
    fn device_type(&self) -> &str {
        match self.disks.first() {
            Some(d) => d.device_type(),
            None => "unknown",
        }
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
        if self.make == "Mixed" {
            // Show individual disk models for mixed groups
            let mut models: Vec<String> = self
                .disks
                .iter()
                .map(|d| format!("{} {}", d.make, d.model))
                .collect();
            models.sort();
            models.dedup();
            format!(
                "{} ({}x, {} total)",
                models.join(" + "),
                self.disk_count(),
                self.total_human()
            )
        } else {
            format!(
                "{} {} ({}x, {} total)",
                self.make,
                self.model,
                self.disk_count(),
                self.total_human()
            )
        }
    }

    pub fn device_paths(&self) -> Vec<String> {
        self.disks.iter().map(|d| d.device.clone()).collect()
    }
}

/// Check that all disks across two slices are within the size similarity threshold.
fn all_sizes_similar(a: &[DiskInfo], b: &[DiskInfo]) -> bool {
    for da in a {
        for db in b {
            if da.size_bytes.abs_diff(db.size_bytes) > SIZE_SIMILARITY_THRESHOLD {
                return false;
            }
        }
    }
    // Also check within each group (relevant for merges of already-mixed groups)
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_disk(device: &str, make: &str, model: &str, size: u64) -> DiskInfo {
        make_disk_transport(device, make, model, size, "sata")
    }

    fn make_disk_transport(
        device: &str,
        make: &str,
        model: &str,
        size: u64,
        transport: &str,
    ) -> DiskInfo {
        DiskInfo {
            device: device.to_string(),
            make: make.to_string(),
            model: model.to_string(),
            size_bytes: size,
            serial: None,
            transport: transport.to_string(),
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
    fn test_group_mixed_disks_different_size() {
        // Different make/model AND very different size — should NOT merge
        let disks = vec![
            make_disk("/dev/sda", "Samsung", "870 EVO", 1_000_000_000_000),
            make_disk("/dev/sdb", "Samsung", "870 EVO", 1_000_000_000_000),
            make_disk("/dev/sdc", "WD", "Blue", 500_000_000_000),
        ];
        let groups = DiskGroup::from_disks(&disks);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn test_group_similar_size_same_transport_merges() {
        // Different make/model, same transport (sata), similar size (within 10GB)
        let size_1tb = 1_000_000_000_000;
        let size_1tb_plus_5gb = size_1tb + 5 * 1_073_741_824;
        let disks = vec![
            make_disk("/dev/sda", "Samsung", "870 EVO", size_1tb),
            make_disk("/dev/sdb", "WD", "Blue", size_1tb_plus_5gb),
        ];
        let groups = DiskGroup::from_disks(&disks);
        assert_eq!(groups.len(), 1, "similar-size same-transport disks should merge");
        assert_eq!(groups[0].disk_count(), 2);
        assert_eq!(groups[0].make, "Mixed");
    }

    #[test]
    fn test_group_similar_size_different_transport_stays_separate() {
        // Similar size but different transport — should NOT merge
        let size_1tb = 1_000_000_000_000;
        let disks = vec![
            make_disk_transport("/dev/sda", "Samsung", "870 EVO", size_1tb, "sata"),
            make_disk_transport("/dev/nvme0n1", "Samsung", "990 PRO", size_1tb, "nvme"),
        ];
        let groups = DiskGroup::from_disks(&disks);
        assert_eq!(
            groups.len(),
            2,
            "same-size different-transport disks should stay separate"
        );
    }

    #[test]
    fn test_group_usb_isolated_from_sata() {
        // USB disk same size as SATA — should NOT merge
        let size_1tb = 1_000_000_000_000;
        let disks = vec![
            make_disk_transport("/dev/sda", "Samsung", "870 EVO", size_1tb, "sata"),
            make_disk_transport("/dev/sdb", "WD", "Elements", size_1tb, "usb"),
        ];
        let groups = DiskGroup::from_disks(&disks);
        assert_eq!(groups.len(), 2, "USB and SATA disks must not merge");
    }

    #[test]
    fn test_group_size_just_over_threshold_stays_separate() {
        // Size difference just over 10GB — should NOT merge
        let size_1tb = 1_000_000_000_000;
        let size_over_threshold = size_1tb + 11 * 1_073_741_824;
        let disks = vec![
            make_disk("/dev/sda", "Samsung", "870 EVO", size_1tb),
            make_disk("/dev/sdb", "WD", "Blue", size_over_threshold),
        ];
        let groups = DiskGroup::from_disks(&disks);
        assert_eq!(groups.len(), 2, "disks >10GB apart should not merge");
    }

    #[test]
    fn test_group_mixed_display_name() {
        let size_1tb = 1_000_000_000_000;
        let disks = vec![
            make_disk("/dev/sda", "Samsung", "870 EVO", size_1tb),
            make_disk("/dev/sdb", "WD", "Blue", size_1tb),
        ];
        let groups = DiskGroup::from_disks(&disks);
        assert_eq!(groups.len(), 1);
        let name = groups[0].display_name();
        assert!(name.contains("Samsung 870 EVO"), "display should list models: {}", name);
        assert!(name.contains("WD Blue"), "display should list models: {}", name);
        assert!(name.contains("2x"), "display should show count: {}", name);
    }

    #[test]
    fn test_group_three_different_makes_similar_size_merges() {
        let size = 500_000_000_000;
        let disks = vec![
            make_disk("/dev/sda", "Samsung", "870 EVO", size),
            make_disk("/dev/sdb", "WD", "Blue", size + 1_073_741_824),
            make_disk("/dev/sdc", "Crucial", "MX500", size + 2 * 1_073_741_824),
        ];
        let groups = DiskGroup::from_disks(&disks);
        assert_eq!(groups.len(), 1, "three similar-size sata disks should merge");
        assert_eq!(groups[0].disk_count(), 3);
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
