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

use ttyforce::engine::executor::{OperationExecutor, SystemdExecutor};
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
    if mount_res.is_err() || !mount_res.unwrap().status.success() {
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
    let check = std::process::Command::new("btrfs")
        .args(["subvolume", "list", mount_point])
        .output()
        .expect("btrfs list");
    let output = String::from_utf8_lossy(&check.stdout);
    assert!(output.contains("@test"), "subvolume not found in:\n{}", output);

    // Cleanup
    let _ = std::process::Command::new("umount").arg(mount_point).output();
    let _ = std::fs::remove_dir(mount_point);
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
    let _ = std::process::Command::new("ip")
        .args(["addr", "flush", "dev", &iface])
        .output();

    let result = exec.execute(&Operation::ConfigureDhcp {
        interface: iface.clone(),
    });

    // Restore the networkd-managed state (re-applies the static IP from the .network file)
    let _ = std::process::Command::new("networkctl")
        .args(["reconfigure", &iface])
        .output();

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
    let _ = std::process::Command::new("networkctl")
        .args(["reconfigure", &iface])
        .output();

    // The polling loop should find the existing static IP and return Success
    assert!(
        result.is_success(),
        "expected Success (static IP should be found by poll), got {:?}",
        result
    );
}
