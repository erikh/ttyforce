//! SSH key import from GitHub.

use std::io::Write;

use crate::engine::feedback::OperationResult;
use crate::engine::real_ops::cmd_log_append;

/// Validate a GitHub username (alphanumeric and hyphens, 1-39 chars).
pub fn is_valid_github_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= 39
        && username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !username.starts_with('-')
        && !username.ends_with('-')
}

/// Fetch SSH public keys from GitHub for a given username using curl.
pub fn fetch_github_keys(username: &str) -> Result<String, String> {
    let url = format!("https://github.com/{}.keys", username);
    let output = std::process::Command::new("curl")
        .args(["-sfL", "--max-time", "10", &url])
        .output()
        .map_err(|e| format!("curl: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "GitHub returned an error (user may not exist): exit {}",
            output.status
        ));
    }

    let keys = String::from_utf8_lossy(&output.stdout).to_string();
    let trimmed = keys.trim().to_string();
    if trimmed.is_empty() {
        return Ok(String::new());
    }

    // Sanity check: every non-empty line should look like an SSH key
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with("ssh-")
            && !line.starts_with("ecdsa-")
            && !line.starts_with("sk-")
        {
            return Err(format!(
                "unexpected response (not SSH keys): {}",
                &line[..line.len().min(60)]
            ));
        }
    }

    Ok(trimmed)
}

/// Install SSH keys to `<mount_point>/ssh/authorized_keys/<system_user>`.
/// Appends to any existing keys and deduplicates.
pub fn install_ssh_keys(mount_point: &str, system_user: &str, keys: &str) -> Result<String, String> {
    let dir = format!("{}/ssh/authorized_keys", mount_point);
    write_authorized_keys(&dir, system_user, keys)?;
    Ok(format!("{}/{}", dir, system_user))
}

/// Write keys to a file named after the user in the given directory.
fn write_authorized_keys(dir: &str, filename: &str, keys: &str) -> Result<(), String> {
    let file_path = format!("{}/{}", dir, filename);

    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {}", dir, e))?;
    set_permissions(dir, 0o755)?;

    // Read existing keys to avoid duplicates
    let existing = std::fs::read_to_string(&file_path).unwrap_or_default();
    let mut new_keys = Vec::new();
    for line in keys.lines() {
        let line = line.trim();
        if !line.is_empty() && !existing.contains(line) {
            new_keys.push(line);
        }
    }

    if new_keys.is_empty() {
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .map_err(|e| format!("open {}: {}", file_path, e))?;

    for key in &new_keys {
        writeln!(file, "{}", key).map_err(|e| format!("write: {}", e))?;
    }

    set_permissions(&file_path, 0o644)?;
    Ok(())
}

/// Set Unix file permissions.
fn set_permissions(path: &str, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, perms).map_err(|e| format!("chmod {}: {}", path, e))
}

/// Execute the ImportSshKeys operation: fetch keys from GitHub and install them.
pub fn execute_import_ssh_keys(
    mount_point: &str,
    system_user: &str,
    github_username: &str,
) -> OperationResult {
    cmd_log_append(format!(
        "$ import SSH keys from github.com/{} for {} -> {}",
        github_username, system_user, mount_point
    ));

    if !is_valid_github_username(github_username) {
        let msg = format!("invalid GitHub username: {}", github_username);
        cmd_log_append(format!("  -> FAILED: {}", msg));
        return OperationResult::Error(msg);
    }

    let keys = match fetch_github_keys(github_username) {
        Ok(k) => k,
        Err(e) => {
            cmd_log_append(format!("  -> FAILED: {}", e));
            return OperationResult::Error(e);
        }
    };

    if keys.is_empty() {
        let msg = format!("no SSH keys found for {}", github_username);
        cmd_log_append(format!("  -> {}", msg));
        return OperationResult::Error(msg);
    }

    let key_count = keys.lines().filter(|l| !l.trim().is_empty()).count();
    cmd_log_append(format!("  found {} key(s)", key_count));

    match install_ssh_keys(mount_point, system_user, &keys) {
        Ok(path) => {
            cmd_log_append(format!("  -> ok: written to {}", path));
            OperationResult::Success
        }
        Err(e) => {
            cmd_log_append(format!("  -> FAILED: {}", e));
            OperationResult::Error(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_github_username() {
        assert!(is_valid_github_username("octocat"));
        assert!(is_valid_github_username("my-user"));
        assert!(is_valid_github_username("a"));
        assert!(is_valid_github_username("user123"));
    }

    #[test]
    fn test_is_valid_github_username_invalid() {
        assert!(!is_valid_github_username(""));
        assert!(!is_valid_github_username("-leading"));
        assert!(!is_valid_github_username("trailing-"));
        assert!(!is_valid_github_username("has spaces"));
        assert!(!is_valid_github_username("has@special"));
        assert!(!is_valid_github_username("has.dot"));
        assert!(!is_valid_github_username(
            "this-username-is-way-too-long-for-github-to-accept-it"
        ));
    }

    #[test]
    fn test_fetch_github_keys_nonexistent_user() {
        let has_curl = std::process::Command::new("curl")
            .arg("--version")
            .output()
            .is_ok();
        if !has_curl {
            return;
        }
        let result = fetch_github_keys("this-user-definitely-does-not-exist-on-github-xyz-999");
        assert!(result.is_err() || result.as_ref().is_ok_and(|k| k.is_empty()));
    }

    #[test]
    fn test_write_authorized_keys_creates_dir_and_file() -> Result<(), String> {
        let tmp = std::env::temp_dir().join("ttyforce-ssh-test");
        let _cleanup = std::fs::remove_dir_all(&tmp);

        let dir = tmp.join("authorized_keys_dir");
        let dir_str = dir.to_str().ok_or("invalid path")?;
        let keys = "ssh-ed25519 AAAAC3test1 user@host\nssh-rsa AAAAB3test2 user@host\n";
        write_authorized_keys(dir_str, "root", keys)?;

        let content = std::fs::read_to_string(dir.join("root"))
            .map_err(|e| e.to_string())?;
        assert!(content.contains("ssh-ed25519 AAAAC3test1"));
        assert!(content.contains("ssh-rsa AAAAB3test2"));

        let _cleanup = std::fs::remove_dir_all(&tmp);
        Ok(())
    }

    #[test]
    fn test_write_authorized_keys_deduplicates() -> Result<(), String> {
        let tmp = std::env::temp_dir().join("ttyforce-ssh-dedup-test");
        let _cleanup = std::fs::remove_dir_all(&tmp);

        let dir = tmp.join("authorized_keys_dir");
        let dir_str = dir.to_str().ok_or("invalid path")?;
        let keys = "ssh-ed25519 AAAAC3test1 user@host\n";
        write_authorized_keys(dir_str, "root", keys)?;
        write_authorized_keys(dir_str, "root", keys)?;

        let content = std::fs::read_to_string(dir.join("root"))
            .map_err(|e| e.to_string())?;
        let count = content.matches("ssh-ed25519 AAAAC3test1").count();
        assert_eq!(count, 1, "key should appear exactly once");

        let _cleanup = std::fs::remove_dir_all(&tmp);
        Ok(())
    }

    #[test]
    fn test_execute_import_invalid_username() {
        let result = execute_import_ssh_keys("/tmp/nonexistent", "root", "-bad-name-");
        assert!(result.is_error());
    }
}
