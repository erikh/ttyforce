# Changelog

## 0.5.0 (2026-07-02)

### Features

- routability detection now survives networks that filter ICMP echo and
  outbound plaintext :53 while allowing :443, and that advertise a black-holed
  IPv6 (e.g. the "berkeley" network). `check_internet_routability` gains a
  TCP :443 probe tier (`tcp_connect_probe` in `syscall.rs`) tried before ICMP,
  and consults the IPv6 gate lazily — only after every IPv4 tier fails — so a
  failed v6 ping is no longer authoritative and a reachable :443/DoH path
  reports online. `check_internet_routability_inner` is made `pub` for
  integration testing; the existing tier tests are updated and new unit tests
  (TCP:443 v4 skips ICMP/v6, TCP:443 v6 when v4 filtered, all-filtered →
  NoInternet) plus hermetic integration tests (real loopback TCP listener, and
  the full local-resolver-dead → TCP:443 fallback compose) are added

## 0.4.10 (2026-06-30)

### Fixes

- initrd now verifies and repairs `/etc/resolv.conf` after DHCP so the GitHub
  SSH-key fetch works on networks that block public DNS over UDP/53 (libvirt NAT,
  captive/guest WiFi). On such networks the only working resolver is the
  DHCP/gateway one, but when the lease's DNS could not be read back,
  `write_resolv_conf_from_lease` retained the seeded public servers
  (`8.8.8.8`/`1.1.1.1`) — which are blocked there — so `curl https://github.com/
  <user>.keys` resolved nothing. After writing resolv.conf from the lease,
  `configure_dhcp` now calls `ensure_working_resolver()`: it tests whether the
  current nameservers actually resolve `example.com`, and if not, rewrites
  resolv.conf from the local resolver candidates (DHCP DNS + default gateway)
  with the public servers trailing, then re-tests. This guarantees the
  gateway/DHCP resolver is present and the blocked public seed is never retained
  on its own. The decision logic is a pure, mock-injectable
  `select_working_resolvers()` with unit tests. Scope is the initrd path; the
  booted system is handled separately by `town-os-resolved-bootstrap`

## 0.4.9 (2026-06-28)

### Features

- getty gains a `--sledgehammer-tryboot` flag for triggering the sledgehammer
  wipe boot on hardware without GRUB (e.g. Raspberry Pi). When set, the wipe is
  triggered with `reboot "0 tryboot"`, which one-shot-boots the Pi firmware's
  `[tryboot]` config section (Town OS points it at a cmdline that adds
  `town.sledgehammer`) and auto-reverts on the next boot — the wipe itself is
  still driven by `town.sledgehammer` on the kernel cmdline, identical to the
  GRUB path. The flag takes precedence over `--sledgehammer-grub-entry` and is
  skipped in mock mode so it never reboots a test host

### Fixes

- detect now demotes removable media to last-resort install targets. USB-attached
  drives and kernel-removable disks (SD cards and other hot-plug media) were
  offered on equal footing with internal NVMe/SATA: the manual TUI listed them
  sorted only by device name, and Easy mode auto-picked the largest disk group
  regardless of transport, so a big USB stick or SD card could be selected over a
  fixed disk. A new `prefer_fixed_disks()` step at the `detect_disks()` chokepoint
  drops any disk that is USB-transport or kernel-removable whenever at least one
  fixed disk is present, so both detection backends and both the manual and
  Easy-mode flows only ever see removable media when nothing fixed exists (a
  USB/SD-only machine can still be installed). Soldered eMMC reports
  non-removable and stays a normal fixed candidate; `mmc` transport alone is not
  used to demote since it covers both eMMC and SD cards. A `removable` flag is
  added to `DiskSpec`, populated from `/sys/block/<dev>/removable` in both the
  sysfs and UDisks2 paths, defaulting to false on uncertainty so a disk is never
  demoted when its removability cannot be read. The disk the system booted from
  is still excluded upstream via `read_boot_disks`. Added hermetic unit tests

## 0.4.8 (2026-06-26)

### Fixes

- initrd internet-routability check now prefers the local resolver over a ping
  to the public free servers. Step 5 of the connectivity flow used to ICMP-ping
  `1.1.1.1`/`8.8.8.8`/`8.8.4.4` first, which on networks that filter outbound
  traffic to public resolvers (libvirt NAT, captive/guest WiFi) simply fails and
  burns the connectivity retries even though the local resolver answers fine.
  `check_internet_routability` now resolves a probe host (`example.com`) through
  the local resolver(s) — the DHCP-offered nameservers plus the gateway's
  forwarder — first; a successful answer proves end-to-end internet without ever
  touching `1.1.1.1`. Only when no local resolver answers does it fall back to
  the public free-server ICMP ping and the gated IPv6 probe. Added hermetic
  integration tests against a loopback mock resolver

## 0.4.7 (2026-06-26)

### Fixes

- initrd DNS check now folds the default gateway into the resolver candidate
  list (DHCP-offered → gateway → `/etc/resolv.conf` → public fallback). In
  NAT/home-router setups the gateway runs a DNS forwarder and, on networks that
  filter outbound DNS to public resolvers, is frequently the only resolver that
  answers — so the check works even when the DHCP lease's DNS cannot be read
  back from dhcpcd (e.g. the libvirt dev VM, where the gateway is dnsmasq
  forwarding to the host's resolvers)

## 0.4.6 (2026-06-26)

### Fixes

- initrd IPv6 routability probe is now gated on a global-*unicast* address
  (`2000::/3`) rather than any global-scope address. `get_interface_ipv6`
  accepts ULA (`fc00::/7`), which is global in scope but not internet-routable
  (e.g. a SLAAC ULA from libvirt's NAT bridge), so the v0.4.5 gate still let the
  IPv6 ping run on a ULA-only stack and pinged a public anycast the ULA can
  never reach. New `interface_has_global_unicast_ipv6` /
  `parse_has_global_unicast_ipv6` helpers back the gate; `get_interface_ipv6` is
  unchanged since a ULA is a legitimate address to report for the guest

## 0.4.5 (2026-06-26)

### Fixes

- initrd internet check no longer probes IPv6 on IPv4-only stacks.
  `check_internet_routability` ignored its interface argument and always fell
  through to an IPv6 ping (`2606:4700:4700::1111`) after the IPv4 public-resolver
  pings failed — even with no IPv6 in the stack. On networks that filter outbound
  ICMP to public resolvers this surfaced a spurious `ping -6` and an "IPv6
  unreachable" result. The IPv6 fallback is now gated on the interface carrying a
  global IPv6 address (`get_interface_ipv6`); with no IPv6 present the probe is
  skipped entirely. Logic moved into a `check_internet_routability_inner` helper
  with injected probes and covered by unit tests

## 0.4.4 (2026-06-25)

### Fixes

- The initrd internet check no longer hardcodes `1.1.1.1`, which failed on
  networks that filter outbound DNS to public resolvers (libvirt NAT, guest/
  captive WiFi) while the DHCP-offered resolver answers fine. Hostnames now
  resolve against an ordered candidate list — DHCP-offered resolvers first,
  then `/etc/resolv.conf`, then the public fallback — trying each until one
  returns an A record (`resolve_via`/`order_nameservers`)
- ttyforce now creates the dhcpcd run/lease directories (`prepare_dhcpcd_dirs`)
  before launching dhcpcd so the lease — and its offered DNS — persists and is
  readable, rather than relying on initrd or dhcpcd hook scripts

### Improvements

- Centralize the public resolvers in `crate::network::PUBLIC_FALLBACK_DNS`
  (`8.8.8.8 8.8.4.4 1.1.1.1`) and use it everywhere those addresses appeared:
  the candidate list, the resolv.conf fallback, the routability pings (initrd
  + real_ops), and the getty online check
- Add integration tests: `resolve_via` fallback against a mock UDP DNS server,
  and dhcpcd directory population

## 0.4.3 (2026-06-25)

### Fixes

- initrd network checks are now dual-stack: bring up one interface, run DHCP
  (dhcpcd covers v4 + v6), and treat the interface as online if either family
  reaches connectivity. `check_ip_address` reports an IPv4 address, else a
  global IPv6 address; `check_upstream_router` and the DHCP route wait scan
  both `/proc/net/route` and `/proc/net/ipv6_route`; `check_internet_routability`
  pings `1.1.1.1`, falling back to `2606:4700:4700::1111`; DNS accepts IPv6
  (and zoned link-local) nameservers, binding a socket of the matching family
- Persisted network config now sets `IPv6AcceptRA=yes` and `[DHCPv6] UseDNS=no`
  so the installed system comes up dual-stack
- Fix a latent endianness bug in the IPv4 gateway decode
  (`u32::from_be(gw.swap_bytes())` was a double-swap on little-endian hosts,
  yielding a byte-reversed gateway) — now a single `swap_bytes()`

### Improvements

- Add `make help`, a self-documenting target that lists all targets with
  descriptions generated from `## ` annotations so it stays in sync; bare
  `make` still builds
- Add InitrdExecutor integration tests covering IPv4 preference, IPv6
  detection, IPv6-only fallback, no-route, and the real UDP DNS path, plus
  unit tests for the v4/v6 route parsers, the IPv6 address parser, and
  nameserver socket parsing; fix the IPv6 integration tests to declare
  addresses in the networkd `.network` unit so `networkctl reconfigure`
  does not flush them

## 0.4.2 (2026-06-13)

### Fixes

- initrd DNS resolution check now uses the nameserver DHCP actually handed out
  instead of reading the first entry from `/etc/resolv.conf`, which can hold the
  `1.1.1.1`/`8.8.8.8` fallback or stale entries. The DHCP nameserver is sourced
  from the dhcpcd lease (`--dumplease`, then `-U`), falling back to
  `/etc/resolv.conf` only when no lease DNS is available
- Read DHCP DNS from dhcpcd's per-interface resolv.conf fragments under
  `/run/dhcpcd/resolv.conf/<iface>.*` (legacy `/var/run` too) as the primary,
  most reliable source. These are plain nameserver lines written straight from
  the DHCP ACK, keyed by interface, and need neither the lease database nor a
  version-specific dump subcommand — the lease dump frequently finds nothing in
  a Town OS initrd. The resolv.conf writer and DNS check now share one source
  chain: run-dir fragments → `--dumplease` → `-U` → existing `/etc/resolv.conf`
  → public resolvers

### Improvements

- Run all podman invocations (build and run) with `--network=host` so
  apt/rustup/cargo can resolve DNS in nested/sandboxed environments; document
  the rule in CLAUDE.md
- Add tests covering resolv fragment combination (single, cross-file dedup,
  search/domain, none-without-nameserver), directory reads (interface-prefix
  matching, missing-dir tolerance), and the shared `first_nameserver` helper

## 0.4.1 (2026-06-07)

### Features

- **aarch64 support** — ttyforce now runs on aarch64 (validated on Apple
  Silicon / Asahi Linux). Hardware and disk detection are architecture-
  independent: they read generic sysfs properties and the kernel `root=`
  parameter rather than any x86-specific assumptions or device-name prefix
  lists, so the same detection path works identically on x86_64 and aarch64
- Support USB-attached drives as installation targets — previously all removable
  devices were filtered out, so a USB/SD data drive could never be selected

### Fixes

- Never offer the disk the running system booted from as an installation target,
  even when it has unused/unpartitioned space or is the USB stick the machine
  booted from. The boot disk is resolved from `/proc/mounts` and the kernel
  `root=` parameter (`UUID=`/`LABEL=`/`PARTUUID=`/`/dev` forms), then excluded by
  whole disk in both the sysfs (initrd) and UDisks2 detection paths
- Self-heal the evdev watcher when a keyboard returns a non-recoverable read
  error (e.g. `ENODEV` from hot-unplug or USB autosuspend): drop the dead device
  instead of logging the same error every tick. Other keyboards and crossterm
  input keep working

### Improvements

- Add hermetic integration tests for boot-disk exclusion (fake `/sys/block`, no
  host access) plus unit tests for the `/proc/mounts` and `root=` parsing helpers

## 0.4.0 (2026-04-19)

### Features

- Add Easy/Advanced installation style selection, shown for all installer entry points
- Add `--log` flag to getty for a full-screen log view at launch
- Add scrollback to the getty journal panes

### Improvements

- Easy mode: poll all ethernet interfaces for carrier for up to 30s before falling back
- Wait for the full DHCP handshake before proceeding, with verbose detection logging
- Wait 10x longer for network carrier before giving up
- Mirror btrfs metadata on parity (RAID5) arrays instead of matching the data profile
- Set a default 24x80 terminal size on bare serial TTYs
- Use horizontal-only padding for the getty quad panes

### Fixes

- Disable DHCP-provided DNS in generated networkd `.network` files

## 0.3.2 (2026-03-29)

### Improvements

- Tabular audit log in getty with structured entries and highlighted fields
- Update README with missing CLI flags for initrd and getty modes

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
