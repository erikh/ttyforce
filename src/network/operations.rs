use crate::manifest::InterfaceKind;
use crate::network::interface::NetworkInterface;
use crate::operations::Operation;

pub fn bring_online_ethernet(iface: &NetworkInterface) -> Vec<Operation> {
    vec![
        Operation::EnableInterface {
            interface: iface.name.clone(),
        },
        Operation::CheckLinkAvailability {
            interface: iface.name.clone(),
        },
        Operation::ConfigureDhcp {
            interface: iface.name.clone(),
        },
        Operation::CheckIpAddress {
            interface: iface.name.clone(),
        },
        Operation::CheckUpstreamRouter {
            interface: iface.name.clone(),
        },
        Operation::CheckInternetRoutability {
            interface: iface.name.clone(),
        },
        Operation::CheckDnsResolution {
            interface: iface.name.clone(),
            hostname: "example.com".to_string(),
        },
        Operation::SelectPrimaryInterface {
            interface: iface.name.clone(),
        },
    ]
}

pub fn bring_online_wifi(iface: &NetworkInterface, ssid: &str, password: &str) -> Vec<Operation> {
    vec![
        Operation::EnableInterface {
            interface: iface.name.clone(),
        },
        Operation::ScanWifiNetworks {
            interface: iface.name.clone(),
        },
        Operation::ReceiveWifiScanResults {
            interface: iface.name.clone(),
        },
        Operation::ConfigureWifiSsidAuth {
            interface: iface.name.clone(),
            ssid: ssid.to_string(),
            password: password.to_string(),
        },
        Operation::AuthenticateWifi {
            interface: iface.name.clone(),
            ssid: ssid.to_string(),
            password: password.to_string(),
        },
        Operation::ConfigureDhcp {
            interface: iface.name.clone(),
        },
        Operation::CheckIpAddress {
            interface: iface.name.clone(),
        },
        Operation::CheckUpstreamRouter {
            interface: iface.name.clone(),
        },
        Operation::CheckInternetRoutability {
            interface: iface.name.clone(),
        },
        Operation::CheckDnsResolution {
            interface: iface.name.clone(),
            hostname: "example.com".to_string(),
        },
        Operation::SelectPrimaryInterface {
            interface: iface.name.clone(),
        },
    ]
}

pub fn bring_online_wifi_qr(iface: &NetworkInterface, qr_data: &str) -> Vec<Operation> {
    vec![
        Operation::EnableInterface {
            interface: iface.name.clone(),
        },
        Operation::ConfigureWifiQrCode {
            interface: iface.name.clone(),
            qr_data: qr_data.to_string(),
        },
        Operation::ConfigureDhcp {
            interface: iface.name.clone(),
        },
        Operation::CheckIpAddress {
            interface: iface.name.clone(),
        },
        Operation::CheckUpstreamRouter {
            interface: iface.name.clone(),
        },
        Operation::CheckInternetRoutability {
            interface: iface.name.clone(),
        },
        Operation::CheckDnsResolution {
            interface: iface.name.clone(),
            hostname: "example.com".to_string(),
        },
        Operation::SelectPrimaryInterface {
            interface: iface.name.clone(),
        },
    ]
}

pub fn shutdown_non_primary(
    interfaces: &[NetworkInterface],
    primary: &str,
) -> Vec<Operation> {
    interfaces
        .iter()
        .filter(|i| i.name != primary && i.enabled)
        .map(|i| Operation::ShutdownInterface {
            interface: i.name.clone(),
        })
        .collect()
}

pub fn default_interface_priority(interfaces: &[NetworkInterface]) -> Vec<&NetworkInterface> {
    let mut sorted: Vec<&NetworkInterface> = interfaces.iter().collect();
    sorted.sort_by(|a, b| {
        let a_score = interface_priority_score(a);
        let b_score = interface_priority_score(b);
        b_score.cmp(&a_score)
    });
    sorted
}

fn interface_priority_score(iface: &NetworkInterface) -> u32 {
    let mut score = 0;
    if iface.kind == InterfaceKind::Ethernet {
        score += 100;
    }
    if iface.has_link {
        score += 50;
    }
    if iface.has_carrier {
        score += 25;
    }
    score
}
