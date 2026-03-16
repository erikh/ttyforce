use serde::{Deserialize, Serialize};

use crate::manifest::{WifiNetworkSpec, WifiSecurity};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal_strength: i32,
    pub frequency_mhz: u32,
    pub security: WifiSecurity,
    pub reachable: bool,
}

impl From<&WifiNetworkSpec> for WifiNetwork {
    fn from(spec: &WifiNetworkSpec) -> Self {
        Self {
            ssid: spec.ssid.clone(),
            signal_strength: spec.signal_strength,
            frequency_mhz: spec.frequency_mhz,
            security: spec.security.clone(),
            reachable: spec.reachable,
        }
    }
}

impl WifiNetwork {
    pub fn signal_bars(&self) -> u8 {
        match self.signal_strength {
            s if s >= -50 => 4,
            s if s >= -60 => 3,
            s if s >= -70 => 2,
            s if s >= -80 => 1,
            _ => 0,
        }
    }

    pub fn signal_display(&self) -> &str {
        match self.signal_bars() {
            4 => "████",
            3 => "███░",
            2 => "██░░",
            1 => "█░░░",
            _ => "░░░░",
        }
    }

    pub fn security_display(&self) -> &str {
        match self.security {
            WifiSecurity::Open => "Open",
            WifiSecurity::Wpa2 => "WPA2",
            WifiSecurity::Wpa3 => "WPA3",
            WifiSecurity::Wep => "WEP",
        }
    }
}
