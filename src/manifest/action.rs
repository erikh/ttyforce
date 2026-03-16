use serde::{Deserialize, Serialize};

use crate::operations::Operation;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionManifest {
    pub operations: Vec<TimestampedOperation>,
    pub final_state: InstallerFinalState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimestampedOperation {
    pub sequence: u64,
    pub operation: Operation,
    pub result: OperationOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationOutcome {
    Success,
    Error(String),
    Timeout,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallerFinalState {
    Installed,
    Aborted,
    Rebooted,
    Error(String),
}

impl ActionManifest {
    pub fn new() -> Self {
        Self {
            operations: Vec::new(),
            final_state: InstallerFinalState::Aborted,
        }
    }

    pub fn record(&mut self, operation: Operation, result: OperationOutcome) {
        let sequence = self.operations.len() as u64;
        self.operations.push(TimestampedOperation {
            sequence,
            operation,
            result,
        });
    }

    pub fn save(&self, path: &str) -> anyhow::Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let manifest: Self = toml::from_str(&content)?;
        Ok(manifest)
    }
}

impl Default for ActionManifest {
    fn default() -> Self {
        Self::new()
    }
}
