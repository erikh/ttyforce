use thiserror::Error;

#[derive(Error, Debug)]
pub enum TtyforceError {
    #[error("network error: {0}")]
    Network(String),

    #[error("disk error: {0}")]
    Disk(String),

    #[error("wifi authentication failed: {0}")]
    WifiAuthFailed(String),

    #[error("wifi connection timeout")]
    WifiTimeout,

    #[error("no network interfaces available")]
    NoNetworkInterfaces,

    #[error("no disks available")]
    NoDisks,

    #[error("invalid hardware manifest: {0}")]
    InvalidManifest(String),

    #[error("operation failed: {0}")]
    OperationFailed(String),

    #[error("dns resolution failed: {0}")]
    DnsResolutionFailed(String),

    #[error("no internet connectivity")]
    NoInternet,

    #[error("dhcp failed: {0}")]
    DhcpFailed(String),

    #[error("install aborted by user")]
    Aborted,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, TtyforceError>;
