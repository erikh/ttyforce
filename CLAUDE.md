# TERMS:

- input: data about hardware, connection status, etc.
- operation: an action that is taken on the system

# RULES:

- ensure deny(dead_code) and deny(unsafe) are at the top and honored
- handle all std::result::Result in an appropriate way
- do not use unwrap
- do not use unsafe code
- write tests for everything, including integration and real tests
- use make test to validate any changes
- integration tests should not alter the host, ever
- tests: unless said otherwise, they perform with simulated input and produce output on the operations that would be performed. They never affect the running system.
- running tests: use the make tasks every time.
- tests should always include the linting checks
- lint checks should be a rust community standard of linters, run as the `lint` make tasks
- never use `let _ = expr;` to suppress unused variable warnings or work around the borrow checker. Fix the actual problem: use the variable, remove the parameter, or restructure the code.
- `#![deny(dead_code)]` and `#![deny(unsafe_code)]` are set at the crate level in both lib.rs and main.rs. Never add `#[allow(dead_code)]` or `#[allow(unsafe_code)]` to bypass them — remove dead code, and use safe abstractions (e.g., nix crate) instead of unsafe.
- do not modify the system beyond configuring hardware
- please check all std::result::Results and handle accordingly

# CLI:

The binary has five subcommands and two global flags.

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
    - `--etc-prefix <DIR>` — Directory that maps to /etc on the installed
      system. Defaults to `<mount_point>/@etc`. Files are written directly
      under this path (e.g., `<DIR>/systemd/network/`).
    - `--tty <DEVICE>` — TTY device to use for the TUI (e.g., `/dev/tty1`,
      `/dev/ttyS0`). Redirects stdin/stdout to the specified device.
- `getty` — Run as a getty replacement (login screen with system status).
  Designed to be invoked by agetty instead of `/bin/login`. Displays
  machine info, network/mDNS status, system stats (CPU, memory, disk),
  and service health via the Town OS API at `localhost:5309`. While
  services are starting, shows live `journalctl -f` output. Has its
  own flags:
    - `--etc-prefix <DIR>` — Same as initrd; passed through to
      reconfigure subprocess.
    - `--tty <DEVICE>` — TTY device to use for the TUI.
    - `--console` — Listen to `/dev/kmsg` and force a full TUI repaint
      whenever kernel messages arrive. Use this when running on a console
      TTY (e.g. `/dev/tty1`) where kernel/systemd messages bleed through
      and corrupt the display.
    - `--shell` — Enable `[q]` Shell action to drop into `/bin/bash`.
    - `--initrd` — Use initrd mode for reconfigure (spawns
      `ttyforce initrd` instead of `ttyforce run`).

    Actions (key-triggered):
    - `[.]` Login — clears the screen, displays `/etc/issue` with
      agetty-style escape substitutions (\n hostname, \l tty, \d date,
      \t time, \s OS, \m arch, \r kernel), then `exec`s into
      `/bin/login`. agetty respawns ttyforce after the shell exits.
    - `[q]` Shell (only when `--shell` is passed) — spawns `/bin/bash`
      as a child process, resumes getty when the shell exits.
    - `[l]` Log — show live journalctl output panel.
    - `[s]` Status — show service status panel.
    - `[r]` Reconfigure — spawns `ttyforce run` (or `ttyforce initrd`
      when `--initrd` is set) as a child process with the same
      `--etc-prefix`/`--tty` flags, resumes getty when done.
    - `[R]` Reboot — reboots the machine.
    - `[p]` Power Off — powers off the machine.
    - `[!]` Sledgehammer — requires typing "SLEDGEHAMMER" to confirm.
      Discovers btrfs member devices, installs a systemd service
      (`ttyforce-sledgehammer.service`) that wipes all disks during
      shutdown (after all filesystems are unmounted), then reboots.

    IMPORTANT — startup panel behavior (do not change):
    At getty startup, if services are NOT all active, the log panel
    (live `journalctl -f` output) is shown automatically with a
    "Services starting" header. This continues UNTIL all services
    become active, at which point it auto-switches to the status
    panel. After that, `l` and `s` toggle between the two panels
    freely. This behavior must not be altered.

    Service status is fetched from the Town OS API at
    `GET /systemd/units?limit=100` on `localhost:5309`.
    Authentication uses a bearer token from `TTYFORCE_API_TOKEN` env
    var or `<etc_prefix>/ttyforce/api-token` file.

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
ttyforce initrd --etc-prefix /mnt/root   # initrd, write configs to /mnt/root/systemd/network/
ttyforce run -i hw.toml                  # TUI with mock executor
ttyforce getty                           # getty replacement (system status + login)
ttyforce getty --tty /dev/tty1           # getty on specific TTY
ttyforce getty --etc-prefix /mnt/root    # getty with custom etc prefix
ttyforce getty --initrd                  # getty with initrd reconfigure mode
ttyforce getty --shell                   # getty with [q] shell action
ttyforce getty --console --initrd        # getty on console TTY in initrd mode
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

## Safe system access (no unsafe code):

- Interface up/down — `ip link set <iface> up/down`
- IP address check — `ip -4 -o addr show <iface>`
- Link/carrier check — reads sysfs `/sys/class/net/<iface>/carrier`
- Route/gateway check — parses `/proc/net/route`
- Internet reachability — `ping -c1 -W<timeout> <addr>`
- DNS resolution — builds DNS A query, sends via `UdpSocket` to
  nameserver from `/etc/resolv.conf`, parses response
- Mount/unmount — `mount(2)` via `nix::mount::mount()` /
  `umount2(2)` via `nix::mount::umount2()`
- Reboot — `reboot(2)` via `nix::sys::reboot::reboot()` with
  `sync()` beforehand

## External tools required in the initrd:

- `ip` — interface up/down and IP address queries
- `ping` — internet reachability check
- `dhcpcd` — DHCP client (protocol too complex for inline implementation)
- `wpa_supplicant` — WPA authentication, CLI mode only, no dbus
  (`wpa_supplicant -B -i <iface> -c <conf>`)
- `wpa_cli` — WPS push-button connection (`wpa_cli -i <iface> wps_pbc`)
  and status polling (`wpa_cli -i <iface> status`)
- `iw` — wifi network scanning (`iw dev <iface> scan`), fallback: `iwlist`
- `rfkill` — unblock wifi radio before detection (best-effort)
- `modprobe` — load wifi kernel modules in initrd (best-effort)
- `parted` — disk partitioning (GPT + single partition)
- `mkfs.btrfs` — btrfs filesystem creation
- `btrfs` — subvolume management
- `<mount>/install.sh` — custom install script (optional)
- `pkill` — cleanup of dhcpcd/wpa_supplicant processes
- `curl` — fetch SSH public keys from GitHub (`https://github.com/<user>.keys`)

## Config persistence:

After a successful install, the `PersistNetworkConfig` operation writes:

- `<etc_prefix>/systemd/network/20-<iface>.network` — networkd DHCP unit
  matched by MAC address (not interface name) so it works regardless of
  interface naming scheme (initrd may use `eth0` while booted system uses
  `enp3s0`). Falls back to name matching if MAC is unavailable.
  Includes `MulticastDNS=yes` for mDNS (.local) hostname resolution.
- `<etc_prefix>/wpa_supplicant/wpa_supplicant-<iface>.conf` — if wifi was
  used, copies the wpa_supplicant config from `/tmp/`

If the user provided GitHub usernames, `ImportSshKeys` writes:

- `/root/.ssh/authorized_keys` — SSH keys on the live system (immediate use)
- `<etc_prefix>/ssh/authorized_keys.d/github` — persisted copy for
  boot-time restoration via the etc overlay

The `etc_prefix` is the directory that corresponds to `/etc` on the
installed system. It defaults to `<mount_point>/@etc` (the Town OS
`@etc` btrfs subvolume). Files are written directly under this path
(no extra `/etc/` prefix is added).

When `--etc-prefix` is specified, it overrides the default and is used
as-is for all config writes.

Example: `ttyforce initrd --etc-prefix /overlays/etc` writes to `/overlays/etc/systemd/network/`.
Example: `ttyforce initrd --etc-prefix /mnt/root/etc` writes to `/mnt/root/etc/systemd/network/`.
Without `--etc-prefix`, writes go to `<mount_point>/@etc/` (e.g. `/town-os/@etc/systemd/network/`).

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

## Internet accessibility:

The installer ensures internet is accessible before proceeding to disk
setup. Both ethernet and wifi flows check:

1. Upstream router reachable (gateway exists)
2. Internet routable (ICMP ping to 1.1.1.1)
3. DNS works (resolve example.com)

If any of these fail, the flow stops with an error on the NetworkProgress
screen. This applies to both systemd and initrd executors.

## Install operation order:

IMPORTANT: All file operations that configure the machine (writing to
`etc_prefix`, persisting network config, importing SSH keys, etc.)
MUST come after filesystem creation (partition, mkfs, mount, subvolume
creation). The target paths do not exist until the btrfs volume is
mounted and subvolumes are created. Never reorder config writes before
filesystem setup.

1. PartitionDisk (each device)
2. MkfsBtrfs or BtrfsRaidSetup
3. MountFilesystem (btrfs at /town-os)
4. CreateBtrfsSubvolume (@etc, @var — Town OS overlay subvolumes)
5. GenerateFstab (mount service to <etc_prefix>/systemd/system/)
6. PersistNetworkConfig (networkd unit + wpa config to <etc_prefix>/)
7. ImportSshKeys (fetch from GitHub, write to /root/.ssh/ + persist to <etc_prefix>/)
8. InstallBaseSystem (runs install.sh if present, otherwise no-op)
9. CleanupUnmount (final unmount so systemd doesn't see stale mount)

ttyforce does NOT create @, @home, @snapshots subvolumes. It creates
@etc and @var which Town OS's make-btrfs.sh expects. The actual overlay
setup (mounting @etc to /overlays/etc, adding fstab entries) is done
by Town OS's make-storage.sh service at boot, not by ttyforce.

Config files are written to `--etc-prefix` (defaults to `<mount_point>/@etc`).
This should be set to wherever the overlay upperdir is accessible
during the initrd phase.

## Btrfs RAID mount:

Before mounting a btrfs filesystem, `btrfs device scan` is run so the
kernel discovers all RAID member devices. Without this, mounting a
single member partition may fail in initrd environments.

## Mount service generation:

After a successful install, a systemd service unit `mount-town-os.service`
is written to `<etc_prefix>/systemd/system/` and enabled via symlink in
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

Files written during install — all go inside the `@etc` btrfs subvolume
(`<mount_point>/@etc/` by default, overridable via `--etc-prefix`):

- `<etc_prefix>/systemd/system/mount-town-os.service` — mount service
- `<etc_prefix>/systemd/system/local-fs.target.wants/` — enable symlink
- `<etc_prefix>/systemd/network/20-<iface>.network` — networkd DHCP unit
- `<etc_prefix>/wpa_supplicant/wpa_supplicant-<iface>.conf` — wifi config
- `<etc_prefix>/ssh/authorized_keys.d/github` — persisted SSH keys

Writes to the running system (not etc_prefix):
- `/etc/resolv.conf` — DNS resolution during install (overwritten on boot)
- `/root/.ssh/authorized_keys` — SSH keys for immediate use

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

## Town OS install system:

Reference: https://gitea.com/town-os/install

Town OS boots from a squashfs image (`root.sfs`) on an ext4 data
partition, with a tmpfs overlay for writability. The boot process:

1. initcpio `town-squashfs` hook mounts ext4 data partition read-only
2. Mounts `root.sfs` as squashfs via loop device
3. Creates overlay: squashfs (lower) + tmpfs (upper) = new root
4. Moves mounts to `/.town/` subdirectories

After boot, `town-os-make-storage.service` runs `make-storage.sh` →
`make-btrfs.sh` which:

1. Detects disks and creates btrfs (single/raid1/raid5)
2. Mounts at `/town-os`
3. Creates subvolumes `@var` and `@etc`
4. Mounts `@var` → `/overlays/var`, `@etc` → `/overlays/etc`
5. Adds overlay entries to `/etc/fstab`:
    - overlay for `/etc` (lower=squashfs, upper=`/overlays/etc`)
    - overlay for `/var` (lower=squashfs, upper=`/overlays/var`)
6. Reloads systemd and mounts all

ttyforce replaces step 1-3 of the `make-btrfs.sh` flow (disk detection,
btrfs creation, mounting). The subvolumes it creates must match what
Town OS expects (`@var`, `@etc`), and network config must be written
to the `@etc` subvolume so it appears in `/etc` via the overlay.

The `--etc-prefix` flag should point to wherever the `@etc` subvolume
is accessible (defaults to `<mount_point>/@etc`). The path is used
directly — no `/etc/` prefix is added.

NOTE: ttyforce does NOT install the base system — the system is
already installed via squashfs. InstallBaseSystem runs `install.sh`
if present, otherwise it is a no-op.

## Architecture:

Both executors implement the same `OperationExecutor` trait. The `Operation`
enum and state machine are shared — only the executor implementation differs.
The initrd executor code lives in `src/engine/initrd_ops/` mirroring
`src/engine/real_ops/`.

- don't commit or push unless I tell you to
- do not publish github releases, publish means crates.io
- TEST EVERYTHING
