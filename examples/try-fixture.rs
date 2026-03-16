use std::io::{self, Write};
use std::process::Command;

fn main() {
    let fixtures = [
        (
            "1",
            "Ethernet + 4 identical disks",
            "fixtures/hardware/ethernet_4disk_same.toml",
        ),
        (
            "2",
            "Ethernet + 1 disk",
            "fixtures/hardware/ethernet_1disk.toml",
        ),
        (
            "3",
            "WiFi + 1 disk",
            "fixtures/hardware/wifi_1disk.toml",
        ),
        (
            "4",
            "WiFi (crowded neighborhood) + 1 disk",
            "fixtures/hardware/wifi_crowded_1disk.toml",
        ),
        (
            "5",
            "WiFi + Ethernet + 4 disks",
            "fixtures/hardware/wifi_ethernet_4disk.toml",
        ),
        (
            "6",
            "WiFi + Ethernet + 1 disk",
            "fixtures/hardware/wifi_ethernet_1disk.toml",
        ),
        (
            "7",
            "WiFi + dead Ethernet + 1 disk",
            "fixtures/hardware/wifi_dead_ethernet_1disk.toml",
        ),
        (
            "8",
            "WiFi + dead Ethernet + 4 disks",
            "fixtures/hardware/wifi_dead_ethernet_4disk.toml",
        ),
        (
            "9",
            "Mixed drives — Workstation (NVMe + SATA SSD + HDD)",
            "fixtures/hardware/mixed_drives_workstation.toml",
        ),
        (
            "10",
            "Mixed drives — Server (NVMe boot + HDD array)",
            "fixtures/hardware/mixed_drives_server.toml",
        ),
        (
            "11",
            "Mixed drives — Homelab (all different drives)",
            "fixtures/hardware/mixed_drives_homelab.toml",
        ),
    ];

    println!();
    println!("  Town OS Installer — Hardware Profiles");
    println!("  ─────────────────────────────────────────────────");
    println!();

    for (num, desc, path) in &fixtures {
        println!("  {}. {}", num, desc);
        println!("     \x1b[90m{}\x1b[0m", path);
    }

    println!();
    println!("  q. Quit");
    println!();

    loop {
        print!("Select a profile [1-{}, q]: ", fixtures.len());
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        let input = input.trim();

        if input == "q" || input == "Q" {
            println!("Bye!");
            return;
        }

        let idx: usize = match input.parse::<usize>() {
            Ok(n) if n >= 1 && n <= fixtures.len() => n - 1,
            _ => {
                println!("Invalid selection, try again.");
                continue;
            }
        };

        let (_, desc, path) = &fixtures[idx];
        println!();
        println!("Launching installer with: {}", desc);
        println!("Hardware manifest: {}", path);
        println!();

        let status = Command::new("cargo")
            .args(["run", "--", path])
            .status();

        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("Installer exited with: {}", s);
            }
            Err(e) => {
                eprintln!("Failed to launch: {}", e);
            }
        }

        println!();
        print!("Run another profile? [y/N]: ");
        io::stdout().flush().unwrap();

        let mut again = String::new();
        io::stdin().read_line(&mut again).unwrap();
        if !again.trim().eq_ignore_ascii_case("y") {
            break;
        }
        println!();
    }
}
