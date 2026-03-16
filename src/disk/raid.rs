use serde::{Deserialize, Serialize};

use super::filesystem::FilesystemType;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RaidConfig {
    Single,
    Mirror,
    RaidZ,
    BtrfsRaid1,
    BtrfsRaid5,
}

impl RaidConfig {
    pub fn for_disk_count(count: usize, fs: &FilesystemType) -> Vec<RaidConfig> {
        match (fs, count) {
            (_, 0) => vec![],
            (FilesystemType::Btrfs, 1) => vec![RaidConfig::Single],
            (FilesystemType::Btrfs, 2) => vec![RaidConfig::Single, RaidConfig::BtrfsRaid1],
            (FilesystemType::Btrfs, _) => {
                vec![RaidConfig::Single, RaidConfig::BtrfsRaid1, RaidConfig::BtrfsRaid5]
            }
            (FilesystemType::Zfs, 1) => vec![RaidConfig::Single],
            (FilesystemType::Zfs, 2) => vec![RaidConfig::Single, RaidConfig::Mirror],
            (FilesystemType::Zfs, _) => {
                vec![RaidConfig::Single, RaidConfig::Mirror, RaidConfig::RaidZ]
            }
        }
    }

    pub fn recommended_for_count(count: usize, fs: &FilesystemType) -> RaidConfig {
        match (fs, count) {
            (_, 0 | 1) => RaidConfig::Single,
            (FilesystemType::Btrfs, 2) => RaidConfig::BtrfsRaid1,
            (FilesystemType::Btrfs, _) => RaidConfig::BtrfsRaid5,
            (FilesystemType::Zfs, 2) => RaidConfig::Mirror,
            (FilesystemType::Zfs, _) => RaidConfig::RaidZ,
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            RaidConfig::Single => "Single",
            RaidConfig::Mirror => "Mirror (ZFS)",
            RaidConfig::RaidZ => "RAID-Z (ZFS)",
            RaidConfig::BtrfsRaid1 => "RAID1 (Btrfs)",
            RaidConfig::BtrfsRaid5 => "RAID5 (Btrfs)",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            RaidConfig::Single => "All data on one drive. No redundancy. Full capacity available.",
            RaidConfig::Mirror => "Mirrored drives. Survives one drive failure. 50% usable capacity.",
            RaidConfig::RaidZ => "Striped with parity. Survives one drive failure. (N-1)/N usable capacity.",
            RaidConfig::BtrfsRaid1 => "Mirrored. Survives one drive failure. 50% usable capacity.",
            RaidConfig::BtrfsRaid5 => "Striped with parity. Survives one drive failure. (N-1)/N usable capacity.",
        }
    }

    pub fn zfs_vdev_type(&self) -> &str {
        match self {
            RaidConfig::Single => "stripe",
            RaidConfig::Mirror => "mirror",
            RaidConfig::RaidZ => "raidz",
            RaidConfig::BtrfsRaid1 => "raid1",
            RaidConfig::BtrfsRaid5 => "raid5",
        }
    }

    pub fn usable_capacity(&self, total_bytes: u64, disk_count: usize) -> u64 {
        match self {
            RaidConfig::Single => total_bytes,
            RaidConfig::Mirror | RaidConfig::BtrfsRaid1 => total_bytes / 2,
            RaidConfig::RaidZ | RaidConfig::BtrfsRaid5 => {
                if disk_count > 0 {
                    total_bytes * (disk_count as u64 - 1) / disk_count as u64
                } else {
                    0
                }
            }
        }
    }
}

impl std::fmt::Display for RaidConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}
