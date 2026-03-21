#!/bin/bash
set -euo pipefail

# ──────────────────────────────────────────────────────────────────────
# run-tests.sh — run integration tests inside an isolated systemd scope
#
# This script is the CMD of the integration container. It:
#   1. Starts dbus, systemd-networkd, systemd-resolved, and udisks2
#   2. Creates loopback block devices for disk tests
#   3. Creates a dummy network interface for network tests
#   4. Runs all test suites: integration, playbook, fixture, scenario, mixed
#   5. Tears down test devices
# ──────────────────────────────────────────────────────────────────────

LOOPDEV_DIR=/var/lib/ttyforce-test
NUM_LOOP_DEVICES=4
LOOP_SIZE_MB=256

cleanup() {
    echo "=== Cleaning up ==="
    # Remove dummy interface and its networkd config
    ip link del dummy0 2>/dev/null || true
    rm -f /etc/systemd/network/10-dummy0.netdev /etc/systemd/network/10-dummy0.network

    # Detach loop devices
    for f in "$LOOPDEV_DIR"/disk*.img; do
        losetup -j "$f" 2>/dev/null | cut -d: -f1 | while read -r dev; do
            losetup -d "$dev" 2>/dev/null || true
        done
    done

    rm -f "$LOOPDEV_DIR"/disk*.img
}
trap cleanup EXIT

echo "=== Starting dbus ==="
mkdir -p /run/dbus
dbus-daemon --system --fork 2>/dev/null || true

echo "=== Configuring dummy network interface via systemd-networkd ==="
# Create the dummy interface via a .netdev unit
cat > /etc/systemd/network/10-dummy0.netdev <<NETDEV
[NetDev]
Name=dummy0
Kind=dummy
NETDEV

# Configure it with a static address via a .network unit
cat > /etc/systemd/network/10-dummy0.network <<NETWORK
[Match]
Name=dummy0

[Network]
Address=10.99.99.1/24
DHCP=no
NETWORK

echo "=== Starting systemd-networkd ==="
systemctl start systemd-networkd 2>/dev/null || \
    /usr/lib/systemd/systemd-networkd &

echo "=== Starting systemd-resolved ==="
# --dns=none in the container run command prevents the runtime from
# bind-mounting /etc/resolv.conf, so resolved can manage it directly.
ln -sf /run/systemd/resolve/stub-resolv.conf /etc/resolv.conf
systemctl start systemd-resolved 2>/dev/null || \
    /usr/lib/systemd/systemd-resolved &

echo "=== Starting udisks2 ==="
systemctl start udisks2 2>/dev/null || \
    /usr/libexec/udisks2/udisksd &

# networkd may not reliably manage interfaces when not running under systemd
# init, so create the interface and assign the IP directly as well.
ip link add dummy0 type dummy 2>/dev/null || true
ip link set dummy0 up
ip addr add 10.99.99.1/24 dev dummy0 2>/dev/null || true
networkctl reconfigure dummy0 2>/dev/null || true
sleep 1

echo "=== Verifying dummy0 is managed by networkd ==="
networkctl status dummy0 2>/dev/null || ip addr show dummy0

echo "=== Creating loop block devices ==="
mkdir -p "$LOOPDEV_DIR"
LOOP_DEVICES=""
for i in $(seq 1 $NUM_LOOP_DEVICES); do
    img="$LOOPDEV_DIR/disk${i}.img"
    dd if=/dev/zero of="$img" bs=1M count=$LOOP_SIZE_MB status=none
    dev=$(losetup --find --show "$img")
    echo "  $dev -> $img"
    if [ -n "$LOOP_DEVICES" ]; then
        LOOP_DEVICES="${LOOP_DEVICES},${dev}"
    else
        LOOP_DEVICES="${dev}"
    fi
done

echo "=== Running tests ==="
export TTYFORCE_TEST_IFACE=dummy0
export TTYFORCE_TEST_LOOP_DEVICES="$LOOP_DEVICES"

cd /build
exit_code=0

echo ""
echo "--- Integration tests (real systemd operations) ---"
cargo test --test integration_tests -- --test-threads=1 || exit_code=1

echo ""
echo "--- Playbook tests (input/operation verification) ---"
cargo test --test playbook_tests || exit_code=$((exit_code | $?))

echo ""
echo "--- Scenario tests ---"
cargo test --test scenario_tests || exit_code=$((exit_code | $?))

echo ""
echo "--- Fixture tests ---"
cargo test --test fixture_tests || exit_code=$((exit_code | $?))

echo ""
echo "--- Mixed disk tests ---"
cargo test --test mixed_disk_tests || exit_code=$((exit_code | $?))

echo ""
echo "--- Unit tests ---"
cargo test --lib || exit_code=$((exit_code | $?))

echo ""
echo "=== Done (exit $exit_code) ==="
exit $exit_code
