use crate::engine::feedback::OperationResult;

use super::run_cmd;

/// Install the base system to the target mount point.
/// This is a placeholder that runs a configurable install command.
pub fn install_base_system(target: &str) -> OperationResult {
    // Check if a custom install script exists
    let install_script = format!("{}/install.sh", target);
    if std::path::Path::new(&install_script).exists() {
        match run_cmd("sh", &[&install_script]) {
            Ok(_) => return OperationResult::Success,
            Err(e) => {
                return OperationResult::Error(format!("install script failed: {}", e));
            }
        }
    }

    // Default: try pacstrap-style install
    match run_cmd("pacstrap", &[target, "base", "linux", "linux-firmware"]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("base system install failed: {}", e)),
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
