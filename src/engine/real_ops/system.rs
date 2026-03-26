use crate::engine::feedback::OperationResult;

use super::run_cmd;

/// Install the base system to the target mount point.
/// Runs install.sh if present, otherwise succeeds as a no-op
/// (Town OS is already installed via squashfs).
pub fn install_base_system(target: &str) -> OperationResult {
    let install_script = format!("{}/install.sh", target);
    if std::path::Path::new(&install_script).exists() {
        match run_cmd("sh", &[&install_script]) {
            Ok(_) => return OperationResult::Success,
            Err(e) => {
                return OperationResult::Error(format!("install script failed: {}", e));
            }
        }
    }

    OperationResult::Success
}

/// Power off the system.
/// Tries dbus (logind) first, falls back to systemctl poweroff.
pub fn power_off() -> OperationResult {
    if let Ok(conn) = zbus::blocking::Connection::system() {
        let result = conn.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "PowerOff",
            &(false,),
        );
        if result.is_ok() {
            return OperationResult::Success;
        }
    }

    match run_cmd("systemctl", &["poweroff"]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("poweroff failed: {}", e)),
    }
}

/// Stop all podman containers.
pub fn stop_all_containers() -> OperationResult {
    match run_cmd("podman", &["stop", "--all", "--time", "10"]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("podman stop --all failed: {}", e)),
    }
}

/// Wipe a disk's partition table and filesystem signatures.
pub fn wipe_disk(device: &str) -> OperationResult {
    if let Err(e) = run_cmd("wipefs", &["--all", device]) {
        return OperationResult::Error(format!("wipefs failed on {}: {}", device, e));
    }
    match run_cmd("sgdisk", &["--zap-all", device]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("sgdisk failed on {}: {}", device, e)),
    }
}

/// Reboot the system.
/// Tries dbus (logind) first, falls back to systemctl reboot.
pub fn reboot() -> OperationResult {
    // Try dbus: org.freedesktop.login1.Manager.Reboot
    if let Ok(conn) = zbus::blocking::Connection::system() {
        let result = conn.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Reboot",
            &(false,),
        );
        if result.is_ok() {
            return OperationResult::Success;
        }
    }

    // Fallback: systemctl reboot
    match run_cmd("systemctl", &["reboot"]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("reboot failed: {}", e)),
    }
}
