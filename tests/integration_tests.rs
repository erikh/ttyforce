//! Integration tests for SystemdExecutor.
//!
//! These tests run inside a container with systemd, dbus, a dummy network
//! interface, and loop block devices.  They exercise the real code paths
//! in `real_ops/` against actual kernel subsystems.
//!
//! Environment variables (set by integration/run-tests.sh):
//!   TTYFORCE_TEST_IFACE        — dummy network interface name (e.g. "dummy0")
//!   TTYFORCE_TEST_LOOP_DEVICES — comma-separated loop device paths
//!
//! Run locally:
//!   make integration
//!
//! Skip when not in the container:
//!   cargo test --test integration_tests  (will skip all tests gracefully)

use ttyforce::engine::executor::{InitrdExecutor, OperationExecutor, SystemdExecutor};
use ttyforce::engine::feedback::OperationResult;
use ttyforce::operations::Operation;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn test_iface() -> Option<String> {
    std::env::var("TTYFORCE_TEST_IFACE").ok()
}

fn loop_devices() -> Vec<String> {
    std::env::var("TTYFORCE_TEST_LOOP_DEVICES")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

macro_rules! require_env {
    ($var:expr) => {
        match $var {
            Some(v) => v,
            None => {
                eprintln!("skipping (not in integration container)");
                return;
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Network — interface management
// ---------------------------------------------------------------------------

#[test]
fn integration_enable_interface() {
    let iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::EnableInterface {
        interface: iface.clone(),
    });
    assert!(
        result.is_success(),
        "EnableInterface failed: {:?}",
        result
    );

    // recorded
    assert_eq!(exec.recorded_operations().len(), 1);
}

#[test]
fn integration_disable_and_reenable_interface() {
    let iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    let r1 = exec.execute(&Operation::DisableInterface {
        interface: iface.clone(),
    });
    assert!(r1.is_success(), "DisableInterface failed: {:?}", r1);

    // re-enable so later tests aren't affected
    let r2 = exec.execute(&Operation::EnableInterface {
        interface: iface.clone(),
    });
    assert!(r2.is_success(), "re-EnableInterface failed: {:?}", r2);
}

#[test]
fn integration_check_link_availability() {
    let iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    // Make sure it's up
    exec.execute(&Operation::EnableInterface {
        interface: iface.clone(),
    });

    let result = exec.execute(&Operation::CheckLinkAvailability {
        interface: iface.clone(),
    });

    // dummy interfaces report carrier once up
    match &result {
        OperationResult::LinkUp => {}
        OperationResult::LinkDown => {
            // acceptable for dummy — carrier file may read 0
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

#[test]
fn integration_check_ip_address() {
    let iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::CheckIpAddress {
        interface: iface.clone(),
    });

    // run-tests.sh adds 10.99.99.1/24 to dummy0
    match &result {
        OperationResult::IpAssigned(ip) => {
            assert_eq!(ip, "10.99.99.1", "unexpected IP: {}", ip);
        }
        other => panic!("expected IpAssigned, got {:?}", other),
    }
}

#[test]
fn integration_check_upstream_router_no_route() {
    let iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    // dummy0 has no default route, so we should get NoRouter
    let result = exec.execute(&Operation::CheckUpstreamRouter {
        interface: iface.clone(),
    });
    assert!(
        matches!(result, OperationResult::NoRouter),
        "expected NoRouter, got {:?}",
        result
    );
}

#[test]
fn integration_check_dns_resolution() {
    let _iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    // localhost should always resolve
    let result = exec.execute(&Operation::CheckDnsResolution {
        interface: "dummy0".into(),
        hostname: "localhost".into(),
    });
    match &result {
        OperationResult::DnsResolved(ip) => {
            assert!(
                ip.starts_with("127.") || ip == "::1",
                "unexpected localhost resolution: {}",
                ip
            );
        }
        OperationResult::DnsFailed(msg) => {
            // dig/getent may not be able to resolve in a minimal container;
            // this is acceptable — the code path was exercised
            eprintln!("DNS resolution failed (expected in minimal container): {}", msg);
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

#[test]
fn integration_shutdown_interface() {
    let iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::ShutdownInterface {
        interface: iface.clone(),
    });
    assert!(result.is_success(), "ShutdownInterface failed: {:?}", result);

    // bring it back up
    exec.execute(&Operation::EnableInterface {
        interface: iface.clone(),
    });
}

#[test]
fn integration_select_primary_interface() {
    let iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    // This will fail to add a default route (no gateway) but should not panic
    let result = exec.execute(&Operation::SelectPrimaryInterface {
        interface: iface.clone(),
    });
    // May be Error (no gateway) or Success — either is acceptable
    match &result {
        OperationResult::Success | OperationResult::Error(_) => {}
        other => panic!("unexpected result: {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Network — error handling for missing interface
// ---------------------------------------------------------------------------

#[test]
fn integration_enable_nonexistent_interface() {
    require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::EnableInterface {
        interface: "doesnotexist99".into(),
    });
    assert!(
        matches!(result, OperationResult::Error(_)),
        "expected error for nonexistent iface, got {:?}",
        result
    );
}

#[test]
fn integration_check_link_nonexistent_interface() {
    require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::CheckLinkAvailability {
        interface: "doesnotexist99".into(),
    });
    assert!(
        matches!(result, OperationResult::LinkDown),
        "expected LinkDown for missing iface, got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Network — wifi operations (no real wifi hardware, expect graceful errors)
// ---------------------------------------------------------------------------

#[test]
fn integration_scan_wifi_no_hardware() {
    require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    // dummy0 is not a wifi interface — scan should fail gracefully
    let result = exec.execute(&Operation::ScanWifiNetworks {
        interface: "dummy0".into(),
    });
    match &result {
        OperationResult::WifiScanResults(nets) => {
            // empty results are fine
            assert!(nets.is_empty());
        }
        OperationResult::Error(_) => {
            // expected — no wifi hardware
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

#[test]
fn integration_authenticate_wifi_no_hardware() {
    require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::AuthenticateWifi {
        interface: "dummy0".into(),
        ssid: "TestNetwork".into(),
        password: "testpass".into(),
    });
    // Should get an auth failure — no wpa_supplicant running for dummy
    match &result {
        OperationResult::WifiAuthFailed(_) | OperationResult::Error(_) => {}
        OperationResult::WifiAuthenticated => {
            // shouldn't happen without real hardware but not a test failure
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Network — state records (no system action)
// ---------------------------------------------------------------------------

#[test]
fn integration_wifi_timeout_is_state_record() {
    require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::WifiConnectionTimeout {
        interface: "wlan0".into(),
        ssid: "Test".into(),
    });
    assert!(
        matches!(result, OperationResult::WifiTimeout),
        "expected WifiTimeout, got {:?}",
        result
    );
}

#[test]
fn integration_wifi_auth_error_is_state_record() {
    require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::WifiAuthError {
        interface: "wlan0".into(),
        ssid: "Test".into(),
    });
    assert!(
        matches!(result, OperationResult::WifiAuthFailed(_)),
        "expected WifiAuthFailed, got {:?}",
        result
    );
}

#[test]
fn integration_abort_is_noop() {
    require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::Abort {
        reason: "test abort".into(),
    });
    assert!(
        matches!(result, OperationResult::Success),
        "Abort should return Success, got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Disk — partition + btrfs on loop devices
// ---------------------------------------------------------------------------

#[test]
fn integration_partition_disk() {
    let devs = loop_devices();
    if devs.is_empty() {
        eprintln!("skipping (no loop devices)");
        return;
    }
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::PartitionDisk {
        device: devs[0].clone(),
    });
    assert!(
        result.is_success(),
        "PartitionDisk failed on {}: {:?}",
        devs[0],
        result
    );
}

#[test]
fn integration_mkfs_btrfs_single() {
    let devs = loop_devices();
    if devs.is_empty() {
        eprintln!("skipping (no loop devices)");
        return;
    }
    let mut exec = SystemdExecutor::new();

    // Partition first
    exec.execute(&Operation::PartitionDisk {
        device: devs[0].clone(),
    });

    let result = exec.execute(&Operation::MkfsBtrfs {
        devices: vec![devs[0].clone()],
    });
    assert!(
        result.is_success(),
        "MkfsBtrfs failed on {}: {:?}",
        devs[0],
        result
    );
}

#[test]
fn integration_btrfs_subvolume() {
    let devs = loop_devices();
    if devs.is_empty() {
        eprintln!("skipping (no loop devices)");
        return;
    }
    let mut exec = SystemdExecutor::new();
    let mount_point = "/tmp/ttyforce-btrfs-test";

    // Partition and format the device
    exec.execute(&Operation::PartitionDisk {
        device: devs[0].clone(),
    });
    exec.execute(&Operation::MkfsBtrfs {
        devices: vec![devs[0].clone()],
    });

    // Mount the partition, create subvolume, unmount
    let part_dev = ttyforce::engine::real_ops::disk::partition_path(&devs[0]);
    std::fs::create_dir_all(mount_point).ok();
    let mount_res = std::process::Command::new("mount")
        .args([&part_dev, mount_point])
        .output();
    let mount_ok = match mount_res {
        Ok(output) => output.status.success(),
        Err(_) => false,
    };
    if !mount_ok {
        eprintln!("skipping subvolume test (mount failed)");
        return;
    }

    let result = exec.execute(&Operation::CreateBtrfsSubvolume {
        mount_point: mount_point.into(),
        name: "@test".into(),
    });
    assert!(
        result.is_success(),
        "CreateBtrfsSubvolume failed: {:?}",
        result
    );

    // Verify subvolume exists
    let check = match std::process::Command::new("btrfs")
        .args(["subvolume", "list", mount_point])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("btrfs list failed: {}", e);
            // Cleanup before returning
            if let Err(ue) = std::process::Command::new("umount").arg(mount_point).output() {
                eprintln!("umount cleanup: {}", ue);
            }
            std::fs::remove_dir(mount_point).ok();
            panic!("btrfs subvolume list command failed: {}", e);
        }
    };
    let output = String::from_utf8_lossy(&check.stdout);
    assert!(output.contains("@test"), "subvolume not found in:\n{}", output);

    // Cleanup (best-effort)
    if let Err(e) = std::process::Command::new("umount").arg(mount_point).output() {
        eprintln!("umount cleanup: {}", e);
    }
    if let Err(e) = std::fs::remove_dir(mount_point) {
        eprintln!("rmdir cleanup: {}", e);
    }
}

#[test]
fn integration_btrfs_raid_setup() {
    let devs = loop_devices();
    if devs.len() < 2 {
        eprintln!("skipping (need ≥2 loop devices for raid)");
        return;
    }
    let mut exec = SystemdExecutor::new();

    // Partition both devices first (raid setup uses partition paths)
    exec.execute(&Operation::PartitionDisk {
        device: devs[0].clone(),
    });
    exec.execute(&Operation::PartitionDisk {
        device: devs[1].clone(),
    });

    let result = exec.execute(&Operation::BtrfsRaidSetup {
        devices: vec![devs[0].clone(), devs[1].clone()],
        raid_level: "raid1".into(),
    });
    assert!(
        result.is_success(),
        "BtrfsRaidSetup failed: {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Disk — partition on nonexistent device (error path)
// ---------------------------------------------------------------------------

#[test]
fn integration_partition_nonexistent_device() {
    require_env!(test_iface()); // just to confirm we're in the container
    let mut exec = SystemdExecutor::new();

    let result = exec.execute(&Operation::PartitionDisk {
        device: "/dev/doesnotexist".into(),
    });
    assert!(
        matches!(result, OperationResult::Error(_)),
        "expected error for nonexistent device, got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// System — reboot and install (should NOT actually reboot in test)
// ---------------------------------------------------------------------------

#[test]
fn integration_install_base_system_placeholder() {
    require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    // InstallBaseSystem succeeds as no-op when no install.sh exists
    let result = exec.execute(&Operation::InstallBaseSystem {
        target: "/tmp/ttyforce-install-test".into(),
    });
    assert!(
        matches!(result, OperationResult::Success),
        "expected success (no-op), got: {:?}",
        result
    );
}

// NOTE: We do NOT test Reboot in integration tests — it would halt the container.

// ---------------------------------------------------------------------------
// Full flow: network ops recorded in order
// ---------------------------------------------------------------------------

#[test]
fn integration_network_operation_sequence() {
    let iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    // Run through the same sequence that bring_ethernet_online uses
    let ops = vec![
        Operation::EnableInterface {
            interface: iface.clone(),
        },
        Operation::CheckLinkAvailability {
            interface: iface.clone(),
        },
        Operation::CheckIpAddress {
            interface: iface.clone(),
        },
    ];

    for op in &ops {
        exec.execute(op);
    }

    let recorded = exec.recorded_operations();
    assert_eq!(recorded.len(), 3);

    // EnableInterface should succeed
    assert!(
        recorded[0].result.is_success(),
        "EnableInterface: {:?}",
        recorded[0].result
    );

    // CheckIpAddress should find our 10.99.99.1
    match &recorded[2].result {
        OperationResult::IpAssigned(ip) => assert_eq!(ip, "10.99.99.1"),
        other => panic!("expected IpAssigned, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// DHCP on dummy (will fail — no DHCP server — but exercises the code path)
// ---------------------------------------------------------------------------

/// This test takes ~30s because the DHCP polling loop waits for an IP that
/// will never arrive (no DHCP server on the dummy interface).
#[test]
#[ignore]
fn integration_configure_dhcp_no_server() {
    let iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    // Remove the static IP so the polling loop has nothing to find
    if let Err(e) = std::process::Command::new("ip")
        .args(["addr", "flush", "dev", &iface])
        .output()
    {
        eprintln!("ip addr flush: {}", e);
    }

    let result = exec.execute(&Operation::ConfigureDhcp {
        interface: iface.clone(),
    });

    // Restore the networkd-managed state (re-applies the static IP from the .network file)
    if let Err(e) = std::process::Command::new("networkctl")
        .args(["reconfigure", &iface])
        .output()
    {
        eprintln!("networkctl reconfigure: {}", e);
    }

    // The DHCP trigger may "succeed" (networkd ReconfigureLink returns ok),
    // but no IP will be assigned, so we expect a timeout error.
    match &result {
        OperationResult::Success => {
            // networkd may "succeed" if the static IP from run-tests.sh is found
        }
        OperationResult::Error(msg) => {
            assert!(
                msg.contains("timeout") || msg.contains("DHCP"),
                "expected timeout/DHCP error, got: {}",
                msg
            );
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

/// Tests the DHCP polling happy path: dummy0 already has a static IP
/// (10.99.99.1/24 configured via systemd-networkd), so the polling loop
/// should find it quickly and return Success.
#[test]
fn integration_configure_dhcp_with_static_ip() {
    let iface = require_env!(test_iface());
    let mut exec = SystemdExecutor::new();

    // Ensure the interface is up and has the static IP
    exec.execute(&Operation::EnableInterface {
        interface: iface.clone(),
    });

    let result = exec.execute(&Operation::ConfigureDhcp {
        interface: iface.clone(),
    });

    // Reconfigure the link via networkd to restore its managed state
    if let Err(e) = std::process::Command::new("networkctl")
        .args(["reconfigure", &iface])
        .output()
    {
        eprintln!("networkctl reconfigure: {}", e);
    }

    // The polling loop should find the existing static IP and return Success
    assert!(
        result.is_success(),
        "expected Success (static IP should be found by poll), got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// InitrdExecutor — network (syscall/sysfs path, IPv4 + IPv6)
//
// These mirror the SystemdExecutor network tests above but drive the
// InitrdExecutor, which uses `ip`/sysfs/`/proc/net/*` directly instead of
// systemd dbus. The dummy0 interface is configured by run-tests.sh with both
// an IPv4 address (10.99.99.1/24) and a global ULA IPv6 address
// (fd00:99::1/64), so the dual-stack code paths can be exercised against a
// real kernel.
// ---------------------------------------------------------------------------

/// Deletes a test-created network interface on drop, so a test that builds a
/// throwaway interface always cleans up — even on panic (Drop runs on unwind).
struct LinkGuard {
    iface: String,
}

impl Drop for LinkGuard {
    fn drop(&mut self) {
        if let Err(e) = std::process::Command::new("ip")
            .args(["link", "del", &self.iface])
            .output()
        {
            eprintln!("failed to delete {}: {}", self.iface, e);
        }
    }
}

#[test]
fn integration_initrd_check_ip_address_prefers_ipv4() {
    let iface = require_env!(test_iface());
    let mut exec = InitrdExecutor::new();

    let result = exec.execute(&Operation::CheckIpAddress {
        interface: iface.clone(),
    });

    // dummy0 has both families; IPv4 is preferred when present.
    match &result {
        OperationResult::IpAssigned(ip) => {
            assert_eq!(ip, "10.99.99.1", "expected IPv4 to be preferred, got {}", ip);
        }
        other => panic!("expected IpAssigned, got {:?}", other),
    }
}

#[test]
fn integration_initrd_detects_global_ipv6() {
    require_env!(test_iface());

    // Directly exercise the real `ip -6` query + parser against the kernel.
    let v6 = ttyforce::engine::initrd_ops::syscall::get_interface_ipv6("dummy0");
    assert_eq!(
        v6,
        Some("fd00:99::1".parse().unwrap()),
        "expected the configured global ULA address, got {:?}",
        v6
    );
}

#[test]
fn integration_initrd_check_ip_address_ipv6_only() {
    require_env!(test_iface());

    // Build a throwaway IPv6-only dummy interface. It has no matching networkd
    // .network unit, so networkd leaves it unmanaged — it won't flush or re-add
    // addresses — giving a stable IPv6-only link without disturbing dummy0.
    let iface = "ttyf6";
    let _guard = LinkGuard {
        iface: iface.into(),
    };

    let setup: [&[&str]; 3] = [
        &["link", "add", iface, "type", "dummy"],
        &["link", "set", iface, "up"],
        &["-6", "addr", "add", "fd00:6::1/64", "dev", iface],
    ];
    for args in setup {
        match std::process::Command::new("ip").args(args).output() {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                eprintln!(
                    "skipping (ip {:?} failed: {})",
                    args,
                    String::from_utf8_lossy(&o.stderr).trim()
                );
                return;
            }
            Err(e) => {
                eprintln!("skipping (could not run ip: {})", e);
                return;
            }
        }
    }

    let mut exec = InitrdExecutor::new();
    let result = exec.execute(&Operation::CheckIpAddress {
        interface: iface.into(),
    });

    // No IPv4 on this link, so the check must report its global IPv6 address.
    match &result {
        OperationResult::IpAssigned(ip) => {
            assert_eq!(ip, "fd00:6::1", "expected IPv6-only address, got {}", ip);
        }
        other => panic!("expected IpAssigned (IPv6), got {:?}", other),
    }
}

#[test]
fn integration_initrd_check_upstream_router_no_route() {
    let iface = require_env!(test_iface());
    let mut exec = InitrdExecutor::new();

    // dummy0 has neither an IPv4 nor an IPv6 default route, so both the
    // /proc/net/route and /proc/net/ipv6_route scans must come up empty.
    let result = exec.execute(&Operation::CheckUpstreamRouter {
        interface: iface.clone(),
    });
    assert!(
        matches!(result, OperationResult::NoRouter),
        "expected NoRouter, got {:?}",
        result
    );
}

#[test]
fn integration_initrd_check_dns_resolution() {
    let iface = require_env!(test_iface());
    let mut exec = InitrdExecutor::new();

    // Exercises the real UDP DNS path (nameserver-socket binding + query).
    // A minimal container may have no working upstream resolver, so a failure
    // is acceptable — what matters is that the code path runs without panicking.
    let result = exec.execute(&Operation::CheckDnsResolution {
        interface: iface.clone(),
        hostname: "example.com".into(),
    });
    match &result {
        OperationResult::DnsResolved(ip) => {
            assert!(!ip.is_empty(), "resolved to empty string");
        }
        OperationResult::DnsFailed(msg) => {
            eprintln!("DNS resolution failed (acceptable in minimal container): {}", msg);
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// DNS resolver fallback (resolve_via)
//
// Exercises the real UDP socket path against a local mock resolver. These are
// hermetic (loopback only) and need no container env, so they run under a
// plain `cargo test` as well as in the integration container. They cover the
// behavior added for filtered public DNS: try each nameserver in order and
// fall through to the next when one is dead/filtered (e.g. a blocked 1.1.1.1),
// which is what lets the DHCP-offered resolver win.
// ---------------------------------------------------------------------------

/// Spawn a one-shot mock DNS server on loopback that answers the next query
/// with an A record for `ip`. Returns the address it bound to.
fn spawn_mock_dns(ip: [u8; 4]) -> std::net::SocketAddr {
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind mock dns");
    let addr = sock.local_addr().expect("mock dns local_addr");
    std::thread::spawn(move || {
        let mut buf = [0u8; 512];
        if let Ok((n, peer)) = sock.recv_from(&mut buf) {
            // Echo the request, then flip it into a response with one A answer:
            //   flags -> response + recursion-available, ANCOUNT -> 1,
            //   answer -> name compression pointer to the question (0xc00c),
            //            TYPE=A, CLASS=IN, TTL=60, RDLENGTH=4, the IP.
            let mut resp = buf[..n].to_vec();
            resp[2] = 0x81;
            resp[3] = 0x80;
            resp[6] = 0x00;
            resp[7] = 0x01;
            resp.extend_from_slice(&[
                0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x04,
            ]);
            resp.extend_from_slice(&ip);
            let _ = sock.send_to(&resp, peer);
        }
    });
    addr
}

/// A loopback address with no listener: sends go nowhere and recv hits the
/// read timeout — the stand-in for a filtered/dead resolver like 1.1.1.1 here.
fn dead_dns() -> std::net::SocketAddr {
    "127.0.0.1:1".parse().expect("parse dead dns addr")
}

#[test]
fn integration_resolve_via_falls_back_past_dead_server() {
    // First candidate is dead (filtered public DNS); the second is the working
    // DHCP-style resolver. resolve_via must skip the dead one and return the
    // working server's answer.
    let good = spawn_mock_dns([93, 184, 216, 34]);
    let candidates = [dead_dns(), good];
    let result = ttyforce::engine::initrd_ops::network::resolve_via(
        &candidates,
        "example.com",
        std::time::Duration::from_millis(300),
    );
    assert_eq!(result, Ok("93.184.216.34".to_string()));
}

#[test]
fn integration_resolve_via_uses_first_working_server() {
    // When the first candidate answers, it is used directly.
    let good = spawn_mock_dns([10, 1, 2, 3]);
    let candidates = [good, dead_dns()];
    let result = ttyforce::engine::initrd_ops::network::resolve_via(
        &candidates,
        "example.com",
        std::time::Duration::from_millis(300),
    );
    assert_eq!(result, Ok("10.1.2.3".to_string()));
}

#[test]
fn integration_resolve_via_errors_when_all_dead() {
    // Every candidate filtered/dead -> an error (the last failure), not a hang.
    let candidates = [dead_dns(), dead_dns()];
    let result = ttyforce::engine::initrd_ops::network::resolve_via(
        &candidates,
        "example.com",
        std::time::Duration::from_millis(200),
    );
    assert!(result.is_err(), "expected error when all servers are dead, got {:?}", result);
}

// ---------------------------------------------------------------------------
// Internet routability prefers the local resolver (routability_local_first)
//
// The initrd connectivity check must NOT ping 1.1.1.1 first. On networks that
// filter outbound traffic to public resolvers, resolving an external name
// through the local resolver (DHCP-offered / gateway forwarder) is the only
// path that proves we're online — so it is tried first, and the static public
// free-server probe is only the fallback. These exercise the real UDP path
// against a loopback mock resolver and are hermetic.
// ---------------------------------------------------------------------------

#[test]
fn integration_routability_resolves_via_local_resolver_first() {
    // A working local resolver answers, so routability succeeds WITHOUT ever
    // touching the public free-server fallback.
    let local = spawn_mock_dns([93, 184, 216, 34]);
    let fallback_called = std::sync::atomic::AtomicBool::new(false);
    let result = ttyforce::engine::initrd_ops::network::routability_local_first(
        &[local],
        "example.com",
        std::time::Duration::from_millis(300),
        || {
            fallback_called.store(true, std::sync::atomic::Ordering::SeqCst);
            OperationResult::NoInternet
        },
    );
    assert!(
        matches!(result, OperationResult::InternetReachable),
        "expected InternetReachable via local resolver, got {:?}",
        result
    );
    assert!(
        !fallback_called.load(std::sync::atomic::Ordering::SeqCst),
        "public free-server fallback must NOT run when the local resolver answers"
    );
}

#[test]
fn integration_routability_falls_back_to_free_servers_when_local_dead() {
    // The local resolver is filtered/dead, so routability falls back to the
    // static public free-server probe and uses its result.
    let result = ttyforce::engine::initrd_ops::network::routability_local_first(
        &[dead_dns()],
        "example.com",
        std::time::Duration::from_millis(200),
        || OperationResult::InternetReachable,
    );
    assert!(
        matches!(result, OperationResult::InternetReachable),
        "expected fallback to public free servers to succeed, got {:?}",
        result
    );
}

#[test]
fn integration_routability_falls_back_when_no_local_resolver() {
    // With no local resolver at all, the fallback probe is consulted directly.
    let fallback_called = std::sync::atomic::AtomicBool::new(false);
    let result = ttyforce::engine::initrd_ops::network::routability_local_first(
        &[],
        "example.com",
        std::time::Duration::from_millis(200),
        || {
            fallback_called.store(true, std::sync::atomic::Ordering::SeqCst);
            OperationResult::NoInternet
        },
    );
    assert!(
        fallback_called.load(std::sync::atomic::Ordering::SeqCst),
        "fallback must run when there is no local resolver"
    );
    assert!(matches!(result, OperationResult::NoInternet));
}

// ---------------------------------------------------------------------------
// dhcpcd directory population (prepare_dhcpcd_dirs_in)
//
// In the initrd, ttyforce must create the run/lease directories dhcpcd needs
// before launching it — otherwise dhcpcd never persists a lease and the
// DHCP-offered DNS can't be read back. These tests create the dirs under a
// throwaway temp root (never the real /run or /var) and assert they exist.
// ---------------------------------------------------------------------------

/// Unique temp dir for a dhcpcd-dirs test, isolated per-test by `tag`.
fn dhcpcd_dirs_temp_root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ttyforce-dhcpcd-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn integration_prepare_dhcpcd_dirs_creates_run_and_lease_dirs() {
    let root = dhcpcd_dirs_temp_root("create");
    let made = ttyforce::engine::initrd_ops::network::prepare_dhcpcd_dirs_in(&root)
        .expect("prepare dhcpcd dirs");

    // The lease DB (for --dumplease), the run dir (control socket), and the
    // resolv-fragment dir must all exist as directories under the root.
    for sub in ["run/dhcpcd", "run/dhcpcd/resolv.conf", "var/db/dhcpcd"] {
        let p = root.join(sub);
        assert!(p.is_dir(), "expected {} to be a directory", p.display());
    }
    // Every created path stays under the root — no absolute-join escape.
    assert_eq!(made.len(), 3);
    assert!(made.iter().all(|p| p.starts_with(&root)), "paths escaped root: {:?}", made);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn integration_prepare_dhcpcd_dirs_is_idempotent() {
    let root = dhcpcd_dirs_temp_root("idempotent");
    // Running twice (e.g. a DHCP retry) must not fail on already-existing dirs.
    ttyforce::engine::initrd_ops::network::prepare_dhcpcd_dirs_in(&root)
        .expect("first prepare");
    ttyforce::engine::initrd_ops::network::prepare_dhcpcd_dirs_in(&root)
        .expect("second prepare is idempotent");
    assert!(root.join("var/db/dhcpcd").is_dir());

    let _ = std::fs::remove_dir_all(&root);
}
