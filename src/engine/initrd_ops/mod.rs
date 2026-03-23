pub mod network;
pub mod system;

use std::fs;

use crate::engine::feedback::OperationResult;
use crate::engine::real_ops::disk;
use crate::operations::Operation;

/// Execute an operation using initrd-compatible tools (no systemd dbus).
pub fn execute(op: &Operation) -> OperationResult {
    match op {
        // Network — ip link
        Operation::EnableInterface { interface } => network::enable_interface(interface),
        Operation::DisableInterface { interface } => network::disable_interface(interface),

        // Network — wifi (iw + wpa_supplicant CLI, no dbus)
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

        // Network — DHCP via dhcpcd
        Operation::ConfigureDhcp { interface } => network::configure_dhcp(interface),
        Operation::SelectPrimaryInterface { interface } => {
            network::select_primary_interface(interface)
        }
        Operation::ShutdownInterface { interface } => network::shutdown_interface(interface),

        // Network — checks (sysfs/ip/ping/getent, no dbus)
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

        // Disk — reuse real_ops (same tools: parted, mkfs.btrfs, mount)
        Operation::PartitionDisk { device } => disk::partition_disk(device),
        Operation::MkfsBtrfs { devices } => disk::mkfs_btrfs(devices),
        Operation::CreateBtrfsSubvolume { mount_point, name } => {
            disk::create_btrfs_subvolume(mount_point, name)
        }
        Operation::BtrfsRaidSetup {
            devices,
            raid_level,
        } => disk::btrfs_raid_setup(devices, raid_level),
        Operation::MountFilesystem {
            device,
            mount_point,
            fs_type,
        } => mount_filesystem_syscall(device, mount_point, fs_type),

        // Generate fstab
        Operation::GenerateFstab {
            mount_point,
            device,
            fs_type,
        } => disk::generate_fstab(mount_point, device, fs_type),

        // Persist network config to installed system
        Operation::PersistNetworkConfig {
            mount_point,
            interface,
        } => network::persist_network_config(mount_point, interface),

        // System
        Operation::InstallBaseSystem { target } => system::install_base_system(target),
        Operation::Reboot => system::reboot(),
        Operation::Exit => OperationResult::Success,
        Operation::Abort { .. } => OperationResult::Success,

        // Cleanup
        Operation::CleanupNetworkConfig { interface } => {
            network::cleanup_network_config(interface)
        }
        Operation::CleanupWpaSupplicant { interface } => {
            network::cleanup_wpa_supplicant(interface)
        }
        Operation::CleanupUnmount { mount_point } => unmount_syscall(mount_point),
    }
}

/// Mount a filesystem using the mount(2) syscall.
/// For btrfs, runs `btrfs device scan` first so RAID members are discovered.
fn mount_filesystem_syscall(device: &str, mount_point: &str, fs_type: &str) -> OperationResult {
    if let Err(e) = fs::create_dir_all(mount_point) {
        return OperationResult::Error(format!(
            "failed to create mount point {}: {}",
            mount_point, e
        ));
    }

    // For btrfs RAID arrays, scan for member devices before mounting
    if fs_type == "btrfs" {
        let _ = crate::engine::real_ops::run_cmd("btrfs", &["device", "scan"]);
    }

    match nix::mount::mount(
        Some(device),
        mount_point,
        Some(fs_type),
        nix::mount::MsFlags::empty(),
        None::<&str>,
    ) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!(
            "mount({}, {}, {}) failed: {}",
            device, mount_point, fs_type, e
        )),
    }
}

/// Unmount a filesystem using the umount2(2) syscall. Best-effort.
fn unmount_syscall(mount_point: &str) -> OperationResult {
    let _ = nix::mount::umount2(mount_point, nix::mount::MntFlags::MNT_DETACH);
    OperationResult::Success
}
