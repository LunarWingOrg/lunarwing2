use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::lunarwing::agent::host;

const DEFAULT_BASE_URL: &str = "https://git.lunarwing.org";
const CONFIG_PATH: &str = "config/gitlab.json";
const TOKEN_SECRET: &str = "gitlab_token";
const MAX_ERROR_CHARS: usize = 2048;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitlabConfig {
    base_url: String,
}

pub struct GitlabClient {
    api_base: String,
}

impl GitlabClient {
    pub fn load() -> Result<Self, String> {
        if !host::secret_exists(TOKEN_SECRET) {
            return Err(
                "GitLab token not configured. Store a personal access token as 'gitlab_token'."
                    .to_string(),
            );
        }

        let configured = match host::workspace_read(CONFIG_PATH) {
            Some(raw) => {
                let config: GitlabConfig = serde_json::from_str(&raw)
                    .map_err(|_| format!("{CONFIG_PATH} is not valid GitLab configuration"))?;
                config.base_url
            }
            None => DEFAULT_BASE_URL.to_string(),
        };
        let api_base = normalize_api_base(&configured)?;
        host::log(
            host::LogLevel::Debug,
            &format!("GitLab API host configured: {}", display_origin(&api_base)),
        );
        Ok(Self { api_base })
    }

    pub fn request_json(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let serialized = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|error| format!("Failed to encode GitLab request: {error}"))?;
        let (status, bytes, headers) = self.send(method, endpoint, serialized.as_deref())?;
        ensure_success(status, &bytes)?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| format!("GitLab returned invalid JSON (HTTP {status})"))?;
        Ok(with_pagination(value, &headers))
    }

    pub fn resource_exists(&self, endpoint: &str) -> Result<bool, String> {
        let (status, bytes, _) = self.send("GET", endpoint, None)?;
        match status {
            200..=299 => Ok(true),
            404 => Ok(false),
            _ => {
                ensure_success(status, &bytes)?;
                Ok(false)
            }
        }
    }

    fn send(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&[u8]>,
    ) -> Result<(u16, Vec<u8>, String), String> {
        if !endpoint.starts_with('/') || endpoint.starts_with("//") {
            return Err("Invalid GitLab API endpoint".to_string());
        }
        let url = format!("{}{}", self.api_base, endpoint);
        let path = endpoint.split('?').next().unwrap_or(endpoint);
        host::log(
            host::LogLevel::Debug,
            &format!("GitLab API request: {method} {path}"),
        );
        let headers = if body.is_some() {
            r#"{"Content-Type":"application/json; charset=utf-8"}"#
        } else {
            "{}"
        };
        let response = host::http_request(method, &url, headers, body, Some(60_000))
            .map_err(|error| format!("GitLab HTTP request failed: {error}"))?;
        Ok((response.status, response.body, response.headers_json))
    }
}

pub fn url_encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(char::from(byte));
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                output.push('%');
                output.push(char::from(HEX[(byte >> 4) as usize]));
                output.push(char::from(HEX[(byte & 0x0f) as usize]));
            }
        }
    }
    output
}

pub fn project_path(project: &str) -> Result<String, String> {
    require_nonempty("project", project)?;
    if project.len() > 512 || project.chars().any(char::is_control) {
        return Err("project must be at most 512 characters without control characters".into());
    }
    if project.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(project.to_string())
    } else {
        let project = project.trim_matches('/');
        require_nonempty("project", project)?;
        Ok(url_encode(project))
    }
}

pub fn require_nonempty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(())
    }
}

pub fn page(per_page: u32, page: u32) -> (u32, u32) {
    (per_page.clamp(1, 100), page.max(1))
}

fn normalize_api_base(value: &str) -> Result<String, String> {
    let mut url = Url::parse(value.trim()).map_err(|_| "Invalid GitLab base_url".to_string())?;
    if url.scheme() != "https" {
        return Err("GitLab base_url must use HTTPS".to_string());
    }
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "GitLab base_url must contain a host and no credentials, query, or fragment"
                .to_string(),
        );
    }
    if url.port().is_some_and(|port| port != 443) {
        return Err("GitLab base_url must use default HTTPS port 443".to_string());
    }

    let root = url.path().trim_end_matches('/');
    let api_path = if root.ends_with("/api/v4") {
        root.to_string()
    } else if root.is_empty() {
        "/api/v4".to_string()
    } else {
        format!("{root}/api/v4")
    };
    url.set_path(&api_path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn display_origin(api_base: &str) -> String {
    Url::parse(api_base)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?;
            Some(match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            })
        })
        .unwrap_or_else(|| "<invalid>".to_string())
}

fn ensure_success(status: u16, body: &[u8]) -> Result<(), String> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    let reason = extract_error(body).unwrap_or_else(|| "request failed".to_string());
    Err(format!("GitLab API returned HTTP {status}: {reason}"))
}

fn with_pagination(value: Value, headers_json: &str) -> Value {
    if !value.is_array() {
        return value;
    }
    let Ok(headers) = serde_json::from_str::<serde_json::Map<String, Value>>(headers_json) else {
        return value;
    };
    let wanted = [
        ("x-page", "page"),
        ("x-next-page", "next_page"),
        ("x-prev-page", "prev_page"),
        ("x-per-page", "per_page"),
        ("x-total", "total"),
        ("x-total-pages", "total_pages"),
    ];
    let mut pagination = serde_json::Map::new();
    for (header_name, output_name) in wanted {
        if let Some((_, header_value)) = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(header_name))
        {
            pagination.insert(output_name.to_string(), header_value.clone());
        }
    }
    if pagination.is_empty() {
        value
    } else {
        serde_json::json!({ "items": value, "pagination": pagination })
    }
}

fn extract_error(body: &[u8]) -> Option<String> {
    let parsed: Value = serde_json::from_slice(body).ok()?;
    let value = parsed
        .get("message")
        .or_else(|| parsed.get("error_description"))
        .or_else(|| parsed.get("error"))?;
    let text = value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string());
    Some(truncate(&text, MAX_ERROR_CHARS))
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_error, normalize_api_base, page, project_path, url_encode, with_pagination,
    };

    #[test]
    fn normalizes_root_and_relative_install_urls() {
        assert_eq!(
            normalize_api_base("https://git.example.test/").unwrap(),
            "https://git.example.test/api/v4"
        );
        assert_eq!(
            normalize_api_base("https://git.example.test/gitlab/").unwrap(),
            "https://git.example.test/gitlab/api/v4"
        );
        assert_eq!(
            normalize_api_base("https://git.example.test/api/v4").unwrap(),
            "https://git.example.test/api/v4"
        );
    }

    #[test]
    fn rejects_unsafe_base_urls() {
        for value in [
            "http://git.example.test",
            "https://user@git.example.test",
            "https://git.example.test?target=evil",
            "https://git.example.test/#fragment",
            "https://git.example.test:8443",
            "not a url",
        ] {
            assert!(normalize_api_base(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn encodes_project_paths_and_query_values() {
        assert_eq!(project_path("42").unwrap(), "42");
        assert_eq!(project_path("group/project").unwrap(), "group%2Fproject");
        assert_eq!(url_encode("feature/x y"), "feature%2Fx%20y");
        assert!(project_path(" ").is_err());
    }

    #[test]
    fn clamps_pagination() {
        assert_eq!(page(0, 0), (1, 1));
        assert_eq!(page(500, 2), (100, 2));
    }

    #[test]
    fn extracts_structured_errors_without_unbounded_output() {
        let error = extract_error(br#"{"message":{"title":["is invalid"]}}"#).unwrap();
        assert_eq!(error, r#"{"title":["is invalid"]}"#);
        let long = format!(r#"{{"message":"{}"}}"#, "x".repeat(3000));
        assert!(extract_error(long.as_bytes()).unwrap().ends_with("..."));
    }

    #[test]
    fn preserves_gitlab_pagination_headers() {
        let value = with_pagination(
            serde_json::json!([{"id": 1}]),
            r#"{"X-Page":"2","x-next-page":"3","X-Total-Pages":"5"}"#,
        );
        assert_eq!(value["items"][0]["id"], 1);
        assert_eq!(value["pagination"]["page"], "2");
        assert_eq!(value["pagination"]["next_page"], "3");
        assert_eq!(value["pagination"]["total_pages"], "5");
    }
}
