//! Playbook-based tests: drive the installer state machine with a sequence of inputs
//! and verify the resulting operations match an expected list.
//!
//! Each playbook is a TOML file in `fixtures/playbooks/` containing:
//!   - hardware_file: path to hardware manifest
//!   - simulated_responses: mock executor responses
//!   - inputs: sequence of UserInput actions
//!   - expected_operation_types: ordered list of operation type names
//!   - expected_final_state: "Installed", "Rebooted", or "Aborted"

use std::fs;
use std::path::Path;

use ttyforce::engine::executor::{operation_type_name, SimulatedResponse, TestExecutor};
use ttyforce::engine::state_machine::{InstallerStateMachine, UserInput};
use ttyforce::engine::OperationExecutor;
use ttyforce::manifest::{HardwareManifest, InstallerFinalState};

#[derive(serde::Deserialize)]
struct Playbook {
    description: String,
    hardware_file: String,
    expected_final_state: String,
    #[serde(default)]
    simulated_responses: Vec<SimulatedResponse>,
    #[serde(default)]
    inputs: Vec<UserInput>,
    #[serde(default)]
    expected_operation_types: Vec<String>,
    /// Expected screen after each input, in order.
    /// Values: "NetworkConfig", "NetworkProgress", "WifiSelect", "WifiPassword",
    ///         "RaidConfig", "DiskGroupSelect", "Confirm",
    ///         "InstallProgress", "Reboot"
    #[serde(default)]
    expected_screens: Vec<String>,
}

fn load_playbook(name: &str) -> Playbook {
    let path = format!("fixtures/playbooks/{}.toml", name);
    let content =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path, e));
    toml::from_str(&content).unwrap_or_else(|e| panic!("failed to parse {}: {}", path, e))
}

fn run_playbook(name: &str) {
    let playbook = load_playbook(name);

    let hardware = HardwareManifest::load(&playbook.hardware_file)
        .unwrap_or_else(|e| panic!("failed to load hardware {}: {}", playbook.hardware_file, e));

    let mut sm = InstallerStateMachine::new(hardware);
    let mut executor = TestExecutor::new(playbook.simulated_responses);

    for (i, input) in playbook.inputs.iter().enumerate() {
        let before = format!("{:?}", sm.current_screen);
        sm.process_input(input.clone(), &mut executor);
        let after = format!("{:?}", sm.current_screen);

        // Verify screen transition if expected_screens is provided
        if i < playbook.expected_screens.len() {
            assert_eq!(
                after, playbook.expected_screens[i],
                "[{}] step #{}: input {:?} (from screen {})\n  expected screen: {}\n  actual screen:   {}\n  network: {}\n  selected_interface: {:?}\n  error: {:?}",
                playbook.description, i, input, before,
                playbook.expected_screens[i], after,
                sm.network_state,
                sm.selected_interface,
                sm.error_message,
            );
        }
    }

    // If expected_screens was provided, verify count matches
    if !playbook.expected_screens.is_empty() {
        assert_eq!(
            playbook.inputs.len(),
            playbook.expected_screens.len(),
            "[{}] expected_screens length ({}) must match inputs length ({})",
            playbook.description,
            playbook.expected_screens.len(),
            playbook.inputs.len(),
        );
    }

    // Verify final state
    let expected_state = match playbook.expected_final_state.as_str() {
        "Installed" => InstallerFinalState::Installed,
        "Rebooted" => InstallerFinalState::Rebooted,
        "Aborted" => InstallerFinalState::Aborted,
        "Exited" => InstallerFinalState::Exited,
        other => panic!(
            "[{}] unknown expected_final_state: {}",
            playbook.description, other
        ),
    };
    assert_eq!(
        sm.action_manifest.final_state, expected_state,
        "[{}] final state mismatch",
        playbook.description
    );

    // Verify operations match expected list
    if !playbook.expected_operation_types.is_empty() {
        let actual_types: Vec<&str> = sm
            .action_manifest
            .operations
            .iter()
            .map(|op| operation_type_name(&op.operation))
            .collect();

        assert_eq!(
            actual_types.len(),
            playbook.expected_operation_types.len(),
            "[{}] operation count mismatch.\nExpected ({}):\n  {}\nActual ({}):\n  {}",
            playbook.description,
            playbook.expected_operation_types.len(),
            playbook.expected_operation_types.join("\n  "),
            actual_types.len(),
            actual_types.join("\n  "),
        );

        for (i, (actual, expected)) in actual_types
            .iter()
            .zip(playbook.expected_operation_types.iter())
            .enumerate()
        {
            assert_eq!(
                actual,
                &expected.as_str(),
                "[{}] operation #{} mismatch.\nExpected: {}\nActual:   {}\n\nFull expected:\n  {}\nFull actual:\n  {}",
                playbook.description,
                i,
                expected,
                actual,
                playbook.expected_operation_types.join("\n  "),
                actual_types.join("\n  "),
            );
        }
    }

    // Cross-check: executor's recorded operations should be consistent
    let exec_ops = executor.recorded_operations();
    let exec_types: Vec<&str> = exec_ops
        .iter()
        .map(|r| operation_type_name(&r.operation))
        .collect();

    assert!(
        !exec_ops.is_empty() || playbook.inputs.is_empty(),
        "[{}] executor recorded no operations",
        playbook.description
    );

    eprintln!(
        "[{}] PASS: {} manifest ops, {} executor ops, final_state={:?}",
        playbook.description,
        sm.action_manifest.operations.len(),
        exec_types.len(),
        sm.action_manifest.final_state
    );
}

// === Individual playbook tests ===

#[test]
fn playbook_ethernet_1disk_full_install() {
    run_playbook("ethernet_1disk_full_install");
}

#[test]
fn playbook_ethernet_4disk_btrfs_raid5() {
    run_playbook("ethernet_4disk_btrfs_raid5");
}

#[test]
fn playbook_wifi_1disk_full_install() {
    run_playbook("wifi_1disk_full_install");
}

#[test]
fn playbook_wifi_wrong_password_retry() {
    run_playbook("wifi_wrong_password_retry");
}

#[test]
fn playbook_wifi_signal_timeout() {
    run_playbook("wifi_signal_timeout");
}

#[test]
fn playbook_wifi_ethernet_prefers_ethernet() {
    run_playbook("wifi_ethernet_prefers_ethernet");
}

#[test]
fn playbook_dead_ethernet_falls_to_wifi() {
    run_playbook("dead_ethernet_falls_to_wifi");
}

#[test]
fn playbook_abort_install() {
    run_playbook("abort_install");
}

#[test]
fn playbook_wifi_scan_refresh() {
    run_playbook("wifi_scan_refresh");
}

#[test]
fn playbook_mixed_drives_workstation_raid5() {
    run_playbook("mixed_drives_workstation_raid5");
}

#[test]
fn playbook_reboot_after_install() {
    run_playbook("reboot_after_install");
}

// === Discovery test: automatically find and run all playbooks ===

#[test]
fn all_playbooks_parse_and_run() {
    let playbook_dir = Path::new("fixtures/playbooks");
    if !playbook_dir.exists() {
        panic!("fixtures/playbooks directory not found");
    }

    let mut count = 0;
    for entry in fs::read_dir(playbook_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().map(|e| e == "toml").unwrap_or(false) {
            let name = path.file_stem().unwrap().to_str().unwrap();
            eprintln!("Running playbook: {}", name);
            run_playbook(name);
            count += 1;
        }
    }

    assert!(count > 0, "no playbooks found in fixtures/playbooks/");
    eprintln!("All {} playbooks passed", count);
}
