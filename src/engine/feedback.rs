use serde::{Deserialize, Serialize};

use crate::network::wifi::WifiNetwork;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OperationResult {
    Success,
    Error(String),
    Timeout,

    // Network-specific results
    LinkUp,
    LinkDown,
    IpAssigned(String),
    NoIp,
    RouterFound(String),
    NoRouter,
    InternetReachable,
    NoInternet,
    DnsResolved(String),
    DnsFailed(String),

    // Wifi-specific results
    WifiScanResults(Vec<WifiNetwork>),
    WifiAuthenticated,
    WifiAuthFailed(String),
    WifiTimeout,
    WifiConnected,
    WifiQrConfigured,
}

impl OperationResult {
    pub fn is_success(&self) -> bool {
        matches!(
            self,
            OperationResult::Success
                | OperationResult::LinkUp
                | OperationResult::IpAssigned(_)
                | OperationResult::RouterFound(_)
                | OperationResult::InternetReachable
                | OperationResult::DnsResolved(_)
                | OperationResult::WifiAuthenticated
                | OperationResult::WifiConnected
                | OperationResult::WifiQrConfigured
                | OperationResult::WifiScanResults(_)
        )
    }

    pub fn is_error(&self) -> bool {
        matches!(
            self,
            OperationResult::Error(_)
                | OperationResult::Timeout
                | OperationResult::LinkDown
                | OperationResult::NoIp
                | OperationResult::NoRouter
                | OperationResult::NoInternet
                | OperationResult::DnsFailed(_)
                | OperationResult::WifiAuthFailed(_)
                | OperationResult::WifiTimeout
        )
    }

    pub fn to_outcome(&self) -> crate::manifest::OperationOutcome {
        if self.is_success() {
            crate::manifest::OperationOutcome::Success
        } else if matches!(self, OperationResult::Timeout | OperationResult::WifiTimeout) {
            crate::manifest::OperationOutcome::Timeout
        } else {
            crate::manifest::OperationOutcome::Error(format!("{:?}", self))
        }
    }
}
