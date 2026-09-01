//! Vision Analysis WASM Tool for LunarWing.
//!
//! Thin client that forwards image analysis requests to the LunarWing
//! Vision Service (OCR sidecar). Does one HTTP call and returns the result.
//!
//! # Design Principles
//!
//! - No `std::env::var()` — WASM sandbox has no env access
//! - No `std::thread::sleep()` — WASM sandbox has no threads
//! - No auth logic — host injects credentials at HTTP boundary
//! - No retry — fail fast; retries belong in the host/agent layer
//! - Strict URL policy — local HTTP or operator-configured remote HTTPS

wit_bindgen::generate!({
    world: "sandboxed-tool",
    path: "../../wit/tool.wit",
});

use serde::{Deserialize, Serialize};

// ── Constants ───────────────────────────────────────────────────────────────

const DEFAULT_VISION_URL: &str = "http://127.0.0.1:8088";
const MAX_IMAGE_SIZE: usize = 10 * 1024 * 1024; // 10MB

/// Hosts allowed to use plaintext HTTP. Remote instances require HTTPS; the host
/// runtime separately narrows the WASM capability to the configured hostname.
const LOCAL_HTTP_HOSTS: &[&str] = &["127.0.0.1", "localhost", "host.containers.internal", "::1"];

// ── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct VisionRequest {
    /// Base64-encoded image, or a workspace file path.
    image: Option<String>,
    /// Workspace file path to read image from.
    file_path: Option<String>,
    /// Analysis mode: "text" (OCR), "describe" (VL), "auto" (smart routing).
    #[serde(default = "default_mode")]
    mode: String,
    /// Optional prompt for VL queries.
    #[serde(default)]
    prompt: Option<String>,
    /// OCR language (default: "eng").
    #[serde(default = "default_lang")]
    ocr_lang: String,
    /// Detail level for VL: "low", "medium", "high".
    #[serde(default = "default_detail")]
    detail_level: String,
    /// Vision service URL. Host configuration wins; remote instances require HTTPS.
    #[serde(default = "default_url")]
    service_url: String,
}

fn default_mode() -> String {
    "auto".to_string()
}
fn default_lang() -> String {
    "eng".to_string()
}
fn default_detail() -> String {
    "medium".to_string()
}
fn default_url() -> String {
    DEFAULT_VISION_URL.to_string()
}

#[derive(Debug, Serialize)]
struct SidecarRequest {
    image: String,
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    ocr_lang: String,
    detail_level: String,
}

#[derive(Debug, Serialize)]
struct OcrOnlyRequest {
    image: String,
    ocr_lang: String,
}

#[derive(Debug, Serialize)]
struct ToolOutput {
    status: String,
    endpoint: String,
    #[serde(flatten)]
    data: serde_json::Value,
}

// ── Tool Implementation ─────────────────────────────────────────────────────

struct VisionAnalyzeTool;

impl exports::lunarwing::agent::tool::Guest for VisionAnalyzeTool {
    fn execute(
        req: exports::lunarwing::agent::tool::Request,
    ) -> exports::lunarwing::agent::tool::Response {
        match execute_inner(&req.params, req.context.as_deref()) {
            Ok(output) => exports::lunarwing::agent::tool::Response {
                output: Some(output),
                error: None,
            },
            Err(e) => exports::lunarwing::agent::tool::Response {
                output: None,
                error: Some(e),
            },
        }
    }

    fn schema() -> String {
        SCHEMA.to_string()
    }

    fn description() -> String {
        "Analyze images using OCR and vision-language models. Extract text, describe scenes, \
         or answer questions about image content. Supports smart auto-routing between OCR and \
         vision backends. Accepts base64 images or workspace file paths."
            .to_string()
    }
}

fn execute_inner(params_json: &str, context_json: Option<&str>) -> Result<String, String> {
    let req: VisionRequest =
        serde_json::from_str(params_json).map_err(|e| format!("Invalid parameters: {e}"))?;

    // Resolve the effective service URL with host-wins precedence:
    //   1. host-injected via Request.context (JobContext.vision_service_url) — trusted,
    //      the LLM cannot redirect vision calls when the host provides a URL.
    //   2. LLM-provided via params (req.service_url) — used only if host didn't inject.
    //   3. default_url() (http://127.0.0.1:8088) — single-tenant fallback.
    let host_url: Option<String> = context_json
        .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
        .and_then(|v| {
            v.get("vision_service_url")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty());

    let effective_url: String = if let Some(h) = host_url.as_deref() {
        h.to_string()
    } else if !req.service_url.is_empty() {
        req.service_url.clone()
    } else {
        default_url()
    };

    // Validate the effective URL against the (loopback-only) allowlist
    let service_url = validate_service_url(&effective_url)?;

    // Get image data — either from direct base64 or workspace file
    let image_b64 = get_image_data(&req)?;

    // Validate image size (rough estimate from base64 length)
    let decoded_size = image_b64.len() * 3 / 4;
    if decoded_size > MAX_IMAGE_SIZE {
        return Err(format!(
            "Image too large: ~{} bytes (max {} bytes)",
            decoded_size, MAX_IMAGE_SIZE
        ));
    }

    // Validate mode
    match req.mode.as_str() {
        "text" | "describe" | "auto" => {}
        other => return Err(format!("Invalid mode '{other}'. Use: text, describe, auto")),
    }

    // Route to the appropriate endpoint
    let (endpoint, body_json) = if req.mode == "text" {
        let body = OcrOnlyRequest {
            image: image_b64,
            ocr_lang: req.ocr_lang,
        };
        (
            format!("{service_url}/v1/ocr"),
            serde_json::to_string(&body).map_err(|e| format!("Serialize error: {e}"))?,
        )
    } else {
        let body = SidecarRequest {
            image: image_b64,
            mode: req.mode,
            prompt: req.prompt,
            ocr_lang: req.ocr_lang,
            detail_level: req.detail_level,
        };
        (
            format!("{service_url}/v1/vision/analyze"),
            serde_json::to_string(&body).map_err(|e| format!("Serialize error: {e}"))?,
        )
    };

    // Single HTTP request — no retry
    let headers = r#"{"Content-Type": "application/json"}"#;
    let body_bytes = body_json.into_bytes();

    let response = lunarwing::agent::host::http_request(
        "POST",
        &endpoint,
        headers,
        Some(&body_bytes),
        Some(60000), // 60s timeout — VL inference can be slow
    )
    .map_err(|e| format!("Vision service request failed: {e}"))?;

    if response.status >= 200 && response.status < 300 {
        let body_str = String::from_utf8(response.body)
            .map_err(|_| "Vision service returned non-UTF8 response".to_string())?;

        let data: serde_json::Value = serde_json::from_str(&body_str)
            .map_err(|e| format!("Vision service returned invalid JSON: {e}"))?;

        let output = ToolOutput {
            status: "success".to_string(),
            endpoint,
            data,
        };

        serde_json::to_string(&output).map_err(|e| format!("Serialize output error: {e}"))
    } else {
        let body_str = String::from_utf8(response.body).unwrap_or_default();
        Err(format!(
            "Vision service returned HTTP {}: {}",
            response.status, body_str
        ))
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Validate a base URL before endpoint paths are appended. Plain HTTP remains
/// available for local sidecars; external instances must use HTTPS.
fn validate_service_url(input: &str) -> Result<String, String> {
    let url = input.trim().trim_end_matches('/');
    let (scheme, authority) = if let Some(authority) = url.strip_prefix("https://") {
        ("https", authority)
    } else if let Some(authority) = url.strip_prefix("http://") {
        ("http", authority)
    } else {
        return Err("Service URL must use https:// (or http:// for a local sidecar)".to_string());
    };
    if authority.contains(['/', '?', '#', '@']) {
        return Err(
            "Service URL must be an origin without credentials, path, query, or fragment"
                .to_string(),
        );
    }

    let host = authority_host(authority)?;
    let local_http = LOCAL_HTTP_HOSTS
        .iter()
        .any(|allowed| host.eq_ignore_ascii_case(allowed));
    if scheme == "http" && !local_http {
        return Err("Remote vision service URLs must use HTTPS".to_string());
    }
    Ok(url.to_string())
}

fn authority_host(authority: &str) -> Result<&str, String> {
    let (host, port) = if let Some(ipv6) = authority.strip_prefix('[') {
        let (host, suffix) = ipv6
            .split_once(']')
            .ok_or_else(|| "Service URL contains an invalid IPv6 authority".to_string())?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or_else(|| "Service URL contains an invalid IPv6 authority".to_string())?,
            )
        };
        (host, port)
    } else if authority.matches(':').count() > 1 {
        return Err("IPv6 service URL hosts must use brackets".to_string());
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        (host, Some(port))
    } else {
        (authority, None)
    };

    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err("Service URL must contain a valid host".to_string());
    }
    if let Some(port) = port {
        let port = port
            .parse::<u16>()
            .map_err(|_| "Service URL port must be between 1 and 65535".to_string())?;
        if port == 0 {
            return Err("Service URL port must be between 1 and 65535".to_string());
        }
    }
    Ok(host)
}

/// Get base64-encoded image data from either direct input or workspace file.
fn get_image_data(req: &VisionRequest) -> Result<String, String> {
    match (&req.image, &req.file_path) {
        (Some(b64), None) => {
            if b64.is_empty() {
                return Err("'image' field is empty".to_string());
            }
            Ok(b64.clone())
        }
        (None, Some(path)) => {
            let content = lunarwing::agent::host::workspace_read(path)
                .ok_or_else(|| format!("Could not read workspace file: {path}"))?;

            if content.is_empty() {
                return Err(format!("Workspace file is empty: {path}"));
            }
            Ok(content.trim().to_string())
        }
        (Some(_), Some(_)) => Err("Provide either 'image' or 'file_path', not both".to_string()),
        (None, None) => {
            Err("Provide either 'image' (base64) or 'file_path' (workspace path)".to_string())
        }
    }
}

export!(VisionAnalyzeTool);
const SCHEMA: &str = r#"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "title": "VisionAnalyzeParams",
  "description": "Parameters for analyzing images with OCR and vision-language models",
  "required": ["image"],
  "properties": {
    "image": {
      "type": "string",
      "description": "Base64-encoded image data or workspace file path (e.g., './screenshot.png' or '~/image.jpg')"
    },
    "mode": {
      "type": "string",
      "enum": ["text", "describe", "auto"],
      "default": "auto",
      "description": "Analysis mode: 'text' for OCR only, 'describe' for vision-language description, 'auto' for smart routing"
    },
    "prompt": {
      "type": "string",
      "description": "Custom question or prompt for vision analysis (e.g., 'What colors are in this image?')"
    },
    "ocr_lang": {
      "type": "string",
      "default": "eng",
      "description": "OCR language code (e.g., 'eng', 'fra', 'deu')"
    },
    "detail_level": {
      "type": "string",
      "enum": ["low", "medium", "high"],
      "default": "medium",
      "description": "Detail level for vision-language analysis (ignored in 'text' mode)"
    },
    "service_url": {
      "type": "string",
      "default": "http://127.0.0.1:8088",
      "description": "Vision service base URL. The host-configured URL takes precedence. Local sidecars may use HTTP; external instances must use HTTPS."
    }
  }
}"#;

#[cfg(test)]
mod tests {
    use super::validate_service_url;

    #[test]
    fn accepts_local_http_and_remote_https_origins() {
        for url in [
            "http://127.0.0.1:20015",
            "http://localhost:30000",
            "http://host.containers.internal:8088",
            "http://[::1]:8088",
            "https://vision.example.com",
            "https://vision.example.com:8443/",
            "https://192.0.2.10:8443",
        ] {
            assert!(validate_service_url(url).is_ok(), "should accept {url}");
        }
    }

    #[test]
    fn rejects_remote_http_and_non_origins() {
        for url in [
            "http://192.168.1.187:8080",
            "http://example.com:8088",
            "127.0.0.1:8088",
            "ftp://vision.example.com",
            "https://user:secret@vision.example.com",
            "https://vision.example.com/v1",
            "https://vision.example.com?tenant=one",
            "https://vision.example.com#fragment",
            "https://vision.example.com:0",
            "https://vision.example.com:65536",
            "https://[2001:db8::1",
        ] {
            assert!(validate_service_url(url).is_err(), "should reject {url}");
        }
    }
}
