# Changelog

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
