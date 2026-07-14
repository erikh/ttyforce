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
    path: String,
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

/// Structured audit log entry for tabular display.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub timestamp: String,
    pub success: bool,
    pub path: String,
    pub action: String,
    pub detail: String,
    pub error: String,
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
                etc_prefix.map(|p| format!("{}/ttyforce/api-token", p)),
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
    ///
    /// A failure on either endpoint is returned, not swallowed. Discarding it
    /// silently is what let a body the client could not parse render as a panel
    /// with no services in it — indistinguishable from a box that genuinely has
    /// none, and impossible to diagnose from the screen.
    pub fn fetch_all_services(&self) -> Result<Vec<ServiceInfo>, String> {
        let mut all = parse_units_json(&self.http_get("/system-services")?)?;
        all.extend(parse_units_json(&self.http_get("/systemd/units?limit=100")?)?);
        Ok(all)
    }

    /// Fetch audit log entries from the Town OS API.
    /// Returns structured entries (most recent last).
    pub fn fetch_audit_log(&self) -> Result<Vec<AuditEntry>, String> {
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

        parse_http_response(&response)
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

        parse_http_response(&response)
    }
}

/// Split an HTTP/1.1 response into status, headers and body, and return the body
/// with its transfer encoding undone.
///
/// The Town OS API is an HTTP/1.1 server and frames a response one of two ways
/// depending only on how big it is: a small one gets `Content-Length` and arrives
/// verbatim, a large one gets `Transfer-Encoding: chunked` and arrives as
/// `<hex-len>\r\n<bytes>\r\n...0\r\n\r\n`. Handing the chunked form straight to a
/// JSON parser fails on the leading length line — so the status panel would show
/// every service on a box with few of them and NONE on a box with many, which is
/// exactly backwards. An HTTP/1.1 client has to decode chunked; there is no
/// opting out.
pub fn parse_http_response(response: &str) -> Result<String, String> {
    let sep = response
        .find("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response: no header terminator".to_string())?;
    let head = &response[..sep];
    let body = &response[sep + 4..];

    let mut lines = head.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| "malformed HTTP response: no status line".to_string())?;
    if let Some(code_str) = status_line.split_whitespace().nth(1) {
        if let Ok(code) = code_str.parse::<u16>() {
            if code >= 400 {
                return Err(format!("API returned HTTP {}", code));
            }
        }
    }

    let chunked = lines.any(|line| match line.split_once(':') {
        Some((name, value)) => {
            name.trim().eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
        }
        None => false,
    });
    if !chunked {
        return Ok(body.to_string());
    }

    let decoded = decode_chunked(body.as_bytes())?;
    String::from_utf8(decoded).map_err(|e| format!("chunked body is not valid UTF-8: {}", e))
}

/// Decode a chunked transfer-encoded body.
///
/// Operates on bytes, not chars: a chunk length counts bytes, and a service
/// description carrying any non-ASCII character would put a byte offset in the
/// middle of a UTF-8 sequence — slicing a &str there panics.
fn decode_chunked(mut rest: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| "malformed chunked body: unterminated chunk size".to_string())?;
        let size_line = std::str::from_utf8(&rest[..line_end])
            .map_err(|e| format!("malformed chunk size: {}", e))?;
        // A chunk size may carry extensions: "1a;name=value".
        let size_text = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|e| format!("malformed chunk size {:?}: {}", size_text, e))?;

        rest = &rest[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size {
            return Err(format!(
                "truncated chunked body: want {} bytes, have {}",
                size,
                rest.len()
            ));
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size..];

        if !rest.starts_with(b"\r\n") {
            return Err("malformed chunked body: missing chunk terminator".to_string());
        }
        rest = &rest[2..];
    }
}

/// Parse the JSON response from the /systemd/units endpoint into ServiceInfo structs.
/// Handles both paginated format `{ "entries": [...] }` and bare array `[...]`.
pub fn parse_units_json(body: &str) -> Result<Vec<ServiceInfo>, String> {
    let units: Vec<RawUnit> = if let Ok(paginated) = serde_json::from_str::<PaginatedUnits>(body) {
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

/// Format a JSON detail string into key=value pairs for tabular display.
/// e.g. `{"username":"erikh","admin":true}` → `username=erikh admin=true`
/// Non-JSON strings are returned as-is.
pub fn format_detail_json(detail: &str) -> String {
    if detail.is_empty() {
        return String::new();
    }
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(detail) {
        let pairs: Vec<String> = map
            .iter()
            .map(|(k, v)| {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Null => "null".to_string(),
                    other => other.to_string(),
                };
                format!("{}={}", k, val)
            })
            .collect();
        pairs.join(" ")
    } else {
        detail.to_string()
    }
}

/// Parse the JSON response from the POST /audit/log endpoint into structured entries.
/// Response format: `{ "entries": [...], "has_more": bool, ... }`
/// Each entry has: action, path, detail, success, error, created_at, account.
pub fn parse_audit_log_json(body: &str) -> Result<Vec<AuditEntry>, String> {
    let entries: Vec<RawAuditEntry> =
        if let Ok(paginated) = serde_json::from_str::<PaginatedAuditLog>(body) {
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

    let result = entries
        .into_iter()
        .filter(|e| !e.action.is_empty())
        .map(|e| AuditEntry {
            timestamp: format_timestamp(&e.created_at),
            success: e.success,
            path: e.path,
            action: e.action,
            detail: format_detail_json(&e.detail),
            error: e.error,
        })
        .collect();

    Ok(result)
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
        let json =
            r#"[{"name": "test.service", "active_state": "failed", "description": "A test"}]"#;
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
            {"action": "Install package", "path": "/packages/install", "detail": "caddy", "success": true, "error": "", "created_at": "2024-01-15T12:00:00Z", "account": "admin"},
            {"action": "Create account", "path": "/account/create", "detail": "{\"username\":\"user1\"}", "success": true, "error": "", "created_at": "2024-01-15T12:01:00.123Z", "account": "admin"}
        ]}"#;
        let entries = parse_audit_log_json(json)?;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action, "Install package");
        assert_eq!(entries[0].path, "/packages/install");
        assert_eq!(entries[0].detail, "caddy");
        assert_eq!(entries[0].timestamp, "2024-01-15 12:00:00");
        assert!(entries[0].success);
        assert_eq!(entries[1].action, "Create account");
        assert_eq!(entries[1].path, "/account/create");
        assert_eq!(entries[1].detail, "username=user1");
        assert_eq!(entries[1].timestamp, "2024-01-15 12:01:00");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_no_timestamp() -> Result<(), String> {
        let json = r#"[{"action": "Something happened", "success": true}]"#;
        let entries = parse_audit_log_json(json)?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "Something happened");
        assert!(entries[0].timestamp.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_failure_with_error() -> Result<(), String> {
        let json = r#"[{"action": "Install package", "path": "/packages/install", "detail": "bad-pkg", "success": false, "error": "not found", "created_at": "2024-01-15T12:00:00Z"}]"#;
        let entries = parse_audit_log_json(json)?;
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].success);
        assert_eq!(entries[0].error, "not found");
        assert_eq!(entries[0].path, "/packages/install");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_failure_no_error_msg() -> Result<(), String> {
        let json = r#"[{"action": "Install package", "success": false, "error": ""}]"#;
        let entries = parse_audit_log_json(json)?;
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].success);
        assert!(entries[0].error.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_empty_array() -> Result<(), String> {
        let entries = parse_audit_log_json("[]")?;
        assert!(entries.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_skips_empty_action() -> Result<(), String> {
        let json =
            r#"[{"action": "", "success": true}, {"action": "Real entry", "success": true}]"#;
        let entries = parse_audit_log_json(json)?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "Real entry");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_paginated() -> Result<(), String> {
        let json = r#"{"entries": [{"action": "entry one", "success": true}, {"action": "entry two", "success": true}], "has_more": false, "total_count": 2}"#;
        let entries = parse_audit_log_json(json)?;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action, "entry one");
        assert_eq!(entries[1].action, "entry two");
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
        let json =
            r#"[{"action": "test", "created_at": "2024-01-15T12:00:00+05:00", "success": true}]"#;
        let entries = parse_audit_log_json(json)?;
        assert_eq!(entries[0].timestamp, "2024-01-15 12:00:00");
        assert_eq!(entries[0].action, "test");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_timestamp_with_negative_timezone_offset() -> Result<(), String> {
        let json =
            r#"[{"action": "test", "created_at": "2024-01-15T12:00:00-05:00", "success": true}]"#;
        let entries = parse_audit_log_json(json)?;
        assert_eq!(entries[0].timestamp, "2024-01-15 12:00:00");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_with_path_field() -> Result<(), String> {
        let json = r#"[{"action": "authenticate", "path": "/account/authenticate", "detail": "{\"username\":\"erikh\"}", "success": true, "created_at": "2026-03-29T19:12:46Z"}]"#;
        let entries = parse_audit_log_json(json)?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/account/authenticate");
        assert_eq!(entries[0].action, "authenticate");
        assert_eq!(entries[0].detail, "username=erikh");
        Ok(())
    }

    #[test]
    fn test_parse_audit_log_json_detail_multiple_fields() -> Result<(), String> {
        let json = r#"[{"action": "create account", "path": "/account/create", "detail": "{\"admin\":true,\"email\":\"e@x.com\",\"username\":\"erikh\"}", "success": true}]"#;
        let entries = parse_audit_log_json(json)?;
        assert_eq!(entries[0].detail, "admin=true email=e@x.com username=erikh");
        Ok(())
    }

    #[test]
    fn test_format_detail_json_object() {
        let detail = r#"{"username":"erikh","admin":true}"#;
        let result = format_detail_json(detail);
        assert!(result.contains("username=erikh"));
        assert!(result.contains("admin=true"));
    }

    #[test]
    fn test_format_detail_json_empty() {
        assert_eq!(format_detail_json(""), "");
    }

    #[test]
    fn test_format_detail_json_plain_string() {
        assert_eq!(format_detail_json("caddy"), "caddy");
    }

    #[test]
    fn test_format_detail_json_nested_object() {
        let detail = r#"{"name":"test","config":{"port":8080}}"#;
        let result = format_detail_json(detail);
        assert!(result.contains("name=test"));
        // Nested objects get JSON representation
        assert!(result.contains("config="));
    }

    #[test]
    fn test_format_detail_json_null_value() {
        let detail = r#"{"key":null}"#;
        assert_eq!(format_detail_json(detail), "key=null");
    }

    #[test]
    fn test_format_detail_json_number_value() {
        let detail = r#"{"port":8080}"#;
        assert_eq!(format_detail_json(detail), "port=8080");
    }

    /// A Content-Length response arrives verbatim.
    #[test]
    fn test_parse_http_response_content_length() -> Result<(), String> {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 64\r\n\r\n{\"entries\":[],\"has_more\":false,\"total_pages\":1,\"total_count\":0}";
        let body = parse_http_response(response)?;
        assert_eq!(
            body,
            r#"{"entries":[],"has_more":false,"total_pages":1,"total_count":0}"#
        );
        Ok(())
    }

    /// Regression: the status panel went blank because the API chunks any
    /// response big enough — and /system-services, the endpoint that carries
    /// every running service, is always big enough. The body then begins with a
    /// hex length line, serde rejects it, and the panel renders empty as though
    /// the box had no services at all. This is the shape the real box returns.
    #[test]
    fn test_parse_http_response_chunked() -> Result<(), String> {
        let json = r#"[{"Name":"town-os-system--rolodex.service","ActiveState":"active","Description":"Town OS System Service: Rolodex DNS"}]"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            json.len(),
            json
        );

        let body = parse_http_response(&response)?;
        assert_eq!(body, json);

        // And it now parses all the way through to the panel's data type.
        let services = parse_units_json(&body)?;
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "town-os-system--rolodex.service");
        assert_eq!(services[0].active_state, "active");
        Ok(())
    }

    /// A body split across several chunks is reassembled in order.
    #[test]
    fn test_parse_http_response_chunked_multiple_chunks() -> Result<(), String> {
        let response = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n[{\"a\"\r\n4\r\n:1}]\r\n0\r\n\r\n";
        assert_eq!(parse_http_response(response)?, r#"[{"a":1}]"#);
        Ok(())
    }

    /// The header match is case-insensitive and tolerates chunk extensions.
    #[test]
    fn test_parse_http_response_chunked_case_and_extensions() -> Result<(), String> {
        let response =
            "HTTP/1.1 200 OK\r\ntransfer-encoding: Chunked\r\n\r\n3;foo=bar\r\n[1]\r\n0\r\n\r\n";
        assert_eq!(parse_http_response(response)?, "[1]");
        Ok(())
    }

    /// Chunk lengths are byte counts, so a multi-byte character must not be
    /// sliced through the middle.
    #[test]
    fn test_parse_http_response_chunked_multibyte() -> Result<(), String> {
        let json = r#"[{"Description":"café"}]"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            json.len(),
            json
        );
        assert_eq!(parse_http_response(&response)?, json);
        Ok(())
    }

    #[test]
    fn test_parse_http_response_error_status() {
        let response = "HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\n\r\n{}";
        let err = parse_http_response(response).expect_err("403 must be an error");
        assert!(err.contains("403"), "unexpected error: {}", err);
    }

    #[test]
    fn test_parse_http_response_truncated_chunk() {
        let response = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nff\r\n[1]\r\n0\r\n\r\n";
        let err = parse_http_response(response).expect_err("a short chunk must be an error");
        assert!(err.contains("truncated"), "unexpected error: {}", err);
    }

    #[test]
    fn test_parse_http_response_bad_chunk_size() {
        let response = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n[1]\r\n0\r\n\r\n";
        let err = parse_http_response(response).expect_err("a bad chunk size must be an error");
        assert!(err.contains("chunk size"), "unexpected error: {}", err);
    }

    #[test]
    fn test_parse_http_response_no_header_terminator() {
        let err = parse_http_response("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n")
            .expect_err("a headerless response must be an error");
        assert!(err.contains("header terminator"), "unexpected error: {}", err);
    }
}
