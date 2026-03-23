# TERMS:

- input: data about hardware, connection status, etc.
- operation: an action that is taken on the system

# RULES:

- tests: unless said otherwise, they perform with simulated input and produce output on the operations that would be performed. They never affect the running system.
- running tests: use the make tasks every time.
- tests should always include the linting checks
- lint checks should be a rust community standard of linters, run as the `lint` make tasks

# CLI:

The binary has four subcommands and two global flags.

## Subcommands

- `detect` — Detect hardware and print the hardware manifest to stdout. With
  `--fixture <file>`, runs a scripted scenario file and prints the resulting
  operations manifest instead.
- `output` — Detect real hardware (or load via `-i`), run the full TUI with a
  mock executor (no real side effects), then print the operations that would
  have been performed. This is a dry-run mode.
- `run` — Detect hardware (or load via `-i`) and launch the real installer
  using systemd (dbus, networkd, resolved, logind).
- `initrd` — Run installer in initrd mode using syscalls and sysfs directly
  (no systemd dbus). Has its own flags:
  - `--etc-target <DIR>` — Target directory for /etc config files. Defaults
    to the mount point. Use when /etc is an overlay from a different path.

## Global flags

- `-i, --input <FILE>` — Load hardware from a manifest file instead of
  auto-detecting. Works with all subcommands.
- `-o, --output <FILE>` — Write output to a file instead of stdout. Works
  with all subcommands.

## Examples

```bash
ttyforce detect                          # auto-detect, print hardware manifest
ttyforce detect -i hw.toml               # print manifest from file
ttyforce detect -o hw.toml               # auto-detect, save to file
ttyforce detect --fixture scenario.toml  # run scenario, print operations
ttyforce output                          # dry-run with real hardware
ttyforce output -i hw.toml               # dry-run with manifest from file
ttyforce run                             # real installer (systemd)
ttyforce initrd                          # real installer (initrd mode)
ttyforce initrd --etc-target /mnt/root   # initrd, write configs to /mnt/root/etc
ttyforce run -i hw.toml                  # TUI with mock executor
```

# DESIGN:

A rust-based text user interface for installing Town OS. It should
be presented when the user would normally be expected to provision disks.

This text user interface should work similarly to nmtui but should make the
focus getting on the internet. If there is a clear path to the internet, it
should just ask if the user wants to change it but show them how it will
connect. That way, if they need to configure wireless they can, but if there is
an option to connect on the wire already, it should just ask.

This text interface should also ask the user what to do with the disks that
exist on the system. Offer configurations:

Group storage by make and model automatically, and offer the user a choice of
which group to provision.

Then offer RAID options with explanations:

- 1 disk - all one drive
- 2 disks - mirror
- 3+ Disks - raidz

The filesystem is always Btrfs. The installation target mount point defaults
to `/town-os`.

This should be testable -- a manifest of actions taken in this case instead of
actually taking them. Likewise, inputs for the available wifi networks, ethernet
configurations, and storage options should be able to be accepted as a manifest
for how to present the installer. The result is an installer that can totally
be fed hardware configurations and let the user mess around in this virtual
environment, and then generate a result of actions to be taken that can be
analyzed later.

Write a set of fixtures that runs through the installer setting up common
hardware and enviornment configurations:

- ethernet + 4 disks all same
- ethernet + 1 disk
- wifi + 1 disk
- wifi in crowded neighborhood + 1 disk
- wifi + ethernet + 4 disks
- wifi + ethernet + 1 disk
- wifi + dead ethernet + 1 disk
- wifi + dead ethernet + 4 disks

Now, write a consistent plan of operations:

- turning a network device on
- scanning a wifi network
- checking for link availability
- performing wifi authentication checks
- supporting automatic wifi configuration, such as qr codes
- checking for ip address
- checking for internet routability
- checking for upstream router
- configuring dhcp for interface
- configuring wifi ssid and authentication for interface
- receiving a list of available ssids with signal strength, etc.
- wifi connection timeout
- wifi auth error
- selection of primary interface - other interfaces should be shut down
- DNS resolution works
- network completely online

Then, I want you to mix these fixtures, and selections of the options available
in the fixtures (assume that the ssid may or may not be connected to when
dealing with names and passwords) with the operations that should be run.

Examples:

- selecting a wifi network from a list of them.
- refreshing the list with a new scan. the list should change.
- connecting to a wifi network and being prompted for a password.
- entering an incorrect password and getting an error back.
- connecting to a wifi network that has a signal timeout (short).
- successful connection to a wifi network with ip provisioning.
- locating and ip provisioning the correct default device:
    - first, connected ethernet
    - second, available wifi devices
    - third, ask user after listing available devices

Please ensure all these mixes are tested. Please provide them separate from the
code so they can be manipulated independently. Inputs should generate a series
of operations; running the inputs should generate the list of operations.

Inputs should drive interactions in the TUI which may involve internal state, or the generation of operations (such as move a file, talk to the network, etc) that should be executed.

The result would be that in a real scenario, those operations will be evaluated immediately, and their results would be fed back as error or state changes, which then might interrupt the input for prepending, such as a ssid list, to wifi access point selection, to password entry, to network negotiation and online status, including DNS resolution of e.g. example.com, but resulting in a full series of inputs and any errors states in-between, and the operations that would have been performed in the series of errors to get to a final state of "installed" or "aborted". The option to reboot the machine should also be available.

Please ensure any other additional functionality is tested.

# INITRD MODE:

The `--initrd` flag selects the `InitrdExecutor` which avoids all systemd
dbus calls. Two executor backends exist:

- `SystemdExecutor` (default) — uses systemd-networkd, systemd-resolved,
  and logind via dbus, with sysfs/command fallbacks
- `InitrdExecutor` (`--initrd`) — uses syscalls and sysfs directly, with
  minimal external tool dependencies

## Syscalls used (no external tools needed):

- Interface up/down — `ioctl(SIOCSIFFLAGS)` to set/clear `IFF_UP`
- IP address check — `ioctl(SIOCGIFADDR)` to read IPv4 address
- Link/carrier check — reads sysfs `/sys/class/net/<iface>/carrier`
- Route/gateway check — parses `/proc/net/route`
- Internet reachability — ICMP echo via `SOCK_DGRAM/IPPROTO_ICMP` raw
  socket, with `SOCK_RAW` fallback, with `ping` command final fallback
- DNS resolution — builds DNS A query, sends via `UdpSocket` to
  nameserver from `/etc/resolv.conf`, parses response
- Mount/unmount — `mount(2)` via `nix::mount::mount()` /
  `umount2(2)` via `nix::mount::umount2()`
- Reboot — `reboot(2)` via `nix::sys::reboot::reboot()` with
  `sync()` beforehand

## External tools required in the initrd:

- `dhcpcd` — DHCP client (protocol too complex for inline implementation)
- `wpa_supplicant` — WPA authentication, CLI mode only, no dbus
  (`wpa_supplicant -B -i <iface> -c <conf>`)
- `iw` — wifi network scanning (`iw dev <iface> scan`), fallback: `iwlist`
- `parted` — disk partitioning (GPT + single partition)
- `mkfs.btrfs` — btrfs filesystem creation
- `btrfs` — subvolume management
- `pacstrap` or `<mount>/install.sh` — base system installation
- `pkill` — cleanup of dhcpcd/wpa_supplicant processes

## Config persistence:

After a successful install, the `PersistNetworkConfig` operation writes:
- `<etc_target>/etc/systemd/network/20-<iface>.network` — networkd DHCP unit
  matched by MAC address (not interface name) so it works regardless of
  interface naming scheme (initrd may use `eth0` while booted system uses
  `enp3s0`). Falls back to name matching if MAC is unavailable.
- `<etc_target>/etc/wpa_supplicant/wpa_supplicant-<iface>.conf` — if wifi was
  used, copies the wpa_supplicant config from `/tmp/`

The `etc_target` defaults to the mount point but can be overridden with
the `--etc-target` flag on the `initrd` subcommand.

systemd-networkd must be enabled on the installed system to pick up
the `.network` file. ttyforce assumes it is already enabled.

## DNS / nameserver setup in initrd:

After dhcpcd obtains a DHCP lease, `/etc/resolv.conf` is written from
the lease data using this fallback chain:
1. `dhcpcd --dumplease <iface>` — parse `domain_name_servers=` field
2. `dhcpcd -U <iface>` — alternative dump format
3. Check if `/etc/resolv.conf` already has nameservers (dhcpcd hooks)
4. Fallback: write `1.1.1.1` and `8.8.8.8` as default nameservers

This is necessary because initrd environments often lack dhcpcd's
hook scripts that normally manage resolv.conf.

## Command output pane:

The TUI has a persistent command log pane in the bottom half of the
screen, visible on every screen. All shell commands and syscall
operations are logged with arguments and results, color-coded:
- Yellow: command invocation (`$ cmd args`)
- Green: success (`-> ok`)
- Red: errors (`-> FAILED`, `error:`)

## Serial console logging:

All command log entries are also written to `/dev/ttyS0` (serial
console) for debugging when the TUI is running on a different TTY.

## Internet accessibility:

The installer ensures internet is accessible before proceeding to disk
setup. Both ethernet and wifi flows check:
1. Upstream router reachable (gateway exists)
2. Internet routable (ICMP ping to 1.1.1.1)
3. DNS works (resolve example.com)

If any of these fail, the flow stops with an error on the NetworkProgress
screen. This applies to both systemd and initrd executors.

## Btrfs RAID mount:

Before mounting a btrfs filesystem, `btrfs device scan` is run so the
kernel discovers all RAID member devices. Without this, mounting a
single member partition may fail in initrd environments.

## Mount service generation:

After a successful install, a systemd service unit `mount-town-os.service`
is written to `<mount>/etc/systemd/system/` and enabled via symlink in
`local-fs.target.wants/`. This service:
- Runs `mkdir -p /town-os` and `btrfs device scan` before mounting
- Mounts the btrfs volume with `subvol=@` at the configured mount point
- Runs before `local-fs.target` and `multi-user.target`
- Uses a service unit (not a .mount unit) to avoid systemd's
  path-escaping issues with hyphens in mount points

The service uses `ConditionPathIsMountPoint=!<mount>` to succeed
immediately if already mounted, and `-` prefix on ExecStartPre so
mkdir and btrfs scan failures are non-fatal.

A .mount unit is NOT used because systemd interprets hyphens in unit
names as path separators (`town-os.mount` → `/town/os`), which is wrong.
Fstab is NOT used because `/etc` is an overlay of `/town-os/etc`.

After a successful install, the volume is unmounted (`CleanupUnmount`)
so systemd doesn't see it in `/proc/mounts` and auto-generate an
invalid `town-os.mount` unit. The service handles remounting on boot.

## Root partition and /etc write policy:

ttyforce NEVER modifies the root partition. The btrfs volume at the
configured mount point (`/town-os` by default) is a data/system
partition, NOT the root partition. The root filesystem is not affected.

Files written during install — all go inside `<mount_point>/etc/`
(the Town OS /etc overlay, NOT the root /etc):
- `<mount>/etc/systemd/system/mount-town-os.service` — mount service
- `<mount>/etc/systemd/system/local-fs.target.wants/` — enable symlink
- `<mount>/etc/systemd/network/20-<iface>.network` — networkd DHCP unit
- `<mount>/etc/wpa_supplicant/wpa_supplicant-<iface>.conf` — wifi config

The ONLY write to the running system's /etc during initrd mode is
`/etc/resolv.conf` — this is necessary for DNS resolution during the
install process and is overwritten by the installed system on boot.

## TUI layout:

The TUI has three main sections:
- Title bar (3 lines)
- Content area (flexible, gets all remaining space)
- Command output pane (10 lines, scrolls to bottom)
- Status bar (3 lines)

The content area has priority over the command pane to ensure disk
groups and other selections are always visible.

## Disk detection:

In initrd mode, disk detection uses generic sysfs properties instead of
dbus/UDisks2. A block device in `/sys/block/` is considered a real disk if:
1. `removable` is not `1` (filters USB sticks, CD-ROMs, floppies)
2. `size` > 0 (filters uninitialized devices)
3. `device/` subdirectory exists (filters loop, ram, dm, zram — virtual devices)
4. Size >= 1GB (filters USB boot media)

This approach works for any disk type (sd*, nvme*, vd*, hd*, xvd*, mmcblk*)
without maintaining name prefix lists.

## Architecture:

Both executors implement the same `OperationExecutor` trait. The `Operation`
enum and state machine are shared — only the executor implementation differs.
The initrd executor code lives in `src/engine/initrd_ops/` mirroring
`src/engine/real_ops/`.

- don't commit or push unless I tell you to
- do not publish github releases, publish means crates.io
- TEST EVERYTHING
