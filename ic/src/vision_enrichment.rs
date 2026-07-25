//! Automatic LunarVision enrichment for inbound XMPP images.

use std::time::Duration;

use base64::Engine;
use futures::{StreamExt, stream};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

use crate::channels::{AttachmentKind, IncomingMessage};

const MAX_IMAGE_SIZE: usize = 10 * 1024 * 1024;
const MAX_RESPONSE_SIZE: usize = 1024 * 1024;
const MAX_EXTRACTED_TEXT_SIZE: usize = 100_000;
const MAX_CONCURRENT_ANALYSES: usize = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const ANALYSIS_PROMPT: &str =
    "Describe the image, including visible text and relevant visual context.";
const TRUNCATION_SUFFIX: &str = "\n\n[... LunarVision output truncated ...]";

/// Invalid LunarVision client configuration.
#[derive(Debug, thiserror::Error)]
pub enum VisionEnrichmentConfigError {
    #[error("VISION_SERVICE_URL is not a valid URL")]
    InvalidUrl,
    #[error("VISION_SERVICE_URL must use http://")]
    InvalidScheme,
    #[error("VISION_SERVICE_URL must not contain credentials, query parameters, or a fragment")]
    InvalidComponents,
    #[error("VISION_SERVICE_URL must point to localhost or a loopback IP address")]
    NonLoopbackHost,
    #[error("VISION_SERVICE_URL must not contain a path")]
    InvalidPath,
    #[error("failed to build LunarVision HTTP client: {0}")]
    Client(#[source] reqwest::Error),
}

#[derive(Debug, thiserror::Error)]
enum VisionEnrichmentError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("service returned HTTP {0}")]
    HttpStatus(u16),
    #[error("service response exceeded {MAX_RESPONSE_SIZE} bytes")]
    ResponseTooLarge,
    #[error("service returned invalid JSON: {0}")]
    InvalidResponse(#[from] serde_json::Error),
}

/// Enriches XMPP image attachments through a configured LunarVision sidecar.
///
/// Failures leave attachments unchanged so provider-native multimodal handling
/// remains available.
pub struct VisionEnrichmentMiddleware {
    client: reqwest::Client,
    endpoint: Url,
    auth_token: Option<SecretString>,
}

impl VisionEnrichmentMiddleware {
    /// Build middleware for a trusted, loopback-only LunarVision sidecar.
    pub fn new(
        service_url: &str,
        auth_token: Option<SecretString>,
    ) -> Result<Self, VisionEnrichmentConfigError> {
        let endpoint = vision_endpoint(service_url)?;
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(VisionEnrichmentConfigError::Client)?;

        Ok(Self {
            client,
            endpoint,
            auth_token,
        })
    }

    /// Analyze eligible XMPP images and store bounded text in `extracted_text`.
    pub async fn process(&self, message: &mut IncomingMessage) {
        if message.channel != "xmpp" {
            return;
        }

        let candidates = message
            .attachments
            .iter()
            .enumerate()
            .filter_map(|(index, attachment)| {
                if attachment.kind != AttachmentKind::Image
                    || attachment.data.is_empty()
                    || attachment.extracted_text.is_some()
                {
                    return None;
                }
                if attachment.data.len() > MAX_IMAGE_SIZE {
                    tracing::warn!(
                        attachment_id = %attachment.id,
                        size = attachment.data.len(),
                        max_size = MAX_IMAGE_SIZE,
                        "Image too large for LunarVision enrichment; preserving original image"
                    );
                    return None;
                }
                Some(index)
            })
            .collect::<Vec<_>>();

        let results = stream::iter(candidates)
            .map(|index| {
                let attachment = &message.attachments[index];
                async move {
                    (
                        index,
                        attachment.id.clone(),
                        self.analyze(&attachment.data).await,
                    )
                }
            })
            .buffer_unordered(MAX_CONCURRENT_ANALYSES)
            .collect::<Vec<_>>()
            .await;

        for (index, attachment_id, result) in results {
            match result {
                Ok(text) => {
                    tracing::info!(
                        attachment_id = %attachment_id,
                        text_len = text.len(),
                        "Enriched XMPP image with LunarVision"
                    );
                    message.attachments[index].extracted_text = Some(text);
                }
                Err(error) => {
                    tracing::warn!(
                        attachment_id = %attachment_id,
                        error = %error,
                        "LunarVision enrichment failed; preserving original image"
                    );
                }
            }
        }
    }

    async fn analyze(&self, image: &[u8]) -> Result<String, VisionEnrichmentError> {
        let image = base64::engine::general_purpose::STANDARD.encode(image);
        let body = AnalyzeRequest {
            image: &image,
            mode: "auto",
            prompt: ANALYSIS_PROMPT,
            ocr_lang: "eng",
            detail_level: "medium",
        };
        let mut request = self.client.post(self.endpoint.clone()).json(&body);
        if let Some(token) = &self.auth_token {
            request = request.bearer_auth(token.expose_secret());
        }

        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(VisionEnrichmentError::HttpStatus(
                response.status().as_u16(),
            ));
        }
        let body = read_response_capped(response).await?;
        let response: AnalyzeResponseEnvelope = serde_json::from_slice(&body)?;
        Ok(response.into_analysis().into_extracted_text())
    }
}

fn vision_endpoint(service_url: &str) -> Result<Url, VisionEnrichmentConfigError> {
    let mut url = Url::parse(service_url).map_err(|_| VisionEnrichmentConfigError::InvalidUrl)?;
    if url.scheme() != "http" {
        return Err(VisionEnrichmentConfigError::InvalidScheme);
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(VisionEnrichmentConfigError::InvalidComponents);
    }
    let is_loopback = match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if !is_loopback {
        return Err(VisionEnrichmentConfigError::NonLoopbackHost);
    }
    if url.path() != "/" {
        return Err(VisionEnrichmentConfigError::InvalidPath);
    }

    url.set_path("/v1/vision/analyze");
    Ok(url)
}

async fn read_response_capped(
    mut response: reqwest::Response,
) -> Result<Vec<u8>, VisionEnrichmentError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_SIZE as u64)
    {
        return Err(VisionEnrichmentError::ResponseTooLarge);
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_SIZE {
            return Err(VisionEnrichmentError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Serialize)]
struct AnalyzeRequest<'a> {
    image: &'a str,
    mode: &'static str,
    prompt: &'static str,
    ocr_lang: &'static str,
    detail_level: &'static str,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AnalyzeResponseEnvelope {
    Direct(AnalyzeResponse),
    Cached { response: AnalyzeResponse },
}

impl AnalyzeResponseEnvelope {
    fn into_analysis(self) -> AnalyzeResponse {
        match self {
            Self::Direct(response) | Self::Cached { response } => response,
        }
    }
}

#[derive(Deserialize)]
struct AnalyzeResponse {
    ocr: OcrResult,
    #[serde(default)]
    vision: Option<VisionResult>,
}

impl AnalyzeResponse {
    fn into_extracted_text(self) -> String {
        let mut sections = Vec::new();
        let ocr = self.ocr.full_text.trim();
        if !ocr.is_empty() {
            sections.push(format!("OCR text:\n{ocr}"));
        }
        if let Some(vision) = self.vision {
            let description = vision.description.trim();
            if !description.is_empty() {
                sections.push(format!("Visual description:\n{description}"));
            }
            if let Some(answer) = vision.prompt_answer {
                let answer = answer.trim();
                if !answer.is_empty() && answer != description {
                    sections.push(format!("Answer to user prompt:\n{answer}"));
                }
            }
        }

        let text = if sections.is_empty() {
            "No readable text or visual description was returned.".to_string()
        } else {
            sections.join("\n\n")
        };
        truncate_extracted_text(text)
    }
}

#[derive(Deserialize)]
struct OcrResult {
    full_text: String,
}

#[derive(Deserialize)]
struct VisionResult {
    description: String,
    #[serde(default)]
    prompt_answer: Option<String>,
}

fn truncate_extracted_text(mut text: String) -> String {
    if text.len() <= MAX_EXTRACTED_TEXT_SIZE {
        return text;
    }
    let content_limit = MAX_EXTRACTED_TEXT_SIZE.saturating_sub(TRUNCATION_SUFFIX.len());
    let boundary = text
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= content_limit)
        .last()
        .unwrap_or(0);
    text.truncate(boundary);
    text.push_str(TRUNCATION_SUFFIX);
    text
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{Value, json};

    use super::*;
    use crate::channels::IncomingAttachment;

    #[derive(Clone)]
    struct MockState {
        requests: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
        status: StatusCode,
        response: Value,
    }

    async fn analyze_handler(
        State(state): State<MockState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        state
            .requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((headers, body));
        (state.status, Json(state.response))
    }

    async fn mock_server(
        status: StatusCode,
        response: Value,
    ) -> (
        String,
        Arc<Mutex<Vec<(HeaderMap, Value)>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = MockState {
            requests: Arc::clone(&requests),
            status,
            response,
        };
        let app = Router::new()
            .route("/v1/vision/analyze", post(analyze_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock LunarVision server");
        let address = listener.local_addr().expect("read mock server address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("run mock LunarVision server");
        });
        (format!("http://{address}"), requests, task)
    }

    fn image_attachment(data: Vec<u8>) -> IncomingAttachment {
        IncomingAttachment {
            id: "image-1".to_string(),
            kind: AttachmentKind::Image,
            mime_type: "image/png".to_string(),
            filename: Some("screen.png".to_string()),
            size_bytes: Some(data.len() as u64),
            source_url: Some("aesgcm://files.example/screen.png#secret-key".to_string()),
            storage_key: None,
            extracted_text: None,
            data,
            duration_secs: None,
        }
    }

    #[tokio::test]
    async fn enriches_xmpp_image_with_authenticated_auto_analysis() {
        let response = json!({
            "mode_used": "hybrid",
            "ocr": {"full_text": "Build failed on line 7"},
            "vision": {
                "description": "Terminal screenshot showing a compiler error",
                "prompt_answer": "The build failed because symbol x is missing"
            },
            "meta": {"backends_used": ["tesseract", "qwen3vl"], "latency_ms": 12}
        });
        let (url, requests, server) = mock_server(StatusCode::OK, response).await;
        let middleware = VisionEnrichmentMiddleware::new(
            &url,
            Some(SecretString::from("vision-secret".to_string())),
        )
        .expect("build middleware");
        let mut message = IncomingMessage::new(
            "xmpp",
            "user",
            "What caused this error? token-must-not-reach-sidecar",
        )
        .with_attachments(vec![image_attachment(vec![1, 2, 3])]);

        middleware.process(&mut message).await;

        let extracted = message.attachments[0]
            .extracted_text
            .as_deref()
            .expect("image should be enriched");
        assert!(extracted.contains("Build failed on line 7"));
        assert!(extracted.contains("Terminal screenshot"));
        assert!(extracted.contains("symbol x is missing"));
        assert_eq!(message.attachments[0].data, vec![1, 2, 3]);
        assert!(!extracted.contains("vision-secret"));
        assert!(!extracted.contains("aesgcm://"));

        let requests = requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]
                .0
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer vision-secret")
        );
        assert_eq!(requests[0].1["image"], "AQID");
        assert_eq!(requests[0].1["mode"], "auto");
        assert_eq!(requests[0].1["prompt"], ANALYSIS_PROMPT);
        assert!(!requests[0].1.to_string().contains("token-must-not"));
        server.abort();
    }

    #[tokio::test]
    async fn service_failure_preserves_multimodal_fallback() {
        let (url, requests, server) = mock_server(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "failed"}),
        )
        .await;
        let middleware = VisionEnrichmentMiddleware::new(&url, None).expect("build middleware");
        let mut message = IncomingMessage::new("xmpp", "user", "look")
            .with_attachments(vec![image_attachment(vec![4, 5, 6])]);

        middleware.process(&mut message).await;

        assert!(message.attachments[0].extracted_text.is_none());
        assert_eq!(message.attachments[0].data, vec![4, 5, 6]);
        assert_eq!(
            requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn ignores_non_xmpp_images() {
        let (url, requests, server) = mock_server(StatusCode::OK, json!({})).await;
        let middleware = VisionEnrichmentMiddleware::new(&url, None).expect("build middleware");
        let mut message = IncomingMessage::new("gateway", "user", "look")
            .with_attachments(vec![image_attachment(vec![1])]);

        middleware.process(&mut message).await;

        assert!(message.attachments[0].extracted_text.is_none());
        assert!(
            requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty()
        );
        server.abort();
    }

    #[tokio::test]
    async fn oversized_image_skips_enrichment_without_losing_bytes() {
        let (url, requests, server) = mock_server(StatusCode::OK, json!({})).await;
        let middleware = VisionEnrichmentMiddleware::new(&url, None).expect("build middleware");
        let data = vec![0; MAX_IMAGE_SIZE + 1];
        let mut message = IncomingMessage::new("xmpp", "user", "look")
            .with_attachments(vec![image_attachment(data)]);

        middleware.process(&mut message).await;

        assert!(message.attachments[0].extracted_text.is_none());
        assert_eq!(message.attachments[0].data.len(), MAX_IMAGE_SIZE + 1);
        assert!(
            requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty()
        );
        server.abort();
    }

    #[tokio::test]
    async fn oversized_sidecar_response_fails_open() {
        let response = json!({
            "ocr": {"full_text": "x".repeat(MAX_RESPONSE_SIZE)},
            "vision": null
        });
        let (url, requests, server) = mock_server(StatusCode::OK, response).await;
        let middleware = VisionEnrichmentMiddleware::new(&url, None).expect("build middleware");
        let mut message = IncomingMessage::new("xmpp", "user", "look")
            .with_attachments(vec![image_attachment(vec![1, 2, 3])]);

        middleware.process(&mut message).await;

        assert!(message.attachments[0].extracted_text.is_none());
        assert_eq!(message.attachments[0].data, vec![1, 2, 3]);
        assert_eq!(
            requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            1
        );
        server.abort();
    }

    #[test]
    fn accepts_cached_sidecar_response_shape() {
        let response: AnalyzeResponseEnvelope = serde_json::from_value(json!({
            "cached": true,
            "response": {
                "mode_used": "text",
                "ocr": {"full_text": "cached OCR"},
                "vision": null,
                "meta": {"backends_used": ["tesseract"], "latency_ms": 1}
            }
        }))
        .expect("parse cached response");

        assert!(
            response
                .into_analysis()
                .into_extracted_text()
                .contains("cached OCR")
        );
    }

    #[test]
    fn endpoint_rejects_non_loopback_and_paths() {
        assert!(matches!(
            vision_endpoint("http://example.com:8088"),
            Err(VisionEnrichmentConfigError::NonLoopbackHost)
        ));
        assert!(matches!(
            vision_endpoint("https://127.0.0.1:8088"),
            Err(VisionEnrichmentConfigError::InvalidScheme)
        ));
        assert!(matches!(
            vision_endpoint("http://127.0.0.1:8088/custom"),
            Err(VisionEnrichmentConfigError::InvalidPath)
        ));
        assert_eq!(
            vision_endpoint("http://[::1]:8088")
                .expect("IPv6 loopback should be accepted")
                .as_str(),
            "http://[::1]:8088/v1/vision/analyze"
        );
    }

    #[test]
    fn extracted_text_limit_includes_truncation_marker() {
        let text = truncate_extracted_text("x".repeat(MAX_EXTRACTED_TEXT_SIZE + 1));
        assert!(text.ends_with(TRUNCATION_SUFFIX));
        assert!(text.len() <= MAX_EXTRACTED_TEXT_SIZE);
    }
}
