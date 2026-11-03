//! Image generation tool using cloud API.
//!
//! The image tools (`image_generate`, `image_edit`) return a sentinel JSON object
//! `{"type":"image_generated","data":"data:image/png;base64,…",…}`;
//! [`parse_image_sentinel`] is the one reader of it for Engine V1 and V2 (TUI design
//! 8.3.6).

use std::time::Duration;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::JobContext;
use crate::tools::{Tool, ToolError, ToolOutput};

/// Largest data URL [`parse_image_sentinel`] accepts (the TUI's limit too).
pub(crate) const MAX_GENERATED_IMAGE_DATA_URL_BYTES: usize = 16 * 1024 * 1024;

/// Room for the sentinel's other fields and its JSON escaping. Larger outputs are not
/// parsed at all.
const MAX_SENTINEL_OVERHEAD_BYTES: usize = 1024 * 1024;

/// The data URL prefixes a sentinel may carry, compared ASCII case-insensitively.
const IMAGE_DATA_URL_PREFIXES: [&str; 4] = [
    "data:image/png;base64,",
    "data:image/jpeg;base64,",
    "data:image/gif;base64,",
    "data:image/webp;base64,",
];

/// An image an image tool returned, ready for `StatusUpdate::ImageGenerated`.
///
/// `Debug` prints sizes only: the data URL can be megabytes of base64.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GeneratedImage {
    /// `data:image/{png|jpeg|gif|webp};base64,…`, prefix in lowercase.
    pub data_url: String,
    /// The file path the tool reported, if any.
    pub path: Option<String>,
}

impl std::fmt::Debug for GeneratedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneratedImage")
            .field("data_url_bytes", &self.data_url.len())
            .field("path_bytes", &self.path.as_ref().map(String::len))
            .finish()
    }
}

/// Whether `tool_name` returns the image sentinel: `image_generate` or `image_edit`,
/// hyphenated spellings included.
pub(crate) fn is_image_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name.replace('-', "_").as_str(),
        "image_generate" | "image_edit"
    )
}

/// Reads the image a successful image-tool call returned (TUI design 8.3.6).
///
/// Only `image_generate` and `image_edit` qualify, and only a sentinel object with
/// `type == "image_generated"`. The tools return it as text, which the tool pipeline
/// serializes as a JSON string, so one level of string encoding is unwrapped. `data`
/// must be a PNG, JPEG, GIF or WebP base64 data URL of at most
/// [`MAX_GENERATED_IMAGE_DATA_URL_BYTES`] (its prefix is returned in lowercase);
/// `path` is kept when it is a string. Anything else yields `None`.
pub(crate) fn parse_image_sentinel(tool_name: &str, output: &str) -> Option<GeneratedImage> {
    if !is_image_tool_name(tool_name)
        || output.len() > MAX_GENERATED_IMAGE_DATA_URL_BYTES + MAX_SENTINEL_OVERHEAD_BYTES
    {
        return None;
    }
    let mut sentinel = sentinel_object(output)?;
    if sentinel.get("type").and_then(Value::as_str) != Some("image_generated") {
        return None;
    }
    let data_url = match sentinel.remove("data") {
        Some(Value::String(data_url)) => image_data_url(data_url),
        _ => None,
    };
    let Some(data_url) = data_url else {
        tracing::warn!(
            tool = tool_name,
            "Image sentinel has no allowed image data URL; not shown as an image"
        );
        return None;
    };
    let path = match sentinel.remove("path") {
        Some(Value::String(path)) => Some(path),
        _ => None,
    };
    Some(GeneratedImage { data_url, path })
}

/// The sentinel object, given directly or wrapped in one JSON string.
fn sentinel_object(output: &str) -> Option<serde_json::Map<String, Value>> {
    match serde_json::from_str(output).ok()? {
        Value::Object(sentinel) => Some(sentinel),
        Value::String(inner) => match serde_json::from_str(&inner).ok()? {
            Value::Object(sentinel) => Some(sentinel),
            _ => None,
        },
        _ => None,
    }
}

/// `data_url` with its prefix lowercased, when it is an allowed, non-empty image data
/// URL within the size limit.
fn image_data_url(mut data_url: String) -> Option<String> {
    let prefix_len = IMAGE_DATA_URL_PREFIXES
        .iter()
        .find(|prefix| {
            data_url
                .get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })?
        .len();
    if data_url.len() <= prefix_len || data_url.len() > MAX_GENERATED_IMAGE_DATA_URL_BYTES {
        return None;
    }
    // The matched prefix is ASCII, so this range is always on a char boundary.
    if let Some(prefix) = data_url.get_mut(..prefix_len) {
        prefix.make_ascii_lowercase();
    }
    Some(data_url)
}

/// The sentinel output of a generated PNG (base64 `image_b64`), as the image tools
/// return it.
pub(crate) fn image_sentinel_output(
    image_b64: &str,
    prompt: &str,
    size: &str,
    elapsed: Duration,
) -> ToolOutput {
    let sentinel = serde_json::json!({
        "type": "image_generated",
        "data": format!("data:image/png;base64,{image_b64}"),
        "media_type": "image/png",
        "prompt": prompt,
        "size": size
    });
    ToolOutput::text(sentinel.to_string(), elapsed)
}

/// Tool for generating images using FLUX or compatible image generation APIs.
pub struct ImageGenerateTool {
    /// API base URL (e.g., "https://lunarwing.org").
    api_base_url: String,
    /// Bearer token for API auth.
    api_key: SecretString,
    /// Model to use (e.g., "black-forest-labs/FLUX.1-schnell").
    model: String,
    /// HTTP client.
    client: reqwest::Client,
}

#[derive(Debug, Serialize)]
struct ImageGenRequest {
    model: String,
    prompt: String,
    size: String,
    response_format: String,
    n: u32,
}

#[derive(Debug, Deserialize)]
struct ImageGenResponse {
    data: Vec<ImageGenData>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ImageGenData {
    b64_json: Option<String>,
    url: Option<String>,
}

impl ImageGenerateTool {
    /// Create a new image generation tool.
    pub fn new(api_base_url: String, api_key: String, model: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .unwrap_or_default();
        Self {
            api_base_url,
            api_key: SecretString::from(api_key),
            model,
            client,
        }
    }
}

#[async_trait]
impl Tool for ImageGenerateTool {
    fn name(&self) -> &str {
        "image_generate"
    }

    fn description(&self) -> &str {
        "Generate an image from a text prompt using an AI image generation model (e.g., FLUX). Returns the generated image data."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text description of the image to generate (max 4000 chars)",
                    "maxLength": 4000
                },
                "size": {
                    "type": "string",
                    "description": "Image dimensions",
                    "enum": ["1024x1024", "1792x1024", "1024x1792"],
                    "default": "1024x1024"
                }
            },
            "required": ["prompt"]
        })
    }

    fn requires_sanitization(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let prompt = params
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("Missing required 'prompt' parameter".to_string())
            })?;

        if prompt.len() > 4000 {
            return Err(ToolError::InvalidParameters(
                "Prompt exceeds 4000 character limit".to_string(),
            ));
        }

        let size = params
            .get("size")
            .and_then(|v| v.as_str())
            .unwrap_or("1024x1024");

        // Validate size
        if !["1024x1024", "1792x1024", "1024x1792"].contains(&size) {
            return Err(ToolError::InvalidParameters(format!(
                "Invalid size '{}'. Must be 1024x1024, 1792x1024, or 1024x1792",
                size
            )));
        }

        let url = format!(
            "{}/v1/images/generations",
            self.api_base_url.trim_end_matches('/')
        );

        let request_body = ImageGenRequest {
            model: self.model.clone(),
            prompt: prompt.to_string(),
            size: size.to_string(),
            response_format: "b64_json".to_string(),
            n: 1,
        };

        let response = self
            .client
            .post(&url)
            .bearer_auth(self.api_key.expose_secret())
            .json(&request_body)
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Image generation request failed: {e}"))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "Image generation API returned {status}: {body}"
            )));
        }

        let gen_response: ImageGenResponse = response.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse image generation response: {e}"))
        })?;

        let image_data = gen_response
            .data
            .first()
            .and_then(|d| d.b64_json.as_deref())
            .ok_or_else(|| ToolError::ExecutionFailed("No image data in response".to_string()))?;

        // Return sentinel JSON for image display
        Ok(image_sentinel_output(
            image_data,
            prompt,
            size,
            start.elapsed(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tool::ApprovalRequirement;

    #[test]
    fn test_tool_metadata() {
        let tool = ImageGenerateTool::new(
            "https://api.example.com".to_string(),
            "test-key".to_string(),
            "flux-1".to_string(),
        );
        assert_eq!(tool.name(), "image_generate");
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::Never
        );

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["prompt"].is_object());
        assert!(schema["properties"]["size"].is_object());
    }

    #[tokio::test]
    async fn test_missing_prompt() {
        let tool = ImageGenerateTool::new(
            "https://api.example.com".to_string(),
            "test-key".to_string(),
            "flux-1".to_string(),
        );
        let ctx = JobContext::default();
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_invalid_size() {
        let tool = ImageGenerateTool::new(
            "https://api.example.com".to_string(),
            "test-key".to_string(),
            "flux-1".to_string(),
        );
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({"prompt": "a cat", "size": "999x999"}),
                &ctx,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_prompt_too_long() {
        let tool = ImageGenerateTool::new(
            "https://api.example.com".to_string(),
            "test-key".to_string(),
            "flux-1".to_string(),
        );
        let ctx = JobContext::default();
        let long_prompt = "x".repeat(4001);
        let result = tool
            .execute(serde_json::json!({"prompt": long_prompt}), &ctx)
            .await;
        assert!(result.is_err());
    }

    /// A 1x1 PNG.
    const TINY_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn sentinel_with(data: Value) -> String {
        serde_json::json!({
            "type": "image_generated",
            "data": data,
            "media_type": "image/png",
            "prompt": "a cat",
        })
        .to_string()
    }

    fn png_data_url() -> String {
        format!("data:image/png;base64,{TINY_PNG_B64}")
    }

    /// How the tool pipeline (`execute_tool_with_safety`) renders a text output.
    fn as_pipeline_text(output: &str) -> String {
        serde_json::to_string_pretty(&Value::String(output.to_string())).unwrap()
    }

    #[test]
    fn parse_image_sentinel_accepts_png_data_url() {
        let sentinel = sentinel_with(Value::String(png_data_url()));
        let image = parse_image_sentinel("image_generate", &sentinel).expect("PNG sentinel");
        assert_eq!(image.data_url, png_data_url());
        assert_eq!(image.path, None);
        // The tool pipeline wraps the text output in a JSON string; the hyphenated
        // spelling of the tool name is the same tool.
        let wrapped = as_pipeline_text(&sentinel);
        assert_eq!(
            parse_image_sentinel("image-generate", &wrapped),
            Some(image.clone())
        );
        // `image_edit` may name a path; a non-string path is ignored.
        let with_path = serde_json::json!({
            "type": "image_generated", "data": png_data_url(), "path": "out/cat.png"
        });
        let edited = parse_image_sentinel("image_edit", &with_path.to_string()).expect("edit");
        assert_eq!(edited.path.as_deref(), Some("out/cat.png"));
        let numeric_path = serde_json::json!({
            "type": "image_generated", "data": png_data_url(), "path": 7
        });
        assert_eq!(
            parse_image_sentinel("image_edit", &numeric_path.to_string()),
            Some(image)
        );
        // The other allowed types; the prefix is compared case-insensitively and
        // returned in lowercase, as the TUI expects.
        for mime in ["jpeg", "gif", "webp"] {
            let url = format!("data:image/{mime};base64,AAAA");
            let parsed =
                parse_image_sentinel("image_generate", &sentinel_with(Value::String(url.clone())));
            assert_eq!(parsed.map(|image| image.data_url), Some(url));
        }
        let shouted = sentinel_with(Value::String(format!(
            "DATA:Image/PNG;BASE64,{TINY_PNG_B64}"
        )));
        assert_eq!(
            parse_image_sentinel("image_generate", &shouted).map(|image| image.data_url),
            Some(png_data_url())
        );
    }

    #[test]
    fn parse_image_sentinel_rejects_other_tools() {
        let sentinel = sentinel_with(Value::String(png_data_url()));
        for tool in [
            "shell",
            "http",
            "image_analyze",
            "image_generator",
            "image",
            "",
        ] {
            assert_eq!(parse_image_sentinel(tool, &sentinel), None, "{tool}");
            assert_eq!(
                parse_image_sentinel(tool, &as_pipeline_text(&sentinel)),
                None,
                "{tool}"
            );
        }
        // Not a sentinel: another type, not JSON, not an object.
        let other_type = serde_json::json!({"type": "text", "data": png_data_url()}).to_string();
        let not_object = serde_json::json!([png_data_url()]).to_string();
        for output in [
            other_type.as_str(),
            "not json",
            not_object.as_str(),
            "\"plain text\"",
        ] {
            assert_eq!(
                parse_image_sentinel("image_generate", output),
                None,
                "{output}"
            );
        }
    }

    #[test]
    fn parse_image_sentinel_rejects_svg_and_oversize() {
        let rejected = [
            Value::String("data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=".into()),
            Value::String("data:image/bmp;base64,Qk0=".into()),
            Value::String("data:image/tiff;base64,SUkqAA==".into()),
            Value::String("data:text/html;base64,PGI+aGk8L2I+".into()),
            Value::String(format!("data:image/png,{TINY_PNG_B64}")),
            Value::String("https://example.com/cat.png".into()),
            // A prefix with no image data.
            Value::String("data:image/png;base64,".into()),
            Value::Null,
            Value::from(42),
        ];
        for data in rejected {
            assert_eq!(
                parse_image_sentinel("image_generate", &sentinel_with(data.clone())),
                None,
                "{data}"
            );
        }
        let missing_data = serde_json::json!({"type": "image_generated"}).to_string();
        assert_eq!(parse_image_sentinel("image_generate", &missing_data), None);

        // Exactly at the cap is accepted, one byte more is not.
        let prefix = "data:image/png;base64,";
        let at_cap = format!(
            "{prefix}{}",
            "A".repeat(MAX_GENERATED_IMAGE_DATA_URL_BYTES - prefix.len())
        );
        let parsed = parse_image_sentinel("image_generate", &sentinel_with(Value::String(at_cap)));
        assert_eq!(
            parsed.map(|image| image.data_url.len()),
            Some(MAX_GENERATED_IMAGE_DATA_URL_BYTES)
        );
        let over_cap = format!(
            "{prefix}{}",
            "A".repeat(MAX_GENERATED_IMAGE_DATA_URL_BYTES + 1 - prefix.len())
        );
        assert_eq!(
            parse_image_sentinel("image_generate", &sentinel_with(Value::String(over_cap))),
            None
        );
        // An output far beyond the cap is refused before it is parsed.
        let huge = "x".repeat(MAX_GENERATED_IMAGE_DATA_URL_BYTES + MAX_SENTINEL_OVERHEAD_BYTES + 1);
        assert_eq!(parse_image_sentinel("image_generate", &huge), None);
    }

    /// Returns the real tool's sentinel without calling an image API.
    struct SentinelTool;

    #[async_trait]
    impl Tool for SentinelTool {
        fn name(&self) -> &str {
            "image_generate"
        }

        fn description(&self) -> &str {
            "Returns the image_generate sentinel"
        }

        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {"prompt": {"type": "string"}}})
        }

        async fn execute(
            &self,
            _params: Value,
            _ctx: &JobContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(image_sentinel_output(
                TINY_PNG_B64,
                "a cat",
                "1024x1024",
                Duration::ZERO,
            ))
        }
    }

    /// The output V1 and V2 actually hand to the parser comes out of the tool pipeline.
    #[tokio::test]
    async fn parse_image_sentinel_reads_the_tool_pipeline_output() {
        let registry = crate::tools::ToolRegistry::new();
        registry.register(std::sync::Arc::new(SentinelTool)).await;
        let safety = crate::safety::SafetyLayer::new(&crate::config::SafetyConfig {
            max_output_length: 100_000,
            injection_check_enabled: false,
        });
        let output = crate::tools::execute::execute_tool_with_safety(
            &registry,
            &safety,
            "image_generate",
            serde_json::json!({"prompt": "a cat"}),
            &JobContext::default(),
        )
        .await
        .expect("sentinel tool runs");
        // The pipeline renders a text output as a JSON string, not as the sentinel
        // object, so a parser that expects the object never matches real tool output.
        assert!(
            matches!(serde_json::from_str::<Value>(&output), Ok(Value::String(_))),
            "{output}"
        );
        let image = parse_image_sentinel("image_generate", &output).expect("pipeline sentinel");
        assert_eq!(image.data_url, png_data_url());
        assert_eq!(image.path, None);
    }

    #[test]
    fn generated_image_debug_omits_the_data_url() {
        let image = GeneratedImage {
            data_url: png_data_url(),
            path: Some("secret-name.png".into()),
        };
        let debug = format!("{image:?}");
        assert!(!debug.contains(TINY_PNG_B64), "{debug}");
        assert!(!debug.contains("secret-name"), "{debug}");
        assert!(debug.contains(&png_data_url().len().to_string()), "{debug}");
    }
}
