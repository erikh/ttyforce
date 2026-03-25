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

    /// Fetch service/unit status from the Town OS API.
    pub fn fetch_services(&self) -> Result<Vec<ServiceInfo>, String> {
        let body = self.http_get("/units")?;
        parse_units_json(&body)
    }

    /// Perform an HTTP GET request to the Town OS API.
    fn http_get(&self, path: &str) -> Result<String, String> {
        let addr = "127.0.0.1:5309"
            .parse()
            .map_err(|e| format!("bad address: {}", e))?;
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
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

/// Parse the JSON response from the /units endpoint into ServiceInfo structs.
/// Expects a JSON array of objects with at least "Name" and "ActiveState" fields.
pub fn parse_units_json(body: &str) -> Result<Vec<ServiceInfo>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("JSON parse error: {}", e))?;

    let arr = match value.as_array() {
        Some(a) => a,
        None => return Err("expected JSON array".to_string()),
    };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_units_json_basic() {
        let json = r#"[
            {"Name": "caddy.service", "ActiveState": "active", "Description": "Caddy web server"},
            {"Name": "forgejo.service", "ActiveState": "inactive", "Description": "Forgejo git server"}
        ]"#;
        let services = parse_units_json(json).unwrap();
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].name, "caddy.service");
        assert_eq!(services[0].active_state, "active");
        assert_eq!(services[0].description, "Caddy web server");
        assert_eq!(services[1].name, "forgejo.service");
        assert_eq!(services[1].active_state, "inactive");
    }

    #[test]
    fn test_parse_units_json_empty_array() {
        let services = parse_units_json("[]").unwrap();
        assert!(services.is_empty());
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
        assert!(result.unwrap_err().contains("expected JSON array"));
    }

    #[test]
    fn test_parse_units_json_missing_fields() {
        let json = r#"[{"Name": "test.service"}]"#;
        let services = parse_units_json(json).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].active_state, "unknown");
        assert_eq!(services[0].description, "");
    }

    #[test]
    fn test_parse_units_json_lowercase_fields() {
        let json = r#"[{"name": "test.service", "active_state": "failed", "description": "A test"}]"#;
        let services = parse_units_json(json).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "test.service");
        assert_eq!(services[0].active_state, "failed");
    }

    #[test]
    fn test_parse_units_json_skips_empty_name() {
        let json = r#"[{"Name": "", "ActiveState": "active"}, {"Name": "real.service", "ActiveState": "active"}]"#;
        let services = parse_units_json(json).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "real.service");
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
}
