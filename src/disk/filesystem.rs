use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemType {
    #[default]
    Btrfs,
    Zfs,
}

impl FilesystemType {
    pub fn display_name(&self) -> &str {
        match self {
            FilesystemType::Btrfs => "Btrfs",
            FilesystemType::Zfs => "ZFS",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            FilesystemType::Btrfs => "Modern copy-on-write filesystem. Recommended for most users. Built into the Linux kernel with excellent tooling support.",
            FilesystemType::Zfs => "Advanced filesystem with built-in volume management. Excellent data integrity features. Requires additional kernel module.",
        }
    }

    pub fn is_default(&self) -> bool {
        matches!(self, FilesystemType::Btrfs)
    }
}

impl std::fmt::Display for FilesystemType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}
