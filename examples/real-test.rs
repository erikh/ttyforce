//! Example installer program that detects real hardware from systemd,
//! then runs the TUI installer with a mock executor (no real operations
//! are ever performed).
//!
//! Usage:
//!   real-test                  # detect hardware, run TUI, record ops only
//!   real-test -o out.toml      # same, write manifest to file
//!   real-test --list-ops       # detect hardware, skip TUI, just list what
//!                              #   auto-detect would do without user input

use std::env;
use std::process;

use ttyforce::engine::executor::TestExecutor;
use ttyforce::engine::state_machine::InstallerStateMachine;
use ttyforce::manifest::HardwareManifest;
use ttyforce::tui::App;

fn main() {
    let args: Vec<String> = env::args().collect();

    let list_ops = args.iter().any(|a| a == "--list-ops");
    let output_file = args
        .iter()
        .position(|a| a == "-o" || a == "--output")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }

    // Step 1: Always detect real hardware from systemd
    eprintln!("Detecting hardware from systemd...");
    let hardware = match ttyforce::detect::detect_hardware() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Hardware detection failed: {}", e);
            process::exit(1);
        }
    };

    print_hardware_summary(&hardware);

    if list_ops {
        // Non-interactive: run auto-detect logic, print the operations that
        // would be queued, then exit. No TUI is shown.
        run_list_ops(hardware, output_file);
    } else {
        // Interactive TUI with a mock executor that records operations
        // without actually performing them.
        run_dry_run(hardware, output_file);
    }
}

fn print_usage() {
    eprintln!("real-test — Town OS installer with systemd hardware detection (safe, no real ops)");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  real-test                      Run installer (record only, no side effects)");
    eprintln!("  real-test -o FILE              Run installer, write manifest to FILE");
    eprintln!("  real-test --list-ops            Non-interactive: list auto-detect operations");
    eprintln!("  real-test --help                Show this help");
    eprintln!();
    eprintln!("This example always uses a mock executor. No disks, networks, or system");
    eprintln!("state will be modified.");
}

fn print_hardware_summary(hardware: &HardwareManifest) {
    let ifaces = &hardware.network.interfaces;
    let eth_count = ifaces.iter().filter(|i| i.kind == ttyforce::manifest::InterfaceKind::Ethernet).count();
    let wifi_count = ifaces.iter().filter(|i| i.kind == ttyforce::manifest::InterfaceKind::Wifi).count();

    eprintln!();
    eprintln!("  Interfaces: {} ethernet, {} wifi", eth_count, wifi_count);
    for iface in ifaces {
        let link = if iface.has_link { "link" } else { "no-link" };
        let carrier = if iface.has_carrier { "carrier" } else { "no-carrier" };
        eprintln!("    {} ({:?}) — {}, {}", iface.name, iface.kind, link, carrier);
    }

    eprintln!("  Disks: {}", hardware.disks.len());
    for disk in &hardware.disks {
        let size_gb = disk.size_bytes / 1_000_000_000;
        eprintln!("    {} — {} {} ({} GB)", disk.device, disk.make, disk.model, size_gb);
    }

    if let Some(wifi_env) = &hardware.network.wifi_environment {
        if !wifi_env.available_networks.is_empty() {
            eprintln!("  WiFi networks: {}", wifi_env.available_networks.len());
            for net in &wifi_env.available_networks {
                eprintln!("    {} ({}dBm, {:?})", net.ssid, net.signal_strength, net.security);
            }
        }
    }
    eprintln!();
}

/// Run the TUI interactively with a TestExecutor (mock).
/// All operations are recorded but nothing is actually executed.
fn run_dry_run(hardware: HardwareManifest, output_file: Option<&str>) {
    eprintln!("Starting TUI — operations will be recorded, not executed.");
    eprintln!();

    let state_machine = InstallerStateMachine::new(hardware);
    let mut app = App::new(state_machine);
    let mut executor = TestExecutor::new(vec![]);

    if let Err(e) = app.run(&mut executor, None) {
        eprintln!("Error: {}", e);
        process::exit(1);
    }

    output_manifest(&app.state_machine, output_file);
}

/// Run auto-detect logic non-interactively and list the operations that
/// would be performed, without launching the TUI.
fn run_list_ops(hardware: HardwareManifest, output_file: Option<&str>) {
    use ttyforce::engine::state_machine::UserInput;

    eprintln!("[list-ops] Running auto-detect logic (non-interactive)...");
    eprintln!();

    let mut state_machine = InstallerStateMachine::new(hardware);
    let mut executor = TestExecutor::new(vec![]);

    // Confirm on NetworkConfig triggers auto-detect, which probes
    // interfaces and advances to the next screen.
    state_machine.process_input(UserInput::Confirm, &mut executor);

    output_manifest(&state_machine, output_file);
}

fn output_manifest(state_machine: &InstallerStateMachine, output_file: Option<&str>) {
    let manifest = &state_machine.action_manifest;

    match output_file {
        Some(path) => {
            let toml_output = toml::to_string_pretty(manifest).unwrap();
            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!("Failed to create {}: {}", parent.display(), e);
                        process::exit(1);
                    }
                }
            }
            if let Err(e) = std::fs::write(path, &toml_output) {
                eprintln!("Failed to write {}: {}", path, e);
                process::exit(1);
            }
            eprintln!("Manifest written to {}", path);
            // Also print operations to stdout
            print_operations_plain(state_machine);
        }
        None => {
            print_operations_plain(state_machine);
        }
    }
}

fn print_operations_plain(state_machine: &InstallerStateMachine) {
    let manifest = &state_machine.action_manifest;

    for op in &manifest.operations {
        let status = match &op.result {
            ttyforce::manifest::OperationOutcome::Success => "OK",
            ttyforce::manifest::OperationOutcome::Error(_) => "FAIL",
            ttyforce::manifest::OperationOutcome::Timeout => "TIMEOUT",
            ttyforce::manifest::OperationOutcome::Skipped => "SKIP",
        };
        println!("{} {}", status, op.operation);

        if let ttyforce::manifest::OperationOutcome::Error(msg) = &op.result {
            println!("  error: {}", msg);
        }
    }

    let final_state = match &manifest.final_state {
        ttyforce::manifest::InstallerFinalState::Installed => "Installed".to_string(),
        ttyforce::manifest::InstallerFinalState::Rebooted => "Rebooted".to_string(),
        ttyforce::manifest::InstallerFinalState::Aborted => "Aborted".to_string(),
        ttyforce::manifest::InstallerFinalState::Exited => "Exited".to_string(),
        ttyforce::manifest::InstallerFinalState::Error(msg) => format!("Error: {}", msg),
    };
    println!("{}", final_state);
}
