use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RaidConfig {
    Single,
    BtrfsRaid1,
    BtrfsRaid5,
}

impl RaidConfig {
    pub fn for_disk_count(count: usize) -> Vec<RaidConfig> {
        match count {
            0 => vec![],
            1 => vec![RaidConfig::Single],
            2 => vec![RaidConfig::Single, RaidConfig::BtrfsRaid1],
            _ => {
                vec![RaidConfig::Single, RaidConfig::BtrfsRaid1, RaidConfig::BtrfsRaid5]
            }
        }
    }

    pub fn recommended_for_count(count: usize) -> RaidConfig {
        match count {
            0 | 1 => RaidConfig::Single,
            2 => RaidConfig::BtrfsRaid1,
            _ => RaidConfig::BtrfsRaid5,
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            RaidConfig::Single => "Single",
            RaidConfig::BtrfsRaid1 => "RAID1 (Btrfs)",
            RaidConfig::BtrfsRaid5 => "RAID5 (Btrfs)",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            RaidConfig::Single => "All data on one drive. No redundancy. Full capacity available.",
            RaidConfig::BtrfsRaid1 => "Mirrored. Survives one drive failure. 50% usable capacity.",
            RaidConfig::BtrfsRaid5 => "Striped with parity. Survives one drive failure. (N-1)/N usable capacity.",
        }
    }

    pub fn raid_level(&self) -> &str {
        match self {
            RaidConfig::Single => "stripe",
            RaidConfig::BtrfsRaid1 => "raid1",
            RaidConfig::BtrfsRaid5 => "raid5",
        }
    }

    pub fn usable_capacity(&self, total_bytes: u64, disk_count: usize) -> u64 {
        match self {
            RaidConfig::Single => total_bytes,
            RaidConfig::BtrfsRaid1 => total_bytes / 2,
            RaidConfig::BtrfsRaid5 => {
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
