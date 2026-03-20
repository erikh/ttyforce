use std::process;

use clap::Parser;

#[derive(Parser)]
#[command(name = "detect-hardware", about = "Detect hardware and print manifest")]
struct Cli {
    /// Write output to a file instead of stdout
    #[arg(short, long)]
    output: Option<String>,
}

fn main() {
    let cli = Cli::parse();

    let hardware = match ttyforce::detect::detect_hardware() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("detect-hardware: {}", e);
            process::exit(1);
        }
    };

    let toml_output = match toml::to_string_pretty(&hardware) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("detect-hardware: failed to serialize: {}", e);
            process::exit(1);
        }
    };

    match cli.output {
        Some(path) => {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() && !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!(
                            "detect-hardware: failed to create {}: {}",
                            parent.display(),
                            e
                        );
                        process::exit(1);
                    }
                }
            }
            if let Err(e) = std::fs::write(&path, &toml_output) {
                eprintln!("detect-hardware: failed to write {}: {}", path, e);
                process::exit(1);
            }
            eprintln!("Hardware manifest written to {}", path);
        }
        None => {
            println!("{}", toml_output);
        }
    }
}
