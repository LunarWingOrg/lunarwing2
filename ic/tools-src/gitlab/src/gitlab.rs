use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::lunarwing::agent::host;

const DEFAULT_BASE_URL: &str = "https://git.lunarwing.org";
const CONFIG_PATH: &str = "config/gitlab.json";
const TOKEN_SECRET: &str = "gitlab_token";
const MAX_ERROR_CHARS: usize = 2048;
/// Auto-pagination safety caps: 20 pages stay well under the per-execution
/// host HTTP-request budget; 2000 items / 3 MiB of source JSON keep merged
/// responses within the configured response-size limit and the guest memory
/// ceiling (a parsed Value tree is several times its JSON text size).
const MAX_AUTO_PAGES: u32 = 20;
const MAX_AUTO_ITEMS: usize = 2_000;
const MAX_AUTO_BYTES: usize = 3 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitlabConfig {
    base_url: String,
}

pub struct GitlabClient {
    api_base: String,
    /// Test seam (run-214 NX-6): when `recording` is set, `send` captures
    /// the (method, endpoint) pair into `recorded_requests` and returns
    /// without touching the host, so dispatch-level tests can assert on
    /// the exact request URL. Production clients are only ever built by
    /// `load`, which never records.
    #[cfg(test)]
    recording: bool,
    #[cfg(test)]
    recorded_requests: RecordedRequests,
}

/// Recorded (method, endpoint) pairs from a recording client (NX-6).
#[cfg(test)]
pub type RecordedRequests = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

/// A fresh recorder for a recording client (NX-6 test seam).
#[cfg(test)]
pub fn recorder() -> RecordedRequests {
    std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
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
        Ok(Self {
            api_base,
            #[cfg(test)]
            recording: false,
            #[cfg(test)]
            recorded_requests: recorder(),
        })
    }

    /// A client that records requests instead of sending them (NX-6 test
    /// seam): every request returns an error after being captured, so a
    /// dispatch-level test observes exactly the first request issued.
    #[cfg(test)]
    pub fn recording(recorded: RecordedRequests) -> Self {
        Self {
            api_base: "https://gitlab-recording.invalid/api/v4".to_string(),
            recording: true,
            recorded_requests: recorded,
        }
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

    pub fn request_json_allow_404(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&Value>,
    ) -> Result<Option<Value>, String> {
        let serialized = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|error| format!("Failed to encode GitLab request: {error}"))?;
        let (status, bytes, headers) = self.send(method, endpoint, serialized.as_deref())?;
        if status == 404 {
            return Ok(None);
        }
        ensure_success(status, &bytes)?;
        if bytes.is_empty() {
            return Ok(Some(Value::Null));
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| format!("GitLab returned invalid JSON (HTTP {status})"))?;
        Ok(Some(with_pagination(value, &headers)))
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

    /// Fetch one keyset-paginated Work Items page with an explicit opaque
    /// continuation cursor extracted from GitLab's `Link: rel="next"` header.
    pub fn request_keyset_page(&self, endpoint: &str) -> Result<Value, String> {
        let (status, bytes, headers) = self.send("GET", endpoint, None)?;
        ensure_success(status, &bytes)?;
        let items: Vec<Value> = serde_json::from_slice(&bytes)
            .map_err(|_| format!("GitLab returned invalid JSON (HTTP {status})"))?;
        let next_endpoint = parse_next_link(&self.api_base, &headers)?;
        Ok(keyset_page_value(items, next_endpoint.as_deref()))
    }

    /// Fetch every Work Items page by following GitLab's opaque keyset links.
    /// Stops at the same page/item/byte caps used by offset auto-pagination.
    pub fn request_all_keyset_pages(&self, endpoint: &str) -> Result<Value, String> {
        let api_base = self.api_base.clone();
        let fetch = |next_endpoint: &str| {
            let (status, bytes, headers) = self.send("GET", next_endpoint, None)?;
            ensure_success(status, &bytes)?;
            let items: Vec<Value> = serde_json::from_slice(&bytes)
                .map_err(|_| format!("GitLab returned invalid JSON (HTTP {status})"))?;
            let next = parse_next_link(&api_base, &headers)?;
            Ok((items, next, bytes.len()))
        };
        let outcome = collect_all_keyset_pages(
            endpoint,
            fetch,
            MAX_AUTO_PAGES,
            MAX_AUTO_ITEMS,
            MAX_AUTO_BYTES,
        )?;
        Ok(all_keyset_pages_value(outcome))
    }

    /// Fetch every page of a paginated GET endpoint in one call.
    ///
    /// Follows `x-next-page` until it is absent/blank or a safety cap (pages,
    /// items, or accumulated bytes) is hit, then merges all items into a
    /// single response. The `pagination` object describes the merged result:
    /// `fetched_pages`, `truncated`, and (only when truncated) `resume_page`
    /// for continuation with a normal paged call.
    pub fn request_all_pages(&self, endpoint: &str) -> Result<Value, String> {
        let fetch = |page_number: u32| {
            let separator = if endpoint.contains('?') { '&' } else { '?' };
            let paged = format!("{endpoint}{separator}page={page_number}");
            let (status, bytes, headers) = self.send("GET", &paged, None)?;
            ensure_success(status, &bytes)?;
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|_| format!("GitLab returned invalid JSON (HTTP {status})"))?;
            match value {
                Value::Array(items) => Ok((items, headers, bytes.len())),
                _ => Err(format!(
                    "GitLab list endpoint returned a non-array body (HTTP {status})"
                )),
            }
        };
        let outcome = collect_all_pages(fetch, MAX_AUTO_PAGES, MAX_AUTO_ITEMS, MAX_AUTO_BYTES)?;
        Ok(all_pages_value(outcome))
    }

    fn send(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&[u8]>,
    ) -> Result<(u16, Vec<u8>, String), String> {
        // NX-6 test seam: record instead of issuing, before any host call.
        #[cfg(test)]
        if self.recording {
            self.recorded_requests
                .lock()
                .expect("recorder lock")
                .push((method.to_string(), endpoint.to_string()));
            return Err("recording client captured the request without sending".to_string());
        }
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
    let pagination = pagination_headers(headers_json);
    if pagination.is_empty() {
        value
    } else {
        serde_json::json!({ "items": value, "pagination": pagination })
    }
}

/// Extract GitLab pagination response headers into a JSON map.
///
/// GitLab emits blank `x-next-page` / `x-prev-page` values on boundary pages;
/// those are skipped so agents never follow a blank `next_page` into a
/// re-fetch loop that repeats page 1 forever (which looks like a stuck tool).
fn pagination_headers(headers_json: &str) -> serde_json::Map<String, Value> {
    let wanted = [
        ("x-page", "page"),
        ("x-next-page", "next_page"),
        ("x-prev-page", "prev_page"),
        ("x-per-page", "per_page"),
        ("x-total", "total"),
        ("x-total-pages", "total_pages"),
    ];
    let mut pagination = serde_json::Map::new();
    let Ok(headers) = serde_json::from_str::<serde_json::Map<String, Value>>(headers_json) else {
        return pagination;
    };
    for (header_name, output_name) in wanted {
        if let Some((_, header_value)) = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(header_name))
        {
            if header_value.as_str().is_some_and(|value| value.is_empty()) {
                continue;
            }
            pagination.insert(output_name.to_string(), header_value.clone());
        }
    }
    pagination
}

fn header_owned_string(headers_json: &str, wanted: &str) -> Option<String> {
    let headers = serde_json::from_str::<serde_json::Map<String, Value>>(headers_json).ok()?;
    let (_, value) = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(wanted))?;
    value.as_str().map(ToOwned::to_owned)
}

/// Parse GitLab's RFC 5988-style `Link` header and return only a same-origin,
/// same-API-base relative endpoint for `rel="next"`. This prevents a server or
/// proxy from turning pagination into credential-bearing cross-origin SSRF.
fn parse_next_link(api_base: &str, headers_json: &str) -> Result<Option<String>, String> {
    let Some(link) = header_owned_string(headers_json, "link") else {
        return Ok(None);
    };
    let Some(target) = link.split(',').find_map(|part| {
        let mut sections = part.trim().split(';');
        let target = sections.next()?.trim();
        let is_next = sections.any(|section| {
            let section = section.trim();
            section.eq_ignore_ascii_case("rel=\"next\"") || section.eq_ignore_ascii_case("rel=next")
        });
        is_next.then_some(target)
    }) else {
        return Ok(None);
    };
    let target = target
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .ok_or_else(|| "GitLab pagination Link has an invalid next target".to_string())?;
    let base =
        Url::parse(api_base).map_err(|_| "Invalid configured GitLab API base".to_string())?;
    let next =
        Url::parse(target).map_err(|_| "GitLab pagination Link is not a valid URL".to_string())?;
    let api_path = base.path().trim_end_matches('/');
    let Some(relative_path) = next.path().strip_prefix(api_path) else {
        return Err("GitLab pagination Link escaped the configured API path".to_string());
    };
    if next.scheme() != base.scheme()
        || next.host_str() != base.host_str()
        || next.port_or_known_default() != base.port_or_known_default()
        || !relative_path.starts_with('/')
        || !next.username().is_empty()
        || next.password().is_some()
        || next.fragment().is_some()
    {
        return Err("GitLab pagination Link escaped the configured API origin".to_string());
    }
    let mut endpoint = relative_path.to_string();
    if let Some(query) = next.query() {
        endpoint.push('?');
        endpoint.push_str(query);
    }
    Ok(Some(endpoint))
}

/// Parse the `x-next-page` header, treating missing, blank, or malformed
/// values as "no further page".
fn parse_next_page(headers_json: &str) -> Option<u32> {
    let headers = serde_json::from_str::<serde_json::Map<String, Value>>(headers_json).ok()?;
    let (_, value) = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("x-next-page"))?;
    let raw = value.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<u32>().ok().filter(|page| *page >= 1)
}

/// Result of a bounded auto-pagination run.
struct AllPagesOutcome {
    /// Merged items across all fetched pages.
    items: Vec<Value>,
    /// Headers of the last fetched page (for total/total_pages).
    headers_json: String,
    /// How many pages were fetched.
    fetched_pages: u32,
    /// Next unfetched page when the run stopped early; `None` on the last page.
    resume_page: Option<u32>,
}

/// Loop over pages via `fetch`, merging items until there is no next page or
/// a safety cap (pages, items, or accumulated source bytes) is hit. A next
/// page equal to the current page is treated as "no further page" so a
/// misbehaving server cannot loop the fetch forever. `fetch` returns the
/// page's items, its response headers, and the raw body length in bytes.
fn collect_all_pages(
    mut fetch: impl FnMut(u32) -> Result<(Vec<Value>, String, usize), String>,
    max_pages: u32,
    max_items: usize,
    max_bytes: usize,
) -> Result<AllPagesOutcome, String> {
    let mut items = Vec::new();
    let mut seen_items = HashSet::new();
    let mut total_bytes = 0usize;
    let mut page_number = 1u32;
    let mut fetched_pages = 0u32;
    loop {
        let (page_items, headers, byte_len) = fetch(page_number)?;
        fetched_pages += 1;
        append_unique_items(&mut items, &mut seen_items, page_items);
        total_bytes = total_bytes.saturating_add(byte_len);
        let next = parse_next_page(&headers).filter(|next| *next != page_number);
        let capped =
            fetched_pages >= max_pages || items.len() >= max_items || total_bytes >= max_bytes;
        let resume_page = match (next, capped) {
            (Some(next_page), false) => {
                page_number = next_page;
                continue;
            }
            (Some(next_page), true) => Some(next_page),
            (None, _) => None,
        };
        return Ok(AllPagesOutcome {
            items,
            headers_json: headers,
            fetched_pages,
            resume_page,
        });
    }
}

/// Build the merged auto-pagination response. Collection-wide headers
/// (`per_page`, `total`, `total_pages`) are kept; single-page headers
/// (`page`, `prev_page`, `next_page`) are replaced by the merged-run fields.
fn all_pages_value(outcome: AllPagesOutcome) -> Value {
    let mut pagination = pagination_headers(&outcome.headers_json);
    for single_page_key in ["page", "prev_page", "next_page"] {
        pagination.remove(single_page_key);
    }
    pagination.insert(
        "fetched_pages".to_string(),
        Value::from(outcome.fetched_pages),
    );
    let total_matches = pagination
        .get("total")
        .and_then(Value::as_str)
        .and_then(|total| total.parse::<usize>().ok())
        .is_none_or(|total| total == outcome.items.len());
    let truncated = outcome.resume_page.is_some() || !total_matches;
    pagination.insert("truncated".to_string(), Value::from(truncated));
    pagination.insert("complete".to_string(), Value::from(!truncated));
    pagination.insert(
        "returned_items".to_string(),
        Value::from(outcome.items.len() as u64),
    );
    if let Some(resume_page) = outcome.resume_page {
        pagination.insert("resume_page".to_string(), Value::from(resume_page));
    }
    serde_json::json!({ "items": outcome.items, "pagination": pagination })
}

fn keyset_page_value(items: Vec<Value>, next_endpoint: Option<&str>) -> Value {
    let next_cursor = next_endpoint.and_then(endpoint_cursor);
    let complete = next_endpoint.is_none();
    let returned_items = items.len() as u64;
    let mut pagination = serde_json::Map::new();
    pagination.insert("pagination".into(), Value::String("keyset".into()));
    pagination.insert("has_next".into(), Value::from(!complete));
    pagination.insert("complete".into(), Value::from(complete));
    pagination.insert("returned_items".into(), Value::from(returned_items));
    if complete {
        pagination.insert("total".into(), Value::from(returned_items));
    }
    if let Some(cursor) = next_cursor {
        pagination.insert("next_cursor".into(), Value::String(cursor));
    }
    serde_json::json!({ "items": items, "pagination": pagination })
}

fn endpoint_cursor(endpoint: &str) -> Option<String> {
    let query = endpoint.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "cursor" && !value.is_empty()).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

struct AllKeysetPagesOutcome {
    items: Vec<Value>,
    fetched_pages: u32,
    next_endpoint: Option<String>,
    cycle_detected: bool,
}

fn append_unique_items(
    destination: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    page_items: Vec<Value>,
) {
    for item in page_items {
        let stable_id = item
            .get("global_id")
            .or_else(|| item.get("id"))
            .or_else(|| item.get("iid"))
            .map(Value::to_string);
        if stable_id.as_ref().is_none_or(|id| seen.insert(id.clone())) {
            destination.push(item);
        }
    }
}

fn collect_all_keyset_pages(
    first_endpoint: &str,
    mut fetch: impl FnMut(&str) -> Result<(Vec<Value>, Option<String>, usize), String>,
    max_pages: u32,
    max_items: usize,
    max_bytes: usize,
) -> Result<AllKeysetPagesOutcome, String> {
    let mut items = Vec::new();
    let mut seen_items = HashSet::new();
    let mut seen_endpoints = HashSet::new();
    let mut fetched_pages = 0u32;
    let mut total_bytes = 0usize;
    let mut endpoint = first_endpoint.to_string();
    loop {
        if !seen_endpoints.insert(endpoint.clone()) {
            return Ok(AllKeysetPagesOutcome {
                items,
                fetched_pages,
                next_endpoint: None,
                cycle_detected: true,
            });
        }
        let current = endpoint.clone();
        let (page_items, next_endpoint, byte_len) = fetch(&current)?;
        fetched_pages += 1;
        append_unique_items(&mut items, &mut seen_items, page_items);
        total_bytes = total_bytes.saturating_add(byte_len);
        let cycle_detected = next_endpoint
            .as_ref()
            .is_some_and(|next| seen_endpoints.contains(next));
        let next_endpoint = next_endpoint.filter(|next| !seen_endpoints.contains(next));
        let capped =
            fetched_pages >= max_pages || items.len() >= max_items || total_bytes >= max_bytes;
        match (next_endpoint, capped) {
            (Some(next), false) => endpoint = next,
            (next, _) => {
                return Ok(AllKeysetPagesOutcome {
                    items,
                    fetched_pages,
                    next_endpoint: next,
                    cycle_detected,
                });
            }
        }
    }
}

fn all_keyset_pages_value(outcome: AllKeysetPagesOutcome) -> Value {
    let next_cursor = outcome.next_endpoint.as_deref().and_then(endpoint_cursor);
    let truncated = outcome.next_endpoint.is_some() || outcome.cycle_detected;
    let returned_items = outcome.items.len() as u64;
    let mut pagination = serde_json::Map::new();
    pagination.insert("pagination".into(), Value::String("keyset".into()));
    pagination.insert("fetched_pages".into(), Value::from(outcome.fetched_pages));
    pagination.insert("truncated".into(), Value::from(truncated));
    pagination.insert("complete".into(), Value::from(!truncated));
    pagination.insert("returned_items".into(), Value::from(returned_items));
    if !truncated {
        pagination.insert("total".into(), Value::from(returned_items));
    }
    if outcome.cycle_detected {
        pagination.insert("cycle_detected".into(), Value::from(true));
    }
    if let Some(cursor) = next_cursor {
        pagination.insert("next_cursor".into(), Value::String(cursor));
    }
    serde_json::json!({ "items": outcome.items, "pagination": pagination })
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
        MAX_AUTO_BYTES, all_keyset_pages_value, collect_all_keyset_pages, collect_all_pages,
        endpoint_cursor, extract_error, keyset_page_value, normalize_api_base, page,
        parse_next_link, parse_next_page, project_path, url_encode, with_pagination,
    };
    use serde_json::{Value, json};

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

    #[test]
    fn pagination_skips_blank_boundary_headers() {
        // GitLab sends an empty x-next-page on the last page; it must not be
        // surfaced, or agents follow it into a re-fetch loop.
        let value = with_pagination(
            serde_json::json!([{"id": 1}]),
            r#"{"X-Page":"3","X-Next-Page":"","X-Prev-Page":"2","X-Total":"21"}"#,
        );
        let pagination = &value["pagination"];
        assert!(pagination.get("next_page").is_none());
        assert_eq!(pagination["prev_page"], "2");
        assert_eq!(pagination["total"], "21");
    }

    #[test]
    fn parse_next_page_handles_missing_blank_and_invalid() {
        assert_eq!(parse_next_page(r#"{"x-next-page":"4"}"#), Some(4));
        assert_eq!(parse_next_page(r#"{"X-Next-Page":"2"}"#), Some(2));
        assert_eq!(parse_next_page(r#"{"x-next-page":""}"#), None);
        assert_eq!(parse_next_page(r#"{"x-next-page":"  "}"#), None);
        assert_eq!(parse_next_page(r#"{"x-next-page":"abc"}"#), None);
        assert_eq!(parse_next_page(r#"{"x-next-page":"0"}"#), None);
        assert_eq!(parse_next_page(r#"{}"#), None);
        assert_eq!(parse_next_page("not json"), None);
    }

    fn canned_page(ids: &[u64], next_page: &str) -> (Vec<Value>, String, usize) {
        canned_page_sized(ids, next_page, 128)
    }

    fn canned_page_sized(
        ids: &[u64],
        next_page: &str,
        byte_len: usize,
    ) -> (Vec<Value>, String, usize) {
        let items = ids.iter().map(|id| json!({"id": id})).collect();
        let headers = format!(
            r#"{{"x-next-page":"{next_page}","x-total":"6","x-total-pages":"3","x-per-page":"2"}}"#
        );
        (items, headers, byte_len)
    }

    #[test]
    fn auto_pagination_merges_until_last_page() {
        let mut calls = Vec::new();
        let outcome = collect_all_pages(
            |page_number| {
                calls.push(page_number);
                Ok(match page_number {
                    1 => canned_page(&[1, 2], "2"),
                    2 => canned_page(&[3, 4], "3"),
                    3 => canned_page(&[5, 6], ""),
                    other => panic!("unexpected page fetch {other}"),
                })
            },
            20,
            2_000,
            MAX_AUTO_BYTES,
        )
        .unwrap();
        assert_eq!(calls, vec![1, 2, 3]);
        assert_eq!(outcome.items.len(), 6);
        assert_eq!(outcome.fetched_pages, 3);
        assert_eq!(outcome.resume_page, None);

        let merged = super::all_pages_value(outcome);
        assert_eq!(merged["items"].as_array().unwrap().len(), 6);
        let pagination = &merged["pagination"];
        assert_eq!(pagination["fetched_pages"], 3);
        assert_eq!(pagination["truncated"], false);
        assert_eq!(pagination["complete"], true);
        assert_eq!(pagination["returned_items"], 6);
        assert_eq!(pagination["total"], "6");
        assert_eq!(pagination["total_pages"], "3");
        assert!(pagination.get("next_page").is_none());
        assert!(pagination.get("resume_page").is_none());
    }

    #[test]
    fn auto_pagination_deduplicates_and_detects_total_mismatch() {
        let outcome = collect_all_pages(
            |page_number| {
                Ok(match page_number {
                    1 => canned_page(&[1, 2], "2"),
                    2 => canned_page(&[2, 3], ""),
                    other => panic!("unexpected page fetch {other}"),
                })
            },
            20,
            2_000,
            MAX_AUTO_BYTES,
        )
        .unwrap();
        assert_eq!(
            outcome
                .items
                .iter()
                .map(|item| item["id"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let merged = super::all_pages_value(outcome);
        // Canned headers claim six total while only three unique records were
        // returned, so the response must not claim a complete inventory.
        assert_eq!(merged["pagination"]["returned_items"], 3);
        assert_eq!(merged["pagination"]["complete"], false);
        assert_eq!(merged["pagination"]["truncated"], true);
    }

    #[test]
    fn auto_pagination_page_cap_reports_resume_page() {
        let outcome = collect_all_pages(
            |page_number| Ok(canned_page(&[1, 2], &(page_number + 1).to_string())),
            2,
            2_000,
            MAX_AUTO_BYTES,
        )
        .unwrap();
        assert_eq!(outcome.fetched_pages, 2);
        assert_eq!(outcome.resume_page, Some(3));
        let merged = super::all_pages_value(outcome);
        assert_eq!(merged["pagination"]["truncated"], true);
        assert_eq!(merged["pagination"]["resume_page"], 3);
        assert!(merged["pagination"].get("next_page").is_none());
    }

    #[test]
    fn auto_pagination_item_cap_reports_resume_page() {
        let outcome = collect_all_pages(
            |page_number| Ok(canned_page(&[1, 2, 3], &(page_number + 1).to_string())),
            20,
            3,
            MAX_AUTO_BYTES,
        )
        .unwrap();
        assert_eq!(outcome.fetched_pages, 1);
        assert_eq!(outcome.resume_page, Some(2));
    }

    #[test]
    fn auto_pagination_byte_cap_reports_resume_page() {
        let outcome = collect_all_pages(
            |page_number| {
                Ok(canned_page_sized(
                    &[1, 2],
                    &(page_number + 1).to_string(),
                    1024,
                ))
            },
            20,
            2_000,
            1_500,
        )
        .unwrap();
        assert_eq!(outcome.fetched_pages, 2);
        assert_eq!(outcome.resume_page, Some(3));
    }

    #[test]
    fn auto_pagination_repeated_next_page_does_not_loop() {
        // A server that keeps advertising the current page must not loop.
        let outcome =
            collect_all_pages(|_| Ok(canned_page(&[1], "1")), 20, 2_000, MAX_AUTO_BYTES).unwrap();
        assert_eq!(outcome.fetched_pages, 1);
        assert_eq!(outcome.resume_page, None);
    }

    #[test]
    fn auto_pagination_propagates_fetch_errors() {
        let result = collect_all_pages(
            |page_number| {
                if page_number == 2 {
                    Err("boom".to_string())
                } else {
                    Ok(canned_page(&[1], "2"))
                }
            },
            20,
            2_000,
            MAX_AUTO_BYTES,
        );
        assert_eq!(result.err().as_deref(), Some("boom"));
    }

    #[test]
    fn keyset_link_parser_returns_only_same_api_origin() {
        let headers = r#"{"Link":"<https://git.example.test/api/v4/projects/35/-/work_items?cursor=abc%2F123&per_page=2>; rel=\"next\", <https://git.example.test/api/v4/projects/35/-/work_items?cursor=old>; rel=\"prev\""}"#;
        let next = parse_next_link("https://git.example.test/api/v4", headers)
            .unwrap()
            .unwrap();
        assert_eq!(
            next,
            "/projects/35/-/work_items?cursor=abc%2F123&per_page=2"
        );
        assert_eq!(endpoint_cursor(&next).as_deref(), Some("abc/123"));

        let evil = r#"{"Link":"<https://evil.test/api/v4/items?cursor=secret>; rel=\"next\""}"#;
        assert!(parse_next_link("https://git.example.test/api/v4", evil).is_err());
    }

    #[test]
    fn keyset_single_page_reports_explicit_completion() {
        let more = keyset_page_value(
            vec![json!({"global_id":"gid://gitlab/WorkItem/2","iid":2})],
            Some("/api/v4/projects/35/-/work_items?cursor=next%2F2"),
        );
        assert_eq!(more["pagination"]["has_next"], true);
        assert_eq!(more["pagination"]["complete"], false);
        assert_eq!(more["pagination"]["next_cursor"], "next/2");
        assert_eq!(more["pagination"]["returned_items"], 1);

        let last = keyset_page_value(Vec::new(), None);
        assert_eq!(last["pagination"]["has_next"], false);
        assert_eq!(last["pagination"]["complete"], true);
        assert_eq!(last["pagination"]["total"], 0);
    }

    #[test]
    fn keyset_auto_pagination_preserves_order_and_deduplicates_mixed_types() {
        let first = "/api/v4/projects/35/-/work_items?per_page=2";
        let second = "/api/v4/projects/35/-/work_items?cursor=page-2";
        let outcome = collect_all_keyset_pages(
            first,
            |endpoint| match endpoint {
                value if value == first => Ok((
                    vec![
                        json!({"global_id":"gid://gitlab/WorkItem/3","iid":3,"work_item_type":{"name":"Issue"}}),
                        json!({"global_id":"gid://gitlab/WorkItem/2","iid":2,"work_item_type":{"name":"Task"}}),
                    ],
                    Some(second.to_string()),
                    100,
                )),
                value if value == second => Ok((
                    vec![
                        json!({"global_id":"gid://gitlab/WorkItem/2","iid":2,"work_item_type":{"name":"Task"}}),
                        json!({"global_id":"gid://gitlab/WorkItem/1","iid":1,"work_item_type":{"name":"Issue"}}),
                    ],
                    None,
                    100,
                )),
                other => panic!("unexpected endpoint {other}"),
            },
            20,
            2_000,
            MAX_AUTO_BYTES,
        )
        .unwrap();
        assert_eq!(outcome.fetched_pages, 2);
        assert_eq!(outcome.next_endpoint, None);
        assert_eq!(
            outcome
                .items
                .iter()
                .map(|item| item["iid"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [3, 2, 1]
        );
        let merged = all_keyset_pages_value(outcome);
        assert_eq!(merged["pagination"]["complete"], true);
        assert_eq!(merged["pagination"]["returned_items"], 3);
        assert_eq!(merged["pagination"]["total"], 3);
    }

    #[test]
    fn keyset_auto_pagination_cap_reports_next_cursor() {
        let first = "/api/v4/projects/35/-/work_items?per_page=2";
        let outcome = collect_all_keyset_pages(
            first,
            |_| {
                Ok((
                    vec![json!({"global_id":"gid://gitlab/WorkItem/1"})],
                    Some("/api/v4/projects/35/-/work_items?cursor=resume%2F2".into()),
                    100,
                ))
            },
            1,
            2_000,
            MAX_AUTO_BYTES,
        )
        .unwrap();
        let merged = all_keyset_pages_value(outcome);
        assert_eq!(merged["pagination"]["truncated"], true);
        assert_eq!(merged["pagination"]["complete"], false);
        assert_eq!(merged["pagination"]["next_cursor"], "resume/2");
    }

    #[test]
    fn keyset_auto_pagination_repeated_endpoint_does_not_loop() {
        let first = "/api/v4/projects/35/-/work_items?per_page=2";
        let outcome = collect_all_keyset_pages(
            first,
            |_| Ok((vec![json!({"id":1})], Some(first.to_string()), 10)),
            20,
            2_000,
            MAX_AUTO_BYTES,
        )
        .unwrap();
        assert_eq!(outcome.fetched_pages, 1);
        assert_eq!(outcome.next_endpoint, None);
        let merged = all_keyset_pages_value(outcome);
        assert_eq!(merged["pagination"]["cycle_detected"], true);
        assert_eq!(merged["pagination"]["complete"], false);
        assert_eq!(merged["pagination"]["truncated"], true);
        assert!(merged["pagination"].get("total").is_none());
    }
}
