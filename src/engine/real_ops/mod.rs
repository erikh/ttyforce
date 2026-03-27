pub mod disk;
pub mod network;
pub mod system;

use std::io::Write;
use std::process::Command;
use std::sync::Mutex;

use crate::engine::feedback::OperationResult;
use crate::operations::Operation;

// ── Global command log ──────────────────────────────────────────────────

static CMD_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Get a snapshot of the command log.
pub fn cmd_log() -> Vec<String> {
    CMD_LOG.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Append a message to the command log.
pub fn cmd_log_append(msg: String) {
    if let Ok(mut log) = CMD_LOG.lock() {
        log.push(msg);
    }
}

/// Best-effort write to /dev/kmsg (kernel log) for initrd debugging.
/// Messages are prefixed with "ttyforce: " so they can be identified in dmesg.
pub fn kmsg_log(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        if let Err(e) = writeln!(f, "ttyforce: {}", msg) {
            eprintln!("kmsg write: {}", e);
        }
    }
}

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

        // WPS push-button (reuse initrd implementation — same wpa_cli approach)
        Operation::WpsPbcStart { interface } => {
            crate::engine::initrd_ops::network::wps_pbc_start(interface)
        }
        Operation::WpsPbcStatus { interface } => {
            crate::engine::initrd_ops::network::wps_pbc_status(interface)
        }

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
        Operation::MountFilesystem {
            device,
            mount_point,
            fs_type,
            ref options,
        } => disk::mount_filesystem(device, mount_point, fs_type, options.as_deref()),

        // System
        Operation::InstallBaseSystem { target } => system::install_base_system(target),
        Operation::Reboot => system::reboot(),
        Operation::Exit => OperationResult::Success,
        Operation::Abort { .. } => OperationResult::Success,

        // Generate fstab
        Operation::GenerateFstab {
            mount_point,
            device,
            fs_type,
        } => disk::generate_fstab(mount_point, device, fs_type),

        // Persist network config — no-op for systemd executor (config already in place)
        Operation::PersistNetworkConfig { .. } => OperationResult::Success,

        // Cleanup
        Operation::CleanupNetworkConfig { interface } => {
            network::cleanup_network_config(interface)
        }
        Operation::CleanupWpaSupplicant { interface } => {
            network::cleanup_wpa_supplicant(interface)
        }
        Operation::CleanupUnmount { mount_point } => disk::cleanup_unmount(mount_point),

        // Getty operations
        Operation::PowerOff => system::power_off(),
        Operation::StopAllContainers => system::stop_all_containers(),
        Operation::WipeDisk { device } => system::wipe_disk(device),
    }
}

/// Run a command and return stdout on success or stderr on failure.
/// Logs the command and its output to the global command log and /dev/console.
pub fn run_cmd(program: &str, args: &[&str]) -> Result<String, String> {
    let cmd_str = if args.is_empty() {
        program.to_string()
    } else {
        format!("{} {}", program, args.join(" "))
    };
    cmd_log_append(format!("$ {}", cmd_str));

    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| {
            let msg = format!("{}: {}", program, e);
            cmd_log_append(format!("  error: {}", msg));
            msg
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Log output (truncate long output)
    for line in stdout.lines().take(5) {
        if !line.trim().is_empty() {
            cmd_log_append(format!("  {}", line));
        }
    }
    if stdout.lines().count() > 5 {
        cmd_log_append(format!("  ... ({} more lines)", stdout.lines().count() - 5));
    }
    for line in stderr.lines().take(3) {
        if !line.trim().is_empty() {
            cmd_log_append(format!("  err: {}", line));
        }
    }

    if output.status.success() {
        cmd_log_append(format!("  -> ok (exit {})", output.status.code().unwrap_or(0)));
        Ok(stdout)
    } else {
        let code = output.status.code().unwrap_or(-1);
        cmd_log_append(format!("  -> FAILED (exit {})", code));
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cmd_log_append_and_read() {
        cmd_log_append("test_marker_append".to_string());
        let log = cmd_log();
        assert!(log.iter().any(|l| l == "test_marker_append"));
    }

    #[test]
    fn test_run_cmd_logs_success() {
        // Use a unique marker so parallel tests don't interfere
        let result = run_cmd("echo", &["unique_success_marker_42"]);
        assert!(result.is_ok());
        let log = cmd_log();
        assert!(log.iter().any(|l| l.contains("$ echo unique_success_marker_42")));
        assert!(log.iter().any(|l| l.contains("unique_success_marker_42") && !l.starts_with('$')));
    }

    #[test]
    fn test_run_cmd_logs_failure() {
        let result = run_cmd("false", &[]);
        assert!(result.is_err());
        let log = cmd_log();
        assert!(log.iter().any(|l| l.contains("$ false")));
        assert!(log.iter().any(|l| l.contains("FAILED")));
    }

    #[test]
    fn test_run_cmd_logs_nonexistent_command() {
        let result = run_cmd("nonexistent_command_xyz_99", &[]);
        assert!(result.is_err());
        let log = cmd_log();
        assert!(log.iter().any(|l| l.contains("$ nonexistent_command_xyz_99")));
        assert!(log.iter().any(|l| l.contains("error:") && l.contains("nonexistent_command_xyz_99")));
    }

    #[test]
    fn test_kmsg_log_does_not_panic() {
        // kmsg_log is best-effort — it should not panic even if /dev/kmsg
        // is unavailable (e.g., in CI or non-root environments).
        kmsg_log("test message from ttyforce unit test");
    }
}
