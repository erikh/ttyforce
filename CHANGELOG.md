# Changelog

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
