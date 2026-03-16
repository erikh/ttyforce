use std::env;
use std::process;

use ttyforce::engine::executor::{RealExecutor, SimulatedResponse, TestExecutor};
use ttyforce::engine::state_machine::InstallerStateMachine;
use ttyforce::manifest::HardwareManifest;
use ttyforce::tui::App;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        // No arguments: auto-detect hardware
        run_auto_detect();
        return;
    }

    match args[1].as_str() {
        "--fixture" => {
            if args.len() < 3 {
                eprintln!("Usage: ttyforce --fixture <scenario.toml>");
                process::exit(1);
            }
            run_fixture(&args[2]);
        }
        "--detect" => {
            // Just detect and print the hardware manifest, then exit
            run_detect_only();
        }
        "--help" | "-h" => {
            print_usage();
        }
        path => {
            run_interactive(path);
        }
    }
}

fn print_usage() {
    eprintln!("Usage: ttyforce [OPTIONS] [hardware-manifest.toml]");
    eprintln!();
    eprintln!("  (no args)              Auto-detect hardware and launch installer");
    eprintln!("  <manifest.toml>        Launch installer with hardware manifest");
    eprintln!("  --detect               Detect hardware and print manifest to stdout");
    eprintln!("  --fixture <file.toml>  Run a test scenario");
    eprintln!("  --help                 Show this help");
}

fn run_auto_detect() {
    eprintln!("Detecting hardware...");
    let hardware = match ttyforce::detect::detect_hardware() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Hardware detection failed: {}", e);
            process::exit(1);
        }
    };

    eprintln!(
        "Found {} network interface(s), {} disk(s)",
        hardware.network.interfaces.len(),
        hardware.disks.len()
    );

    if hardware.disks.is_empty() {
        eprintln!("Error: no disks detected");
        process::exit(1);
    }

    run_with_hardware(hardware);
}

fn run_detect_only() {
    let hardware = match ttyforce::detect::detect_hardware() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Hardware detection failed: {}", e);
            process::exit(1);
        }
    };

    let output = toml::to_string_pretty(&hardware).unwrap();
    println!("{}", output);
}

fn run_interactive(hardware_path: &str) {
    let hardware = match HardwareManifest::load(hardware_path) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to load hardware manifest: {}", e);
            process::exit(1);
        }
    };

    // Hardware manifest = simulated environment, use mock executor
    let state_machine = InstallerStateMachine::new(hardware);
    let mut app = App::new(state_machine);
    let mut executor = TestExecutor::new(vec![]);

    if let Err(e) = app.run(&mut executor) {
        eprintln!("Error: {}", e);
        process::exit(1);
    }

    print_operations_summary(&app.state_machine);
}

fn run_with_hardware(hardware: HardwareManifest) {
    // Auto-detected hardware = real system, use real executor
    let state_machine = InstallerStateMachine::new(hardware);
    let mut app = App::new(state_machine);
    let mut executor = RealExecutor::new();

    if let Err(e) = app.run(&mut executor) {
        eprintln!("Error: {}", e);
        process::exit(1);
    }

    print_operations_summary(&app.state_machine);
}

fn run_fixture(scenario_path: &str) {
    let content = match std::fs::read_to_string(scenario_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to read scenario: {}", e);
            process::exit(1);
        }
    };

    let scenario: ScenarioFile = match toml::from_str(&content) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to parse scenario: {}", e);
            process::exit(1);
        }
    };

    let hardware = match HardwareManifest::load(&scenario.hardware_file) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to load hardware manifest: {}", e);
            process::exit(1);
        }
    };

    let mut state_machine = InstallerStateMachine::new(hardware);
    let mut executor = TestExecutor::new(scenario.simulated_responses);

    for input in scenario.inputs {
        state_machine.process_input(input, &mut executor);
    }

    let output = toml::to_string_pretty(&state_machine.action_manifest).unwrap();
    println!("{}", output);
}

fn print_operations_summary(state_machine: &InstallerStateMachine) {
    let manifest = &state_machine.action_manifest;

    println!();
    println!("═══════════════════════════════════════════════════════");
    println!("  Town OS Installer — Operations Summary");
    println!("═══════════════════════════════════════════════════════");
    println!();

    if manifest.operations.is_empty() {
        println!("  No operations were performed.");
    } else {
        for op in &manifest.operations {
            let status = match &op.result {
                ttyforce::manifest::OperationOutcome::Success => "\x1b[32m OK \x1b[0m",
                ttyforce::manifest::OperationOutcome::Error(_) => "\x1b[31mFAIL\x1b[0m",
                ttyforce::manifest::OperationOutcome::Timeout => "\x1b[33m T/O\x1b[0m",
                ttyforce::manifest::OperationOutcome::Skipped => "\x1b[90mSKIP\x1b[0m",
            };
            println!("  [{:>3}] [{}] {}", op.sequence, status, op.operation);

            if let ttyforce::manifest::OperationOutcome::Error(msg) = &op.result {
                println!("         \x1b[31m└─ {}\x1b[0m", msg);
            }
        }
    }

    println!();
    println!("───────────────────────────────────────────────────────");
    let final_state_display = match &manifest.final_state {
        ttyforce::manifest::InstallerFinalState::Installed => "\x1b[32mInstalled\x1b[0m",
        ttyforce::manifest::InstallerFinalState::Rebooted => "\x1b[32mRebooted\x1b[0m",
        ttyforce::manifest::InstallerFinalState::Aborted => "\x1b[33mAborted\x1b[0m",
        ttyforce::manifest::InstallerFinalState::Error(msg) => {
            eprintln!("  Error: {}", msg);
            "\x1b[31mError\x1b[0m"
        }
    };
    println!(
        "  Final state: {}  |  Operations: {}",
        final_state_display,
        manifest.operations.len()
    );
    println!("═══════════════════════════════════════════════════════");
    println!();
}

#[derive(serde::Deserialize)]
struct ScenarioFile {
    hardware_file: String,
    #[serde(default)]
    simulated_responses: Vec<SimulatedResponse>,
    #[serde(default)]
    inputs: Vec<ttyforce::engine::state_machine::UserInput>,
}
