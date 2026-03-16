use serde::{Deserialize, Serialize};

use crate::manifest::{InterfaceKind, NetworkInterfaceSpec};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub name: String,
    pub kind: InterfaceKind,
    pub mac: String,
    pub has_link: bool,
    pub has_carrier: bool,
    pub ip_address: Option<String>,
    pub is_primary: bool,
    pub enabled: bool,
}

impl From<&NetworkInterfaceSpec> for NetworkInterface {
    fn from(spec: &NetworkInterfaceSpec) -> Self {
        Self {
            name: spec.name.clone(),
            kind: spec.kind.clone(),
            mac: spec.mac.clone(),
            has_link: spec.has_link,
            has_carrier: spec.has_carrier,
            ip_address: None,
            is_primary: false,
            enabled: false,
        }
    }
}
