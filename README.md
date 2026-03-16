# ttyforce

Text user interface for installing Town OS. Presented during disk provisioning, it handles network configuration and disk setup through an nmtui-style interface.

## Usage

```bash
# Auto-detect hardware and launch installer
ttyforce

# Launch with a hardware manifest (simulated environment)
ttyforce fixtures/hardware/ethernet_4disk_same.toml

# Detect hardware and print manifest
ttyforce --detect

# Run a scripted test scenario
ttyforce --fixture fixtures/scenarios/full_install_ethernet_4disk.toml
```

## How it works

### Network

The installer prioritizes getting online with minimal user interaction:

1. If a wired connection with link+carrier is already up, it selects it and advances directly to disk setup — no probing or DHCP reconfiguration
2. If ethernet has link but no carrier, it brings the interface up step by step (enable, check link, DHCP, IP check, connectivity checks)
3. If ethernet is dead or absent, it falls back to wifi
4. Wifi presents a scannable network list with signal strength and security info
5. Supports WPA2/WPA3 password entry and QR code configuration

### Disks

Disks are automatically grouped by make and model. RAID options are presented based on disk count:

- **1 disk** — single drive
- **2 disks** — mirror (btrfs RAID1 or ZFS mirror)
- **3+ disks** — raidz (btrfs RAID5 or ZFS raidz)

Both Btrfs and ZFS are supported as filesystem options.

### Final screen

After installation completes (or is aborted), the final screen offers three choices:

- **Reboot** — restart the machine into the new system
- **Exit** — return to the shell
- **Power Off** — shut down

### Hardware detection

Detection uses systemd dbus interfaces with sysfs/command fallbacks. Negative results from networkd are never trusted on their own — the installer always falls through to direct system checks (sysfs carrier, `ip addr show`, `ip route`, etc.):

- **Network interfaces** — systemd-networkd (`org.freedesktop.network1`) for link/carrier state, wpa_supplicant dbus for wifi scanning, sysfs and `ip` command as fallbacks
- **Disks** — UDisks2 (`org.freedesktop.UDisks2`) for block device enumeration and drive metadata
- **DNS** — systemd-resolved (`org.freedesktop.resolve1`) for name resolution, with `dig`/`getent` fallback

## Testing

```bash
# Unit + fixture + scenario + playbook tests
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

Pass any of these as argument #1 to run the TUI in a simulated environment:

```bash
ttyforce fixtures/hardware/ethernet_4disk_same.toml
```

### Scenarios

Scripted test cases in `fixtures/scenarios/` feed inputs and mock responses to the state machine non-interactively:

```bash
ttyforce --fixture fixtures/scenarios/full_install_ethernet_4disk.toml
```

### Playbooks

Playbooks in `fixtures/playbooks/` extend scenarios with assertions — expected screen transitions, operation sequences, and final state. These are verified by `cargo test --test playbook_tests`.

## License

MIT
