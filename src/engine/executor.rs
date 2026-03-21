use serde::{Deserialize, Serialize};

use crate::engine::feedback::OperationResult;
use crate::operations::Operation;

pub trait OperationExecutor {
    fn execute(&mut self, op: &Operation) -> OperationResult;
    fn recorded_operations(&self) -> &[RecordedOperation];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedOperation {
    pub operation: Operation,
    pub result: OperationResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedResponse {
    pub operation_match: OperationMatcher,
    pub result: OperationResult,
    #[serde(default)]
    pub consume: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationMatcher {
    Exact(Operation),
    ByType(String),
    Any,
}

impl OperationMatcher {
    pub fn matches(&self, op: &Operation) -> bool {
        match self {
            OperationMatcher::Exact(expected) => op == expected,
            OperationMatcher::ByType(type_name) => {
                let op_type = operation_type_name(op);
                op_type == type_name
            }
            OperationMatcher::Any => true,
        }
    }
}

pub fn operation_type_name(op: &Operation) -> &str {
    match op {
        Operation::EnableInterface { .. } => "EnableInterface",
        Operation::DisableInterface { .. } => "DisableInterface",
        Operation::ScanWifiNetworks { .. } => "ScanWifiNetworks",
        Operation::CheckLinkAvailability { .. } => "CheckLinkAvailability",
        Operation::AuthenticateWifi { .. } => "AuthenticateWifi",
        Operation::ConfigureWifiQrCode { .. } => "ConfigureWifiQrCode",
        Operation::ConfigureDhcp { .. } => "ConfigureDhcp",
        Operation::CheckIpAddress { .. } => "CheckIpAddress",
        Operation::CheckUpstreamRouter { .. } => "CheckUpstreamRouter",
        Operation::CheckInternetRoutability { .. } => "CheckInternetRoutability",
        Operation::CheckDnsResolution { .. } => "CheckDnsResolution",
        Operation::SelectPrimaryInterface { .. } => "SelectPrimaryInterface",
        Operation::ShutdownInterface { .. } => "ShutdownInterface",
        Operation::WifiConnectionTimeout { .. } => "WifiConnectionTimeout",
        Operation::WifiAuthError { .. } => "WifiAuthError",
        Operation::ConfigureWifiSsidAuth { .. } => "ConfigureWifiSsidAuth",
        Operation::ReceiveWifiScanResults { .. } => "ReceiveWifiScanResults",
        Operation::PartitionDisk { .. } => "PartitionDisk",
        Operation::MkfsBtrfs { .. } => "MkfsBtrfs",
        Operation::CreateBtrfsSubvolume { .. } => "CreateBtrfsSubvolume",
        Operation::BtrfsRaidSetup { .. } => "BtrfsRaidSetup",
        Operation::InstallBaseSystem { .. } => "InstallBaseSystem",
        Operation::Reboot => "Reboot",
        Operation::Exit => "Exit",
        Operation::Abort { .. } => "Abort",
        Operation::CleanupNetworkConfig { .. } => "CleanupNetworkConfig",
        Operation::CleanupWpaSupplicant { .. } => "CleanupWpaSupplicant",
        Operation::CleanupUnmount { .. } => "CleanupUnmount",
    }
}

// ---------------------------------------------------------------------------
// MockExecutor — deterministic responses for unit tests
// ---------------------------------------------------------------------------

pub struct MockExecutor {
    responses: Vec<SimulatedResponse>,
    recorded: Vec<RecordedOperation>,
}

impl MockExecutor {
    pub fn new(responses: Vec<SimulatedResponse>) -> Self {
        Self {
            responses,
            recorded: Vec::new(),
        }
    }

    fn find_response(&mut self, op: &Operation) -> OperationResult {
        let mut found_idx = None;
        for (i, resp) in self.responses.iter().enumerate() {
            if resp.operation_match.matches(op) {
                found_idx = Some(i);
                break;
            }
        }

        if let Some(idx) = found_idx {
            let result = self.responses[idx].result.clone();
            if self.responses[idx].consume {
                self.responses.remove(idx);
            }
            result
        } else {
            OperationResult::Success
        }
    }
}

impl OperationExecutor for MockExecutor {
    fn execute(&mut self, op: &Operation) -> OperationResult {
        let result = self.find_response(op);
        self.recorded.push(RecordedOperation {
            operation: op.clone(),
            result: result.clone(),
        });
        result
    }

    fn recorded_operations(&self) -> &[RecordedOperation] {
        &self.recorded
    }
}

/// Backward-compatible alias.
pub type TestExecutor = MockExecutor;

// ---------------------------------------------------------------------------
// SystemdExecutor — executes operations via systemd / dbus / shell commands
// ---------------------------------------------------------------------------

pub struct SystemdExecutor {
    recorded: Vec<RecordedOperation>,
}

impl SystemdExecutor {
    pub fn new() -> Self {
        Self {
            recorded: Vec::new(),
        }
    }
}

impl Default for SystemdExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl OperationExecutor for SystemdExecutor {
    fn execute(&mut self, op: &Operation) -> OperationResult {
        let result = super::real_ops::execute(op);
        self.recorded.push(RecordedOperation {
            operation: op.clone(),
            result: result.clone(),
        });
        result
    }

    fn recorded_operations(&self) -> &[RecordedOperation] {
        &self.recorded
    }
}

/// Backward-compatible alias.
pub type RealExecutor = SystemdExecutor;
