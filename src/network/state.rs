use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NetworkState {
    Offline,
    DeviceEnabled,
    Scanning,
    NetworkSelected,
    Authenticating,
    Connected,
    DhcpConfiguring,
    IpAssigned,
    CheckingRouter,
    CheckingInternet,
    CheckingDns,
    Online,
    Error(String),
}

impl NetworkState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, NetworkState::Online | NetworkState::Error(_))
    }

    pub fn is_online(&self) -> bool {
        matches!(self, NetworkState::Online)
    }

    pub fn next_for_ethernet(&self) -> Option<NetworkState> {
        match self {
            NetworkState::Offline => Some(NetworkState::DeviceEnabled),
            NetworkState::DeviceEnabled => Some(NetworkState::DhcpConfiguring),
            NetworkState::DhcpConfiguring => Some(NetworkState::IpAssigned),
            NetworkState::IpAssigned => Some(NetworkState::CheckingRouter),
            NetworkState::CheckingRouter => Some(NetworkState::CheckingInternet),
            NetworkState::CheckingInternet => Some(NetworkState::CheckingDns),
            NetworkState::CheckingDns => Some(NetworkState::Online),
            _ => None,
        }
    }

    pub fn next_for_wifi(&self) -> Option<NetworkState> {
        match self {
            NetworkState::Offline => Some(NetworkState::DeviceEnabled),
            NetworkState::DeviceEnabled => Some(NetworkState::Scanning),
            NetworkState::Scanning => Some(NetworkState::NetworkSelected),
            NetworkState::NetworkSelected => Some(NetworkState::Authenticating),
            NetworkState::Authenticating => Some(NetworkState::Connected),
            NetworkState::Connected => Some(NetworkState::DhcpConfiguring),
            NetworkState::DhcpConfiguring => Some(NetworkState::IpAssigned),
            NetworkState::IpAssigned => Some(NetworkState::CheckingRouter),
            NetworkState::CheckingRouter => Some(NetworkState::CheckingInternet),
            NetworkState::CheckingInternet => Some(NetworkState::CheckingDns),
            NetworkState::CheckingDns => Some(NetworkState::Online),
            _ => None,
        }
    }
}

impl std::fmt::Display for NetworkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkState::Offline => write!(f, "Offline"),
            NetworkState::DeviceEnabled => write!(f, "Device Enabled"),
            NetworkState::Scanning => write!(f, "Scanning"),
            NetworkState::NetworkSelected => write!(f, "Network Selected"),
            NetworkState::Authenticating => write!(f, "Authenticating"),
            NetworkState::Connected => write!(f, "Connected"),
            NetworkState::DhcpConfiguring => write!(f, "Configuring DHCP"),
            NetworkState::IpAssigned => write!(f, "IP Assigned"),
            NetworkState::CheckingRouter => write!(f, "Checking Router"),
            NetworkState::CheckingInternet => write!(f, "Checking Internet"),
            NetworkState::CheckingDns => write!(f, "Checking DNS"),
            NetworkState::Online => write!(f, "Online"),
            NetworkState::Error(e) => write!(f, "Error: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ethernet_state_progression() {
        let mut state = NetworkState::Offline;
        let expected = vec![
            NetworkState::DeviceEnabled,
            NetworkState::DhcpConfiguring,
            NetworkState::IpAssigned,
            NetworkState::CheckingRouter,
            NetworkState::CheckingInternet,
            NetworkState::CheckingDns,
            NetworkState::Online,
        ];
        for exp in expected {
            state = state.next_for_ethernet().unwrap();
            assert_eq!(state, exp);
        }
        assert!(state.next_for_ethernet().is_none());
    }

    #[test]
    fn test_wifi_state_progression() {
        let mut state = NetworkState::Offline;
        let expected = vec![
            NetworkState::DeviceEnabled,
            NetworkState::Scanning,
            NetworkState::NetworkSelected,
            NetworkState::Authenticating,
            NetworkState::Connected,
            NetworkState::DhcpConfiguring,
            NetworkState::IpAssigned,
            NetworkState::CheckingRouter,
            NetworkState::CheckingInternet,
            NetworkState::CheckingDns,
            NetworkState::Online,
        ];
        for exp in expected {
            state = state.next_for_wifi().unwrap();
            assert_eq!(state, exp);
        }
        assert!(state.next_for_wifi().is_none());
    }

    #[test]
    fn test_terminal_states() {
        assert!(NetworkState::Online.is_terminal());
        assert!(NetworkState::Error("test".to_string()).is_terminal());
        assert!(!NetworkState::Offline.is_terminal());
        assert!(!NetworkState::DhcpConfiguring.is_terminal());
    }

    #[test]
    fn test_error_state_no_next() {
        let error = NetworkState::Error("test".to_string());
        assert!(error.next_for_ethernet().is_none());
        assert!(error.next_for_wifi().is_none());
    }
}
