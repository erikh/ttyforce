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
        let body = self.http_get("/audit-log?limit=200")?;
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
}

/// Parse the JSON response from the /systemd/units endpoint into ServiceInfo structs.
/// Handles both paginated format `{ "entries": [...] }` and bare array `[...]`.
pub fn parse_units_json(body: &str) -> Result<Vec<ServiceInfo>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("JSON parse error: {}", e))?;

    // Try paginated format first: { "entries": [...] }
    let arr = value
        .get("entries")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array())
        .ok_or_else(|| "expected entries array or JSON array".to_string())?;

    let mut services = Vec::new();
    for item in arr {
        let name = item
            .get("Name")
            .or_else(|| item.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let active_state = item
            .get("ActiveState")
            .or_else(|| item.get("active_state"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let description = item
            .get("Description")
            .or_else(|| item.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !name.is_empty() {
            services.push(ServiceInfo {
                name,
                active_state,
                description,
            });
        }
    }

    Ok(services)
}

/// Parse the JSON response from the /audit-log endpoint into log lines.
/// Handles both paginated format `{ "entries": [...] }` and bare array `[...]`.
/// Each entry should have a "message" (or "msg") field, and optionally a "timestamp" field.
pub fn parse_audit_log_json(body: &str) -> Result<Vec<String>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("JSON parse error: {}", e))?;

    let arr = value
        .get("entries")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array())
        .ok_or_else(|| "expected entries array or JSON array".to_string())?;

    let mut lines = Vec::new();
    for item in arr {
        let msg = item
            .get("message")
            .or_else(|| item.get("msg"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if msg.is_empty() {
            continue;
        }

        let timestamp = item
            .get("timestamp")
            .or_else(|| item.get("time"))
            .and_then(|v| v.as_str());

        let line = match timestamp {
            Some(ts) => format!("{} {}", ts, msg),
            None => msg.to_string(),
        };
        lines.push(line);
    }

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
        let json = r#"[
            {"timestamp": "2024-01-15T12:00:00Z", "message": "User logged in"},
            {"timestamp": "2024-01-15T12:01:00Z", "message": "Config changed"}
        ]"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "2024-01-15T12:00:00Z User logged in");
        assert_eq!(lines[1], "2024-01-15T12:01:00Z Config changed");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_no_timestamp() -> Result<(), String> {
        let json = r#"[{"message": "something happened"}]"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "something happened");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_alt_fields() -> Result<(), String> {
        let json = r#"[{"time": "12:00", "msg": "event"}]"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "12:00 event");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_empty_array() -> Result<(), String> {
        let lines = parse_audit_log_json("[]")?;
        assert!(lines.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_skips_empty_message() -> Result<(), String> {
        let json = r#"[{"message": ""}, {"message": "real entry"}]"#;
        let lines = parse_audit_log_json(json)?;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "real entry");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_paginated() -> Result<(), String> {
        let json = r#"{"entries": [{"message": "entry one"}, {"message": "entry two"}]}"#;
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
}
