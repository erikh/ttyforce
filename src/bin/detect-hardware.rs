use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();

    let output_path = if args.len() >= 3 && args[1] == "--output" {
        Some(args[2].as_str())
    } else {
        None
    };

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

    match output_path {
        Some(path) => {
            // Ensure parent directory exists
            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!("detect-hardware: failed to create {}: {}", parent.display(), e);
                        process::exit(1);
                    }
                }
            }
            if let Err(e) = std::fs::write(path, &toml_output) {
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
