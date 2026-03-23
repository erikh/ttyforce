//! CLI subcommand tests.
//!
//! These tests exercise the `ttyforce` binary's subcommands by running the
//! compiled binary as a subprocess. They verify argument parsing, output
//! format, and file I/O for each subcommand.

use std::process::Command;

fn ttyforce_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ttyforce"))
}

// === Help and argument parsing ===

#[test]
fn cli_no_args_shows_help() {
    let out = ttyforce_bin().output().unwrap();
    // clap exits 2 when a required subcommand is missing
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Usage:") || stderr.contains("COMMAND"),
        "expected usage info, got: {}",
        stderr
    );
}

#[test]
fn cli_help_flag() {
    let out = ttyforce_bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("detect"));
    assert!(stdout.contains("output"));
    assert!(stdout.contains("run"));
    assert!(stdout.contains("--input"));
    assert!(stdout.contains("--output"));
}

#[test]
fn cli_detect_help() {
    let out = ttyforce_bin().args(["detect", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--fixture"));
    assert!(stdout.contains("--input"));
    assert!(stdout.contains("--output"));
}

#[test]
fn cli_output_help() {
    let out = ttyforce_bin().args(["output", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--input"));
    assert!(stdout.contains("--output"));
}

#[test]
fn cli_run_help() {
    let out = ttyforce_bin().args(["run", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--input"));
    assert!(stdout.contains("--output"));
}

// === detect subcommand ===

#[test]
fn cli_detect_with_input_file() {
    let out = ttyforce_bin()
        .args(["detect", "-i", "fixtures/hardware/ethernet_1disk.toml"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Should output a TOML hardware manifest
    assert!(stdout.contains("[[network.interfaces]]"), "expected TOML hardware manifest, got: {}", stdout);
    assert!(stdout.contains("eth0"));
}

#[test]
fn cli_detect_with_output_file() {
    let tmp = std::env::temp_dir().join("ttyforce-cli-test-detect.toml");
    let _ = std::fs::remove_file(&tmp);

    let out = ttyforce_bin()
        .args([
            "detect",
            "-i", "fixtures/hardware/wifi_1disk.toml",
            "-o", tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(content.contains("[[network.interfaces]]"));
    assert!(content.contains("wlan0"));

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn cli_detect_missing_input_file() {
    let out = ttyforce_bin()
        .args(["detect", "-i", "nonexistent_file.toml"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Failed to load"));
}

// === detect --fixture ===

#[test]
fn cli_detect_fixture_runs_scenario() {
    let out = ttyforce_bin()
        .args(["detect", "--fixture", "fixtures/scenarios/abort_install.toml"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Should output an action manifest with the abort operation
    assert!(stdout.contains("Abort"), "expected Abort in manifest, got: {}", stdout);
    assert!(stdout.contains("final_state"));
}

#[test]
fn cli_detect_fixture_with_output_file() {
    let tmp = std::env::temp_dir().join("ttyforce-cli-test-fixture.toml");
    let _ = std::fs::remove_file(&tmp);

    let out = ttyforce_bin()
        .args([
            "detect",
            "--fixture", "fixtures/scenarios/abort_install.toml",
            "-o", tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(content.contains("Abort"));

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn cli_detect_fixture_missing_file() {
    let out = ttyforce_bin()
        .args(["detect", "--fixture", "nonexistent_scenario.toml"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Failed to read"));
}

// === initrd subcommand ===

#[test]
fn cli_help_shows_initrd_subcommand() {
    let out = ttyforce_bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("initrd"), "expected initrd subcommand in help, got: {}", stdout);
}

#[test]
fn cli_initrd_help() {
    let out = ttyforce_bin().args(["initrd", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--etc-target"), "expected --etc-target in initrd help, got: {}", stdout);
    assert!(stdout.contains("--input"), "expected --input in initrd help");
    assert!(stdout.contains("--output"), "expected --output in initrd help");
}

// === Global flag position ===

#[test]
fn cli_global_flags_before_subcommand() {
    let out = ttyforce_bin()
        .args(["-i", "fixtures/hardware/ethernet_1disk.toml", "detect"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[[network.interfaces]]"));
}

#[test]
fn cli_global_flags_after_subcommand() {
    let out = ttyforce_bin()
        .args(["detect", "-i", "fixtures/hardware/ethernet_1disk.toml"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[[network.interfaces]]"));
}
