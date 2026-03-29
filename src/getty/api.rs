use chrono::{DateTime, FixedOffset, NaiveDateTime};
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Status information for a single service/unit.
#[derive(Debug)]
pub struct ServiceInfo {
    pub name: String,
    pub active_state: String,
    pub description: String,
}

/// Raw JSON representation of a service/unit from the API.
/// Supports both PascalCase (systemd style) and snake_case field names.
#[derive(Deserialize)]
struct RawUnit {
    #[serde(alias = "name")]
    #[serde(default)]
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(alias = "active_state")]
    #[serde(default)]
    #[serde(rename = "ActiveState")]
    active_state: Option<String>,
    #[serde(alias = "description")]
    #[serde(default)]
    #[serde(rename = "Description")]
    description: Option<String>,
}

/// Paginated wrapper for unit responses.
#[derive(Deserialize)]
struct PaginatedUnits {
    entries: Vec<RawUnit>,
}

/// Raw JSON representation of an audit log entry.
#[derive(Deserialize)]
struct RawAuditEntry {
    #[serde(default)]
    action: String,
    #[serde(default)]
    detail: String,
    #[serde(default = "default_true")]
    success: bool,
    #[serde(default)]
    error: String,
    #[serde(default)]
    created_at: String,
}

fn default_true() -> bool {
    true
}

/// Paginated wrapper for audit log responses.
#[derive(Deserialize)]
struct PaginatedAuditLog {
    entries: Vec<RawAuditEntry>,
}

/// Minimal HTTP client for the Town OS API at localhost:5309.
pub struct TownApiClient {
    token: Option<String>,
}

impl TownApiClient {
    pub fn new(token: Option<String>) -> Self {
        Self { token }
    }

    /// Resolve the API token from environment or config file.
    pub fn from_env(etc_prefix: Option<&str>) -> Self {
        let token = std::env::var("TTYFORCE_API_TOKEN").ok().or_else(|| {
            let paths = [
                etc_prefix
                    .map(|p| format!("{}/ttyforce/api-token", p)),
                Some("/etc/ttyforce/api-token".to_string()),
            ];
            for path in paths.into_iter().flatten() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let t = content.trim().to_string();
                    if !t.is_empty() {
                        return Some(t);
                    }
                }
            }
            None
        });
        Self::new(token)
    }

    /// Fetch system services only (town-os-system--*).
    /// Used for startup readiness checks.
    pub fn fetch_system_services(&self) -> Result<Vec<ServiceInfo>, String> {
        let body = self.http_get("/system-services")?;
        parse_units_json(&body)
    }

    /// Fetch all services: system services + package units.
    /// Used for the status panel display.
    pub fn fetch_all_services(&self) -> Result<Vec<ServiceInfo>, String> {
        let mut all = Vec::new();

        if let Ok(body) = self.http_get("/system-services") {
            if let Ok(services) = parse_units_json(&body) {
                all.extend(services);
            }
        }

        if let Ok(body) = self.http_get("/systemd/units?limit=100") {
            if let Ok(services) = parse_units_json(&body) {
                all.extend(services);
            }
        }

        Ok(all)
    }

    /// Fetch audit log entries from the Town OS API.
    /// Returns a list of log lines (most recent last).
    pub fn fetch_audit_log(&self) -> Result<Vec<String>, String> {
        let body = self.http_post("/audit/log", r#"{"limit":200}"#)?;
        parse_audit_log_json(&body)
    }

    /// Perform an HTTP GET request to the Town OS API.
    fn http_get(&self, path: &str) -> Result<String, String> {
        let addr = "127.0.0.1:5309"
            .parse()
            .map_err(|e| format!("bad address: {}", e))?;
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .map_err(|e| format!("API unavailable: {}", e))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set timeout: {}", e))?;

        let mut request = format!(
            "GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",
            path
        );
        if let Some(ref token) = self.token {
            request.push_str(&format!("Authorization: Bearer {}\r\n", token));
        }
        request.push_str("\r\n");

        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("write failed: {}", e))?;

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(|e| format!("read failed: {}", e))?;

        // Find the body after the HTTP headers (\r\n\r\n separator)
        let body_start = response
            .find("\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(0);
        let body = &response[body_start..];

        // Check for HTTP error status
        if let Some(status_line) = response.lines().next() {
            if let Some(code_str) = status_line.split_whitespace().nth(1) {
                if let Ok(code) = code_str.parse::<u16>() {
                    if code >= 400 {
                        return Err(format!("API returned HTTP {}", code));
                    }
                }
            }
        }

        Ok(body.to_string())
    }

    /// Perform an HTTP POST request to the Town OS API.
    fn http_post(&self, path: &str, json_body: &str) -> Result<String, String> {
        let addr = "127.0.0.1:5309"
            .parse()
            .map_err(|e| format!("bad address: {}", e))?;
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .map_err(|e| format!("API unavailable: {}", e))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("set timeout: {}", e))?;

        let mut request = format!(
            "POST {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            path,
            json_body.len()
        );
        if let Some(ref token) = self.token {
            request.push_str(&format!("Authorization: Bearer {}\r\n", token));
        }
        request.push_str("\r\n");
        request.push_str(json_body);

        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("write failed: {}", e))?;

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(|e| format!("read failed: {}", e))?;

        let body_start = response
            .find("\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(0);
        let body = &response[body_start..];

        if let Some(status_line) = response.lines().next() {
            if let Some(code_str) = status_line.split_whitespace().nth(1) {
                if let Ok(code) = code_str.parse::<u16>() {
                    if code >= 400 {
                        return Err(format!("API returned HTTP {}", code));
                    }
                }
            }
        }

        Ok(body.to_string())
    }
}

/// Parse the JSON response from the /systemd/units endpoint into ServiceInfo structs.
/// Handles both paginated format `{ "entries": [...] }` and bare array `[...]`.
pub fn parse_units_json(body: &str) -> Result<Vec<ServiceInfo>, String> {
    let units: Vec<RawUnit> = if let Ok(paginated) =
        serde_json::from_str::<PaginatedUnits>(body)
    {
        paginated.entries
    } else {
        serde_json::from_str::<Vec<RawUnit>>(body).map_err(|e| {
            if serde_json::from_str::<serde_json::Value>(body).is_ok() {
                "expected entries array or JSON array".to_string()
            } else {
                format!("JSON parse error: {}", e)
            }
        })?
    };

    let services = units
        .into_iter()
        .filter_map(|u| {
            let name = u.name.unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            Some(ServiceInfo {
                name,
                active_state: u.active_state.unwrap_or_else(|| "unknown".to_string()),
                description: u.description.unwrap_or_default(),
            })
        })
        .collect();

    Ok(services)
}

/// Format an ISO 8601 timestamp string to "YYYY-MM-DD HH:MM:SS" for display.
fn format_timestamp(created_at: &str) -> String {
    if created_at.is_empty() {
        return String::new();
    }
    // Try parsing as DateTime with timezone offset (e.g. 2024-01-15T12:00:00+05:00)
    if let Ok(dt) = DateTime::<FixedOffset>::parse_from_rfc3339(created_at) {
        return dt.format("%Y-%m-%d %H:%M:%S").to_string();
    }
    // Try parsing as naive datetime (e.g. 2024-01-15T12:00:00)
    if let Ok(dt) = NaiveDateTime::parse_from_str(created_at, "%Y-%m-%dT%H:%M:%S") {
        return dt.format("%Y-%m-%d %H:%M:%S").to_string();
    }
    // Try with fractional seconds (e.g. 2024-01-15T12:00:00.123)
    if let Ok(dt) = NaiveDateTime::parse_from_str(created_at, "%Y-%m-%dT%H:%M:%S%.f") {
        return dt.format("%Y-%m-%d %H:%M:%S").to_string();
    }
    created_at.to_string()
}

/// Parse the JSON response from the POST /audit/log endpoint into log lines.
/// Response format: `{ "entries": [...], "has_more": bool, ... }`
/// Each entry has: action, detail, success, error, created_at, account.
pub fn parse_audit_log_json(body: &str) -> Result<Vec<String>, String> {
    let entries: Vec<RawAuditEntry> = if let Ok(paginated) =
        serde_json::from_str::<PaginatedAuditLog>(body)
    {
        paginated.entries
    } else {
        serde_json::from_str::<Vec<RawAuditEntry>>(body).map_err(|e| {
            if serde_json::from_str::<serde_json::Value>(body).is_ok() {
                "expected entries array or JSON array".to_string()
            } else {
                format!("JSON parse error: {}", e)
            }
        })?
    };

    let lines = entries
        .into_iter()
        .filter(|e| !e.action.is_empty())
        .map(|e| {
            let ts = format_timestamp(&e.created_at);

            let mut line = if ts.is_empty() {
                e.action.clone()
            } else {
                format!("{} {}", ts, e.action)
            };

            if !e.detail.is_empty() {
                line.push_str(&format!(": {}", e.detail));
            }

            if !e.success && !e.error.is_empty() {
                line.push_str(&format!(" [ERROR: {}]", e.error));
            } else if !e.success {
                line.push_str(" [FAILED]");
            }

            line
        })
        .collect();

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_units_json_basic() -> Result<(), String> {
        let json = r#"[
            {"Name": "caddy.service", "ActiveState": "active", "Description": "Caddy web server"},
            {"Name": "forgejo.service", "ActiveState": "inactive", "Description": "Forgejo git server"}
        ]"#;
        let services = parse_units_json(json)?;
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].name, "caddy.service");
        assert_eq!(services[0].active_state, "active");
        assert_eq!(services[0].description, "Caddy web server");
        assert_eq!(services[1].name, "forgejo.service");
        assert_eq!(services[1].active_state, "inactive");
        Ok(())
    }

    #[test]
    fn test_parse_units_json_empty_array() -> Result<(), String> {
        let services = parse_units_json("[]")?;
        assert!(services.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_units_json_malformed() {
        let result = parse_units_json("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_units_json_not_array() {
        let result = parse_units_json(r#"{"key": "value"}"#);
        assert!(result.is_err());
        if let Err(msg) = result {
            assert!(msg.contains("expected entries array"));
        }
    }

    #[test]
    fn test_parse_units_json_paginated() -> Result<(), String> {
        let json = r#"{
            "entries": [
                {"Name": "caddy.service", "ActiveState": "active", "Description": "Caddy"},
                {"Name": "forgejo.service", "ActiveState": "activating", "Description": "Forgejo"}
            ],
            "has_more": false,
            "total_pages": 1,
            "total_count": 2
        }"#;
        let services = parse_units_json(json)?;
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].name, "caddy.service");
        assert_eq!(services[0].active_state, "active");
        assert_eq!(services[1].active_state, "activating");
        Ok(())
    }

    #[test]
    fn test_parse_units_json_paginated_empty_entries() -> Result<(), String> {
        let json = r#"{"entries": [], "has_more": false, "total_count": 0}"#;
        let services = parse_units_json(json)?;
        assert!(services.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_units_json_missing_fields() -> Result<(), String> {
        let json = r#"[{"Name": "test.service"}]"#;
        let services = parse_units_json(json)?;
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].active_state, "unknown");
        assert_eq!(services[0].description, "");
        Ok(())
    }

    #[test]
    fn test_parse_units_json_lowercase_fields() -> Result<(), String> {
        let json = r#"[{"name": "test.service", "active_state": "failed", "description": "A test"}]"#;
        let services = parse_units_json(json)?;
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "test.service");
        assert_eq!(services[0].active_state, "failed");
        Ok(())
    }

    #[test]
    fn test_parse_units_json_skips_empty_name() -> Result<(), String> {
        let json = r#"[{"Name": "", "ActiveState": "active"}, {"Name": "real.service", "ActiveState": "active"}]"#;
        let services = parse_units_json(json)?;
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "real.service");
        Ok(())
    }

    #[test]
    fn test_api_client_new() {
        let client = TownApiClient::new(Some("test-token".to_string()));
        assert_eq!(client.token.as_deref(), Some("test-token"));
    }

    #[test]
    fn test_api_client_no_token() {
        let client = TownApiClient::new(None);
        assert!(client.token.is_none());
    }

    #[test]
    fn test_parse_audit_log_basic() -> Result<(), String> {
        let json = r#"{"entries": [
            {"action": "Install package", "detail": "caddy", "success": true, "error": "", "created_at": "2024-01-15T12:00:00Z", "account": "admin"},
            {"action": "Create account", "detail": "user1", "success": true, "error": "", "created_at": "2024-01-15T12:01:00.123Z", "account": "admin"}
        ]}"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "2024-01-15 12:00:00 Install package: caddy");
        assert_eq!(lines[1], "2024-01-15 12:01:00 Create account: user1");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_no_timestamp() -> Result<(), String> {
        let json = r#"[{"action": "Something happened", "success": true}]"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "Something happened");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_failure_with_error() -> Result<(), String> {
        let json = r#"[{"action": "Install package", "detail": "bad-pkg", "success": false, "error": "not found", "created_at": "2024-01-15T12:00:00Z"}]"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            "2024-01-15 12:00:00 Install package: bad-pkg [ERROR: not found]"
        );
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_failure_no_error_msg() -> Result<(), String> {
        let json = r#"[{"action": "Install package", "success": false, "error": ""}]"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "Install package [FAILED]");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_empty_array() -> Result<(), String> {
        let lines = parse_audit_log_json("[]")?;
        assert!(lines.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_skips_empty_action() -> Result<(), String> {
        let json = r#"[{"action": "", "success": true}, {"action": "Real entry", "success": true}]"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "Real entry");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_paginated() -> Result<(), String> {
        let json = r#"{"entries": [{"action": "entry one", "success": true}, {"action": "entry two", "success": true}], "has_more": false, "total_count": 2}"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "entry one");
        assert_eq!(lines[1], "entry two");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_malformed() {
        let result = parse_audit_log_json("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_audit_log_not_array() {
        let result = parse_audit_log_json(r#"{"key": "value"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_audit_log_timestamp_with_timezone_offset() -> Result<(), String> {
        let json = r#"[{"action": "test", "created_at": "2024-01-15T12:00:00+05:00", "success": true}]"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines[0], "2024-01-15 12:00:00 test");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_timestamp_with_negative_timezone_offset() -> Result<(), String> {
        let json = r#"[{"action": "test", "created_at": "2024-01-15T12:00:00-05:00", "success": true}]"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines[0], "2024-01-15 12:00:00 test");
        Ok(())
    }
}
