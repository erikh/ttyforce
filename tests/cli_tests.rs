//! CLI subcommand tests.
//!
//! These tests exercise the `ttyforce` binary's subcommands by running the
//! compiled binary as a subprocess. They verify argument parsing, output
//! format, and file I/O for each subcommand.

use std::process::Command;

fn ttyforce_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ttyforce"))
}

fn run_err(msg: &str) -> String {
    format!("command execution failed: {}", msg)
}

// === Help and argument parsing ===

#[test]
fn cli_no_args_shows_help() -> Result<(), String> {
    let out = ttyforce_bin().output().map_err(|e| run_err(&e.to_string()))?;
    // clap exits 2 when a required subcommand is missing
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Usage:") || stderr.contains("COMMAND"),
        "expected usage info, got: {}",
        stderr
    );
    Ok(())
}

#[test]
fn cli_help_flag() -> Result<(), String> {
    let out = ttyforce_bin().arg("--help").output().map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("detect"));
    assert!(stdout.contains("output"));
    assert!(stdout.contains("run"));
    assert!(stdout.contains("getty"));
    assert!(stdout.contains("--input"));
    assert!(stdout.contains("--output"));
    Ok(())
}

#[test]
fn cli_detect_help() -> Result<(), String> {
    let out = ttyforce_bin().args(["detect", "--help"]).output().map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--fixture"));
    assert!(stdout.contains("--input"));
    assert!(stdout.contains("--output"));
    Ok(())
}

#[test]
fn cli_output_help() -> Result<(), String> {
    let out = ttyforce_bin().args(["output", "--help"]).output().map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--input"));
    assert!(stdout.contains("--output"));
    Ok(())
}

#[test]
fn cli_run_help() -> Result<(), String> {
    let out = ttyforce_bin().args(["run", "--help"]).output().map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--input"));
    assert!(stdout.contains("--output"));
    Ok(())
}

#[test]
fn cli_getty_help() -> Result<(), String> {
    let out = ttyforce_bin().args(["getty", "--help"]).output().map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--etc-prefix"), "expected --etc-prefix flag, got: {}", stdout);
    assert!(stdout.contains("--tty"), "expected --tty flag, got: {}", stdout);
    Ok(())
}

// === detect subcommand ===

#[test]
fn cli_detect_with_input_file() -> Result<(), String> {
    let out = ttyforce_bin()
        .args(["detect", "-i", "fixtures/hardware/ethernet_1disk.toml"])
        .output()
        .map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Should output a TOML hardware manifest
    assert!(stdout.contains("[[network.interfaces]]"), "expected TOML hardware manifest, got: {}", stdout);
    assert!(stdout.contains("eth0"));
    Ok(())
}

#[test]
fn cli_detect_with_output_file() -> Result<(), String> {
    let tmp = std::env::temp_dir().join("ttyforce-cli-test-detect.toml");
    let _cleanup = std::fs::remove_file(&tmp);

    let tmp_str = tmp.to_str().ok_or("temp path not valid UTF-8")?;
    let out = ttyforce_bin()
        .args([
            "detect",
            "-i", "fixtures/hardware/wifi_1disk.toml",
            "-o", tmp_str,
        ])
        .output()
        .map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let content = std::fs::read_to_string(&tmp).map_err(|e| format!("read output: {}", e))?;
    assert!(content.contains("[[network.interfaces]]"));
    assert!(content.contains("wlan0"));

    let _cleanup = std::fs::remove_file(&tmp);
    Ok(())
}

#[test]
fn cli_detect_missing_input_file() -> Result<(), String> {
    let out = ttyforce_bin()
        .args(["detect", "-i", "nonexistent_file.toml"])
        .output()
        .map_err(|e| run_err(&e.to_string()))?;
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Failed to load"));
    Ok(())
}

// === detect --fixture ===

#[test]
fn cli_detect_fixture_runs_scenario() -> Result<(), String> {
    let out = ttyforce_bin()
        .args(["detect", "--fixture", "fixtures/scenarios/abort_install.toml"])
        .output()
        .map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Should output an action manifest with the abort operation
    assert!(stdout.contains("Abort"), "expected Abort in manifest, got: {}", stdout);
    assert!(stdout.contains("final_state"));
    Ok(())
}

#[test]
fn cli_detect_fixture_with_output_file() -> Result<(), String> {
    let tmp = std::env::temp_dir().join("ttyforce-cli-test-fixture.toml");
    let _cleanup = std::fs::remove_file(&tmp);

    let tmp_str = tmp.to_str().ok_or("temp path not valid UTF-8")?;
    let out = ttyforce_bin()
        .args([
            "detect",
            "--fixture", "fixtures/scenarios/abort_install.toml",
            "-o", tmp_str,
        ])
        .output()
        .map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let content = std::fs::read_to_string(&tmp).map_err(|e| format!("read output: {}", e))?;
    assert!(content.contains("Abort"));

    let _cleanup = std::fs::remove_file(&tmp);
    Ok(())
}

#[test]
fn cli_detect_fixture_missing_file() -> Result<(), String> {
    let out = ttyforce_bin()
        .args(["detect", "--fixture", "nonexistent_scenario.toml"])
        .output()
        .map_err(|e| run_err(&e.to_string()))?;
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Failed to read"));
    Ok(())
}

// === initrd subcommand ===

#[test]
fn cli_help_shows_initrd_subcommand() -> Result<(), String> {
    let out = ttyforce_bin().arg("--help").output().map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("initrd"), "expected initrd subcommand in help, got: {}", stdout);
    Ok(())
}

#[test]
fn cli_initrd_help() -> Result<(), String> {
    let out = ttyforce_bin().args(["initrd", "--help"]).output().map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--etc-prefix"), "expected --etc-prefix in initrd help, got: {}", stdout);
    assert!(stdout.contains("--tty"), "expected --tty in initrd help, got: {}", stdout);
    assert!(stdout.contains("--input"), "expected --input in initrd help");
    assert!(stdout.contains("--output"), "expected --output in initrd help");
    Ok(())
}

#[test]
fn cli_initrd_tty_nonexistent_device() -> Result<(), String> {
    // --tty with a nonexistent device should fail with an error
    let out = ttyforce_bin()
        .args([
            "initrd",
            "-i", "fixtures/hardware/ethernet_1disk.toml",
            "--tty", "/dev/nonexistent_tty_device",
        ])
        .output()
        .map_err(|e| run_err(&e.to_string()))?;
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Error") || stderr.contains("error") || stderr.contains("No such file"),
        "expected error about nonexistent TTY, got: {}",
        stderr
    );
    Ok(())
}

// === Global flag position ===

#[test]
fn cli_global_flags_before_subcommand() -> Result<(), String> {
    let out = ttyforce_bin()
        .args(["-i", "fixtures/hardware/ethernet_1disk.toml", "detect"])
        .output()
        .map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[[network.interfaces]]"));
    Ok(())
}

#[test]
fn cli_global_flags_after_subcommand() -> Result<(), String> {
    let out = ttyforce_bin()
        .args(["detect", "-i", "fixtures/hardware/ethernet_1disk.toml"])
        .output()
        .map_err(|e| run_err(&e.to_string()))?;
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[[network.interfaces]]"));
    Ok(())
}
