use serde::{Deserialize, Serialize};

use crate::manifest::DiskSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskInfo {
    pub device: String,
    pub make: String,
    pub model: String,
    pub size_bytes: u64,
    pub serial: Option<String>,
}

impl From<&DiskSpec> for DiskInfo {
    fn from(spec: &DiskSpec) -> Self {
        Self {
            device: spec.device.clone(),
            make: spec.make.clone(),
            model: spec.model.clone(),
            size_bytes: spec.size_bytes,
            serial: spec.serial.clone(),
        }
    }
}

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
}
