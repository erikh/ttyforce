use std::net::{Ipv4Addr, TcpStream};
use std::time::Duration;

/// System information gathered by probing the local machine.
pub struct SystemInfo {
    pub hostname: String,
    pub kernel_version: String,
    pub architecture: String,
    pub cpu_model: String,
    pub cpu_cores: usize,
    pub load_average: f64,
    pub mem_total_mb: u64,
    pub mem_used_mb: u64,
    pub disk_total_gb: f64,
    pub disk_used_gb: f64,
    pub disk_available_gb: f64,
    pub network_online: bool,
    pub ip_address: Option<String>,
    pub default_interface: Option<String>,
    pub mdns_url: String,
    pub town_os_version: Option<String>,
}

impl SystemInfo {
    /// Probe the live system for all info.
    pub fn probe(mount_point: &str) -> Self {
        let utsname = nix::sys::utsname::uname()
            .expect("uname() should not fail on Linux");
        let hostname = utsname.nodename().to_string_lossy().to_string();
        let kernel_version = utsname.release().to_string_lossy().to_string();
        let architecture = utsname.machine().to_string_lossy().to_string();

        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let (cpu_model, cpu_cores) = parse_cpuinfo(&cpuinfo);

        let loadavg = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        let load_average = parse_loadavg(&loadavg);

        let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let (mem_total_mb, mem_used_mb) = parse_meminfo(&meminfo);

        let (disk_total_gb, disk_used_gb, disk_available_gb) = probe_disk_usage(mount_point);

        let route_content = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
        let (default_interface, _gateway) = parse_proc_route(&route_content)
            .unwrap_or_default();

        let ip_address = if !default_interface.is_empty() {
            crate::engine::initrd_ops::syscall::get_interface_ipv4(&default_interface)
                .map(|ip| ip.to_string())
        } else {
            None
        };

        let network_online = check_online();

        let mdns_url = format!("{}.local", hostname);

        let town_os_version = read_town_os_version(mount_point);

        let default_interface = if default_interface.is_empty() {
            None
        } else {
            Some(default_interface)
        };

        SystemInfo {
            hostname,
            kernel_version,
            architecture,
            cpu_model,
            cpu_cores,
            load_average,
            mem_total_mb,
            mem_used_mb,
            disk_total_gb,
            disk_used_gb,
            disk_available_gb,
            network_online,
            ip_address,
            default_interface,
            mdns_url,
            town_os_version,
        }
    }
}

/// Parse /proc/cpuinfo for CPU model name and core count.
pub fn parse_cpuinfo(content: &str) -> (String, usize) {
    let mut model = String::from("Unknown");
    let mut cores: usize = 0;

    for line in content.lines() {
        if line.starts_with("model name") {
            if let Some((_, val)) = line.split_once(':') {
                model = val.trim().to_string();
            }
        }
        if line.starts_with("processor") {
            cores += 1;
        }
    }

    if cores == 0 {
        cores = 1;
    }
    (model, cores)
}

/// Parse /proc/meminfo for total and used memory in MB.
/// Used = MemTotal - MemAvailable.
pub fn parse_meminfo(content: &str) -> (u64, u64) {
    let mut total_kb: u64 = 0;
    let mut available_kb: u64 = 0;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_meminfo_value(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available_kb = parse_meminfo_value(rest);
        }
    }

    let total_mb = total_kb / 1024;
    let used_mb = total_kb.saturating_sub(available_kb) / 1024;
    (total_mb, used_mb)
}

fn parse_meminfo_value(s: &str) -> u64 {
    s.split_whitespace()
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Parse /proc/loadavg for the 1-minute load average.
pub fn parse_loadavg(content: &str) -> f64 {
    content
        .split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// Probe disk usage via statvfs. Returns (total_gb, used_gb, available_gb).
pub fn probe_disk_usage(path: &str) -> (f64, f64, f64) {
    match nix::sys::statvfs::statvfs(path) {
        Ok(stat) => {
            let block_size = stat.fragment_size() as f64;
            let total = (stat.blocks() as f64 * block_size) / (1024.0 * 1024.0 * 1024.0);
            let available = (stat.blocks_available() as f64 * block_size)
                / (1024.0 * 1024.0 * 1024.0);
            let used = total - available;
            (total, used, available)
        }
        Err(_) => (0.0, 0.0, 0.0),
    }
}

/// Parse /proc/net/route for the default route interface and gateway.
/// Returns Some((interface, gateway_ip)) or None.
pub fn parse_proc_route(content: &str) -> Option<(String, String)> {
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 3 {
            continue;
        }
        // Default route has destination 00000000
        if fields[1] == "00000000" {
            let iface = fields[0].to_string();
            let gw_hex = fields[2];
            let gateway = parse_hex_ip(gw_hex);
            return Some((iface, gateway));
        }
    }
    None
}

fn parse_hex_ip(hex: &str) -> String {
    if let Ok(val) = u32::from_str_radix(hex, 16) {
        let ip = Ipv4Addr::from(val.to_be());
        ip.to_string()
    } else {
        "0.0.0.0".to_string()
    }
}

/// Check if the machine is online by attempting a TCP connect to 1.1.1.1:53.
pub fn check_online() -> bool {
    TcpStream::connect_timeout(
        &"1.1.1.1:53".parse().unwrap(),
        Duration::from_secs(2),
    )
    .is_ok()
}

/// Read Town OS version from a version file.
pub fn read_town_os_version(mount_point: &str) -> Option<String> {
    let paths = [
        format!("{}/version", mount_point),
        "/etc/town-os-version".to_string(),
    ];
    for path in &paths {
        if let Ok(content) = std::fs::read_to_string(path) {
            let version = content.trim().to_string();
            if !version.is_empty() {
                return Some(version);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cpuinfo() {
        let content = "\
processor\t: 0
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7-9700K CPU @ 3.60GHz
cpu MHz\t\t: 3600.000

processor\t: 1
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7-9700K CPU @ 3.60GHz
cpu MHz\t\t: 3600.000

processor\t: 2
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7-9700K CPU @ 3.60GHz

processor\t: 3
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7-9700K CPU @ 3.60GHz
";
        let (model, cores) = parse_cpuinfo(content);
        assert_eq!(model, "Intel(R) Core(TM) i7-9700K CPU @ 3.60GHz");
        assert_eq!(cores, 4);
    }

    #[test]
    fn test_parse_cpuinfo_no_model() {
        let content = "processor\t: 0\nvendor_id\t: GenuineIntel\n";
        let (model, cores) = parse_cpuinfo(content);
        assert_eq!(model, "Unknown");
        assert_eq!(cores, 1);
    }

    #[test]
    fn test_parse_cpuinfo_empty() {
        let (model, cores) = parse_cpuinfo("");
        assert_eq!(model, "Unknown");
        assert_eq!(cores, 1);
    }

    #[test]
    fn test_parse_meminfo() {
        let content = "\
MemTotal:       16384000 kB
MemFree:         2048000 kB
MemAvailable:    8192000 kB
Buffers:          512000 kB
Cached:          4096000 kB
";
        let (total, used) = parse_meminfo(content);
        assert_eq!(total, 16000); // 16384000 / 1024
        assert_eq!(used, 8000); // (16384000 - 8192000) / 1024
    }

    #[test]
    fn test_parse_meminfo_empty() {
        let (total, used) = parse_meminfo("");
        assert_eq!(total, 0);
        assert_eq!(used, 0);
    }

    #[test]
    fn test_parse_loadavg() {
        let content = "0.45 0.30 0.25 1/234 5678\n";
        assert!((parse_loadavg(content) - 0.45).abs() < 0.001);
    }

    #[test]
    fn test_parse_loadavg_empty() {
        assert!((parse_loadavg("") - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_proc_route() {
        let content = "\
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
eth0\t0000A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0
";
        let result = parse_proc_route(content);
        assert!(result.is_some());
        let (iface, gw) = result.unwrap();
        assert_eq!(iface, "eth0");
        // 0101A8C0 in little-endian = 192.168.1.1
        assert_eq!(gw, "192.168.1.1");
    }

    #[test]
    fn test_parse_proc_route_no_default() {
        let content = "\
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT
eth0\t0000A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0
";
        assert!(parse_proc_route(content).is_none());
    }

    #[test]
    fn test_parse_proc_route_empty() {
        assert!(parse_proc_route("").is_none());
    }

    #[test]
    fn test_mdns_url() {
        assert_eq!(format!("{}.local", "mybox"), "mybox.local");
    }

    #[test]
    fn test_read_town_os_version_missing() {
        assert!(read_town_os_version("/nonexistent/path/xyz").is_none());
    }

    #[test]
    fn test_parse_hex_ip() {
        // 0101A8C0 in /proc/net/route is little-endian: C0.A8.01.01 = 192.168.1.1
        assert_eq!(parse_hex_ip("0101A8C0"), "192.168.1.1");
    }

    #[test]
    fn test_parse_hex_ip_zeros() {
        assert_eq!(parse_hex_ip("00000000"), "0.0.0.0");
    }

    #[test]
    fn test_disk_usage_nonexistent() {
        let (total, used, avail) = probe_disk_usage("/nonexistent_mount_xyz");
        assert!((total - 0.0).abs() < 0.001);
        assert!((used - 0.0).abs() < 0.001);
        assert!((avail - 0.0).abs() < 0.001);
    }
}
