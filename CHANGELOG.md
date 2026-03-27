# Changelog

## 0.3.1 (2026-03-27)

### Features

- Display `/etc/issue` with agetty-style escape substitutions before login in getty mode
- Enable MulticastDNS (`MulticastDNS=yes`) in all generated networkd `.network` files

### Fixes

- Use relative symlink for mount service enablement (fixes stale `/new_root/` path after pivot_root)
- Remove automatic serial console logging to `/dev/ttyS0` (was interfering with getty on tty1)
- Remove all unsafe code from the crate (rewrite syscall.rs to use `ip`/`ping` commands)
- Remove all `.unwrap()` calls and `let _ = expr;` patterns per CLAUDE.md rules

### Improvements

- Add `libblockdev-crypto2` to integration test container to silence udisksd warning

## 0.2.1 (2026-03-24)

### Features

- WPS push-button support for wifi connection (`wpa_cli wps_pbc`)
- `deny(dead_code)` and `deny(unsafe_code)` enforced at the crate level

### Improvements

- Group disks for RAID by transport type and similar size (10 GB threshold), not just make/model
- Prepare wifi hardware (rfkill unblock, modprobe) in initrd before interface detection
- Sync README with current codebase (serial logging, config paths, external tools, `--tty` flag)

## 0.2.0-alpha2 (2026-03-19)

### Breaking changes

- Remove ZFS support entirely (filesystem variants, operations, tests, fixtures)
- Restructure CLI into subcommands (`detect`, `output`, `run`) replacing flags (`--detect`, `--fixture`)
- Remove filesystem selection screen — Btrfs is now auto-selected

### Features

- CLI subcommand `detect` — prints hardware manifest (replaces `--detect`)
- CLI subcommand `output` — dry-run TUI with mock executor, prints operations that would be performed
- CLI subcommand `run` — launches the real installer
- Global `-i/--input` and `-o/--output` flags for all subcommands
- Configurable mount point (default `/town-os`, was hardcoded `/mnt`)
- CLI test suite (`tests/cli_tests.rs`)

### Improvements

- Skip filesystem selection screen, advance directly from network to RAID config
- `detect-hardware` binary now uses clap for argument parsing
- Updated README and CLAUDE.md with full CLI documentation

## 0.2.0-alpha (2026-03-19)

### Features

- Initial Town OS installer TUI with network and disk provisioning
- Disk grouping by make/model with RAID configuration options (single, mirror, raidz)
- WiFi network selection, authentication, and connection management
- Ethernet auto-detection with link availability checks
- DHCP configuration and IP provisioning
- DNS resolution verification
- Hardware detection tool (`detect-hardware`)
- Reboot, power off, and exit options on completion screen
- Simulation/test mode with manifest-driven inputs and operation outputs
- Integration test suite with container-based hardware simulation

### Improvements

- Skip DHCP when ethernet interface already has an IP assigned
- Skip network probing entirely for already-connected ethernet
- Fix networkd false negatives blocking already-connected interfaces
- Add exit option to reboot screen alongside reboot and power off
- Fix TUI exit on install completion
- Real hardware test example for integration testing

## 0.1.0

Initial development (unreleased).
