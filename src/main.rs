#![deny(dead_code)]
#![deny(unsafe_code)]

use std::process;

use clap::{Parser, Subcommand};

use ttyforce::engine::executor::{InitrdExecutor, RealExecutor, SimulatedResponse, TestExecutor};
use ttyforce::engine::state_machine::InstallerStateMachine;
use ttyforce::tui::GettyApp;
use ttyforce::manifest::HardwareManifest;
use ttyforce::tui::App;

#[derive(Parser)]
#[command(name = "ttyforce", about = "Town OS installer TUI")]
struct Cli {
    /// Hardware manifest input file (skip auto-detection)
    #[arg(short, long, global = true)]
    input: Option<String>,

    /// Write output to a file instead of stdout
    #[arg(short, long, global = true)]
    output: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Detect hardware and print the hardware manifest
    Detect {
        /// Run a fixture scenario file instead of detecting hardware
        #[arg(long)]
        fixture: Option<String>,
    },
    /// Detect real hardware, run the TUI with a mock executor, and print the operations that would be performed
    Output,
    /// Detect hardware and launch the real installer (systemd mode)
    Run,
    /// Run installer in initrd mode (syscalls, no systemd dbus)
    Initrd {
        /// Root prefix for /etc config file writes (default: same as mount point)
        #[arg(long)]
        etc_prefix: Option<String>,
        /// TTY device to use for the TUI (e.g. /dev/tty1, /dev/ttyS0)
        #[arg(long)]
        tty: Option<String>,
    },
    /// Run as getty replacement (system status + login screen)
    Getty {
        /// Root prefix for /etc config file writes (default: same as mount point)
        #[arg(long)]
        etc_prefix: Option<String>,
        /// TTY device to use for the TUI (e.g. /dev/tty1, /dev/ttyS0)
        #[arg(long)]
        tty: Option<String>,
        /// Listen to /dev/kmsg and repaint on kernel messages (use on console TTYs)
        #[arg(long)]
        console: bool,
        /// Enable [q] Shell action to drop into $SHELL
        #[arg(long)]
        shell: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Detect { fixture } => {
            if let Some(scenario_path) = fixture {
                run_fixture(&scenario_path, cli.output.as_deref());
            } else {
                run_detect(cli.input.as_deref(), cli.output.as_deref());
            }
        }
        Command::Output => {
            run_output(cli.input.as_deref(), cli.output.as_deref());
        }
        Command::Run => {
            run_installer(cli.input.as_deref(), cli.output.as_deref(), false, None, None);
        }
        Command::Initrd { etc_prefix, tty } => {
            run_installer(
                cli.input.as_deref(),
                cli.output.as_deref(),
                true,
                etc_prefix.as_deref(),
                tty.as_deref(),
            );
        }
        Command::Getty { etc_prefix, tty, console, shell } => {
            run_getty(etc_prefix, tty, console, shell);
        }
    }
}

fn load_hardware(input: Option<&str>, initrd: bool) -> HardwareManifest {
    match input {
        Some(path) => match HardwareManifest::load(path) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("Failed to load hardware manifest: {}", e);
                process::exit(1);
            }
        },
        None => {
            eprintln!("Detecting hardware...");
            let detect_result = if initrd {
                ttyforce::detect::detect_hardware_initrd()
            } else {
                ttyforce::detect::detect_hardware()
            };
            match detect_result {
                Ok(h) => {
                    eprintln!(
                        "Found {} network interface(s), {} disk(s)",
                        h.network.interfaces.len(),
                        h.disks.len()
                    );
                    h
                }
                Err(e) => {
                    eprintln!("Hardware detection failed: {}", e);
                    process::exit(1);
                }
            }
        }
    }
}

fn write_output(content: &str, output: Option<&str>) {
    match output {
        Some(path) => {
            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.as_os_str().is_empty() && !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!("Failed to create directory {}: {}", parent.display(), e);
                        process::exit(1);
                    }
                }
            }
            if let Err(e) = std::fs::write(path, content) {
                eprintln!("Failed to write {}: {}", path, e);
                process::exit(1);
            }
            eprintln!("Output written to {}", path);
        }
        None => {
            println!("{}", content);
        }
    }
}

fn run_detect(input: Option<&str>, output: Option<&str>) {
    let hardware = load_hardware(input, false);
    let manifest = match toml::to_string_pretty(&hardware) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to serialize hardware manifest: {}", e);
            process::exit(1);
        }
    };
    write_output(&manifest, output);
}

fn run_output(input: Option<&str>, output: Option<&str>) {
    let hardware = load_hardware(input, false);

    if hardware.disks.is_empty() {
        eprintln!("Error: no disks detected");
        process::exit(1);
    }

    let state_machine = InstallerStateMachine::new(hardware);
    let mut app = App::new(state_machine);
    let mut executor = TestExecutor::new(vec![]);

    if let Err(e) = app.run(&mut executor, None) {
        eprintln!("Error: {}", e);
        process::exit(1);
    }

    print_operations_summary(&app.state_machine);

    let manifest = match toml::to_string_pretty(&app.state_machine.action_manifest) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to serialize action manifest: {}", e);
            process::exit(1);
        }
    };
    write_output(&manifest, output);
}

fn run_installer(
    input: Option<&str>,
    output: Option<&str>,
    initrd: bool,
    etc_prefix: Option<&str>,
    tty: Option<&str>,
) {
    let hardware = load_hardware(input, initrd);

    if hardware.disks.is_empty() {
        eprintln!("Error: no disks detected");
        process::exit(1);
    }

    let mut state_machine = InstallerStateMachine::new(hardware);
    if let Some(target) = etc_prefix {
        state_machine.etc_prefix = Some(target.to_string());
    }
    let mut app = App::new(state_machine);

    if input.is_some() {
        let mut executor = TestExecutor::new(vec![]);
        if let Err(e) = app.run(&mut executor, tty) {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    } else if initrd {
        let mut executor = InitrdExecutor::new();
        if let Err(e) = app.run(&mut executor, tty) {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    } else {
        let mut executor = RealExecutor::new();
        if let Err(e) = app.run(&mut executor, tty) {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    }

    print_operations_summary(&app.state_machine);

    if let Some(out) = output {
        let manifest = match toml::to_string_pretty(&app.state_machine.action_manifest) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Failed to serialize action manifest: {}", e);
                process::exit(1);
            }
        };
        write_output(&manifest, Some(out));
    }
}

fn run_getty(etc_prefix: Option<String>, tty: Option<String>, console: bool, shell: bool) {
    let tty_clone = tty.clone();
    let mut app = GettyApp::new(etc_prefix, tty, "/town-os".to_string(), console);
    app.shell_enabled = shell;
    let mut executor = RealExecutor::new();

    if let Err(e) = app.run(&mut executor, tty_clone.as_deref()) {
        eprintln!("Getty error: {}", e);
        process::exit(1);
    }
}

fn run_fixture(scenario_path: &str, output: Option<&str>) {
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

    let manifest = match toml::to_string_pretty(&state_machine.action_manifest) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to serialize action manifest: {}", e);
            process::exit(1);
        }
    };
    write_output(&manifest, output);
}

fn print_operations_summary(state_machine: &InstallerStateMachine) {
    let manifest = &state_machine.action_manifest;

    eprintln!();
    eprintln!("═══════════════════════════════════════════════════════");
    eprintln!("  Town OS Installer — Operations Summary");
    eprintln!("═══════════════════════════════════════════════════════");
    eprintln!();

    if manifest.operations.is_empty() {
        eprintln!("  No operations were performed.");
    } else {
        for op in &manifest.operations {
            let status = match &op.result {
                ttyforce::manifest::OperationOutcome::Success => "\x1b[32m OK \x1b[0m",
                ttyforce::manifest::OperationOutcome::Error(_) => "\x1b[31mFAIL\x1b[0m",
                ttyforce::manifest::OperationOutcome::Timeout => "\x1b[33m T/O\x1b[0m",
                ttyforce::manifest::OperationOutcome::Skipped => "\x1b[90mSKIP\x1b[0m",
            };
            eprintln!("  [{:>3}] [{}] {}", op.sequence, status, op.operation);

            if let ttyforce::manifest::OperationOutcome::Error(msg) = &op.result {
                eprintln!("         \x1b[31m└─ {}\x1b[0m", msg);
            }
        }
    }

    eprintln!();
    eprintln!("───────────────────────────────────────────────────────");
    let final_state_display = match &manifest.final_state {
        ttyforce::manifest::InstallerFinalState::Installed => "\x1b[32mInstalled\x1b[0m",
        ttyforce::manifest::InstallerFinalState::Rebooted => "\x1b[32mRebooted\x1b[0m",
        ttyforce::manifest::InstallerFinalState::Aborted => "\x1b[33mAborted\x1b[0m",
        ttyforce::manifest::InstallerFinalState::Exited => "\x1b[36mExited\x1b[0m",
        ttyforce::manifest::InstallerFinalState::Error(msg) => {
            eprintln!("  Error: {}", msg);
            "\x1b[31mError\x1b[0m"
        }
    };
    eprintln!(
        "  Final state: {}  |  Operations: {}",
        final_state_display,
        manifest.operations.len()
    );
    eprintln!("═══════════════════════════════════════════════════════");
    eprintln!();
}

#[derive(serde::Deserialize)]
struct ScenarioFile {
    hardware_file: String,
    #[serde(default)]
    simulated_responses: Vec<SimulatedResponse>,
    #[serde(default)]
    inputs: Vec<ttyforce::engine::state_machine::UserInput>,
}
