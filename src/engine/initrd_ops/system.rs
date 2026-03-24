use crate::engine::feedback::OperationResult;

use crate::engine::real_ops::run_cmd;

/// Install the base system to the target mount point.
/// Same as systemd executor — uses install.sh or pacstrap.
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

    match run_cmd("pacstrap", &[target, "base", "linux", "linux-firmware"]) {
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("base system install failed: {}", e)),
    }
}

/// Reboot the system using the reboot(2) syscall.
pub fn reboot() -> OperationResult {
    nix::unistd::sync();
    match nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_AUTOBOOT) {
        // reboot() doesn't return on success, but the type system requires this
        Ok(_) => OperationResult::Success,
        Err(e) => OperationResult::Error(format!("reboot syscall failed: {}", e)),
    }
}
