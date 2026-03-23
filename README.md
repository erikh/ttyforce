# ttyforce

Text user interface for installing Town OS. Presented during disk provisioning, it handles network configuration and disk setup through an nmtui-style interface.

## Usage

```bash
# Detect hardware and print the manifest to stdout
ttyforce detect

# Detect hardware, save manifest to a file
ttyforce detect -o hardware.toml

# Print manifest from an existing hardware file (no auto-detection)
ttyforce detect -i fixtures/hardware/ethernet_4disk_same.toml

# Run a scripted fixture scenario and print the resulting operations
ttyforce detect --fixture fixtures/scenarios/full_install_ethernet_4disk.toml

# Detect real hardware, run the TUI with a mock executor,
# and print the operations that would be performed (dry run)
ttyforce output

# Same dry run, but load hardware from a file
ttyforce output -i fixtures/hardware/ethernet_1disk.toml

# Save the dry-run operations manifest to a file
ttyforce output -o operations.toml

# Detect hardware and launch the real installer
ttyforce run

# Launch in initrd mode (dhcpcd, ip, wpa_supplicant CLI — no systemd)
ttyforce run --initrd

# Launch the real installer with hardware from a file (mock executor)
ttyforce run -i fixtures/hardware/ethernet_1disk.toml
```

### Subcommands

| Subcommand | Description |
|---|---|
| `detect` | Detect hardware and print the hardware manifest. With `--fixture`, runs a scripted scenario and prints the resulting operations manifest instead. |
| `output` | Detect real hardware (or load via `-i`), run the full TUI with a mock executor so no real changes are made, then print the operations that would have been performed. |
| `run` | Detect hardware (or load via `-i`) and launch the real installer. Uses the real executor when auto-detecting, mock executor when loading from file. |

### Global flags

| Flag | Description |
|---|---|
| `-i, --input <FILE>` | Load hardware from a manifest file instead of auto-detecting. |
| `-o, --output <FILE>` | Write output to a file instead of stdout. |
| `--initrd` | Use initrd-compatible tools (dhcpcd, ip, wpa_supplicant CLI) instead of systemd. Persists network config to the installed system. Only affects `run`. |

## How it works

### Network

The installer prioritizes getting online with minimal user interaction:

1. If a wired connection with link+carrier is already up, it selects it and advances directly to disk setup — no probing or DHCP reconfiguration
2. If ethernet has link but no carrier, it brings the interface up step by step (enable, check link, DHCP, IP check, connectivity checks)
3. If ethernet is dead or absent, it falls back to wifi
4. Wifi presents a scannable network list with signal strength and security info
5. Supports WPA2/WPA3 password entry and QR code configuration

### Disks

Disks are automatically grouped by make and model. The filesystem is always Btrfs. RAID options are presented based on disk count:

- **1 disk** — single drive
- **2 disks** — RAID1 (Btrfs mirror)
- **3+ disks** — RAID5 (Btrfs striped with parity)

The installation target mount point defaults to `/town-os`.

### Final screen

After installation completes (or is aborted), the final screen offers three choices:

- **Reboot** — restart the machine into the new system
- **Exit** — return to the shell
- **Power Off** — shut down

### Executor modes

Two executor backends are available:

- **Systemd** (default) — Uses systemd dbus interfaces (networkd, resolved, logind) with sysfs/command fallbacks. Suitable for full systemd environments.
- **Initrd** (`--initrd`) — Uses syscalls and sysfs directly where possible, with minimal external tool dependencies. After a successful install, network configuration (networkd units and wpa_supplicant configs) is persisted to `<mount_point>/etc/` so the installed system boots with working networking.

  **Syscalls used (no external tools):**
  - Interface up/down — `ioctl(SIOCSIFFLAGS)`
  - IP address check — `ioctl(SIOCGIFADDR)`
  - Link/carrier check — sysfs `/sys/class/net/<iface>/carrier`
  - Route check — `/proc/net/route`
  - Internet check — ICMP echo via raw socket (`SOCK_DGRAM/IPPROTO_ICMP`)
  - DNS resolution — UDP socket to nameserver from `/etc/resolv.conf`
  - Mount/unmount — `mount(2)` / `umount2(2)`
  - Reboot — `reboot(2)` syscall

  **Required external tools in initrd:**
  - `dhcpcd` — DHCP client
  - `wpa_supplicant` — WPA authentication (CLI mode, no dbus)
  - `iw` — wifi scanning (fallback: `iwlist`)
  - `parted` — disk partitioning
  - `mkfs.btrfs` — btrfs filesystem creation
  - `btrfs` — subvolume management
  - `pacstrap` or `install.sh` — base system installation

### Hardware detection

Detection uses systemd dbus interfaces with sysfs/command fallbacks. Negative results from networkd are never trusted on their own — the installer always falls through to direct system checks (sysfs carrier, `ip addr show`, `ip route`, etc.):

- **Network interfaces** — systemd-networkd (`org.freedesktop.network1`) for link/carrier state, wpa_supplicant dbus for wifi scanning, sysfs and `ip` command as fallbacks
- **Disks** — UDisks2 (`org.freedesktop.UDisks2`) for block device enumeration and drive metadata
- **DNS** — systemd-resolved (`org.freedesktop.resolve1`) for name resolution, with `dig`/`getent` fallback

## Testing

```bash
# Unit + fixture + scenario + playbook + CLI tests (includes lint)
make test

# Integration tests in a container (requires podman, uses sudo if needed)
make test-integration
```

### Fixtures

Hardware manifests in `fixtures/hardware/` define simulated hardware configurations:

- `ethernet_4disk_same` — ethernet + 4 identical disks
- `ethernet_1disk` — ethernet + 1 disk
- `wifi_1disk` — wifi only + 1 disk
- `wifi_crowded_1disk` — crowded wifi neighborhood
- `wifi_ethernet_*` — both interfaces present
- `wifi_dead_ethernet_*` — dead ethernet, wifi available
- `mixed_drives_*` — workstation/server/homelab with mixed drive vendors

Use with `detect` or `output` via `-i`:

```bash
ttyforce detect -i fixtures/hardware/ethernet_4disk_same.toml
ttyforce output -i fixtures/hardware/ethernet_4disk_same.toml
```

### Scenarios

Scripted test cases in `fixtures/scenarios/` feed inputs and mock responses to the state machine non-interactively:

```bash
ttyforce detect --fixture fixtures/scenarios/full_install_ethernet_4disk.toml
```

### Playbooks

Playbooks in `fixtures/playbooks/` extend scenarios with assertions — expected screen transitions, operation sequences, and final state. These are verified by `cargo test --test playbook_tests`.

## License

MIT
