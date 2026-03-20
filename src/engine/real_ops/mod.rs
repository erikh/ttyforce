pub mod disk;
pub mod network;
pub mod system;

use std::process::Command;

use crate::engine::feedback::OperationResult;
use crate::operations::Operation;

/// Execute an operation using real system commands and dbus calls.
pub fn execute(op: &Operation) -> OperationResult {
    match op {
        // Network — dbus / shell
        Operation::EnableInterface { interface } => network::enable_interface(interface),
        Operation::DisableInterface { interface } => network::disable_interface(interface),
        Operation::ScanWifiNetworks { interface } => network::scan_wifi_networks(interface),
        Operation::ReceiveWifiScanResults { interface } => {
            network::receive_wifi_scan_results(interface)
        }
        Operation::AuthenticateWifi {
            interface,
            ssid,
            password,
        } => network::authenticate_wifi(interface, ssid, password),
        Operation::ConfigureWifiSsidAuth {
            interface,
            ssid,
            password,
        } => network::configure_wifi_ssid_auth(interface, ssid, password),
        Operation::ConfigureWifiQrCode {
            interface,
            qr_data,
        } => network::configure_wifi_qr_code(interface, qr_data),
        Operation::ConfigureDhcp { interface } => network::configure_dhcp(interface),
        Operation::SelectPrimaryInterface { interface } => {
            network::select_primary_interface(interface)
        }
        Operation::ShutdownInterface { interface } => network::shutdown_interface(interface),

        // Network — checks
        Operation::CheckLinkAvailability { interface } => {
            network::check_link_availability(interface)
        }
        Operation::CheckIpAddress { interface } => network::check_ip_address(interface),
        Operation::CheckUpstreamRouter { interface } => {
            network::check_upstream_router(interface)
        }
        Operation::CheckInternetRoutability { interface } => {
            network::check_internet_routability(interface)
        }
        Operation::CheckDnsResolution {
            interface,
            hostname,
        } => network::check_dns_resolution(interface, hostname),

        // Network — state records (no system action)
        Operation::WifiConnectionTimeout { .. } => OperationResult::WifiTimeout,
        Operation::WifiAuthError { .. } => OperationResult::WifiAuthFailed("auth error".into()),

        // Disk
        Operation::PartitionDisk { device } => disk::partition_disk(device),
        Operation::MkfsBtrfs { devices } => disk::mkfs_btrfs(devices),
        Operation::CreateBtrfsSubvolume { mount_point, name } => {
            disk::create_btrfs_subvolume(mount_point, name)
        }
        Operation::BtrfsRaidSetup {
            devices,
            raid_level,
        } => disk::btrfs_raid_setup(devices, raid_level),

        // System
        Operation::InstallBaseSystem { target } => system::install_base_system(target),
        Operation::Reboot => system::reboot(),
        Operation::Exit => OperationResult::Success,
        Operation::Abort { .. } => OperationResult::Success,
    }
}

/// Run a command and return stdout on success or stderr on failure.
pub fn run_cmd(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{}: {}", program, e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}
