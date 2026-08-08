//! Sandboxed client for the loopback LunarVoice sidecar.

wit_bindgen::generate!({
    world: "sandboxed-tool",
    path: "../../wit/tool.wit",
});

use serde::{Deserialize, Serialize};

const DEFAULT_VOICE_URL: &str = "http://127.0.0.1:8090";
const MAX_AUDIO_SIZE: usize = 50 * 1024 * 1024;
const ALLOWED_HOSTS: &[&str] = &["127.0.0.1", "localhost", "host.containers.internal", "::1"];

#[derive(Debug, Deserialize)]
struct VoiceRequest {
    audio: String,
    #[serde(default = "default_format")]
    format: String,
    #[serde(default = "default_language")]
    language: String,
    #[serde(default)]
    model: String,
    #[serde(default = "default_url")]
    service_url: String,
}

#[derive(Debug, Serialize)]
struct SidecarRequest<'a> {
    audio: &'a str,
    format: &'a str,
    language: &'a str,
    model: &'a str,
}

#[derive(Debug, Serialize)]
struct ToolOutput {
    status: &'static str,
    transcription: serde_json::Value,
}

struct VoiceTranscribeTool;

impl exports::lunarwing::agent::tool::Guest for VoiceTranscribeTool {
    fn execute(
        request: exports::lunarwing::agent::tool::Request,
    ) -> exports::lunarwing::agent::tool::Response {
        match execute_inner(&request.params, request.context.as_deref()) {
            Ok(output) => exports::lunarwing::agent::tool::Response {
                output: Some(output),
                error: None,
            },
            Err(error) => exports::lunarwing::agent::tool::Response {
                output: None,
                error: Some(error),
            },
        }
    }

    fn schema() -> String {
        SCHEMA.to_string()
    }

    fn description() -> String {
        "Transcribe base64 audio locally with the LunarVoice ONNX sidecar. Supports WAV, Ogg/Opus, MP3, MP4, WebM, FLAC, M4A, and AAC input."
            .to_string()
    }
}

fn execute_inner(params_json: &str, context_json: Option<&str>) -> Result<String, String> {
    let request: VoiceRequest = serde_json::from_str(params_json)
        .map_err(|error| format!("Invalid parameters: {error}"))?;
    if request.audio.is_empty() {
        return Err("'audio' field is empty".to_string());
    }
    let estimated_size = request.audio.len().saturating_mul(3) / 4;
    if estimated_size > MAX_AUDIO_SIZE {
        return Err(format!(
            "Audio too large: approximately {estimated_size} bytes (max {MAX_AUDIO_SIZE})"
        ));
    }
    validate_format(&request.format)?;

    let host_url = context_json
        .and_then(|context| serde_json::from_str::<serde_json::Value>(context).ok())
        .and_then(|value| {
            value
                .get("voice_service_url")
                .and_then(|url| url.as_str())
                .map(str::to_string)
        })
        .filter(|url| !url.is_empty());
    let effective_url = host_url.as_deref().unwrap_or(&request.service_url);
    let service_url = validate_service_url(effective_url)?;
    let endpoint = format!("{service_url}/v1/voice/transcribe");
    let body = serde_json::to_vec(&SidecarRequest {
        audio: &request.audio,
        format: &request.format,
        language: &request.language,
        model: &request.model,
    })
    .map_err(|error| format!("Serialize error: {error}"))?;

    let response = lunarwing::agent::host::http_request(
        "POST",
        &endpoint,
        r#"{"Content-Type":"application/json"}"#,
        Some(&body),
        Some(120_000),
    )
    .map_err(|error| format!("LunarVoice request failed: {error}"))?;
    if !(200..300).contains(&response.status) {
        return Err(format!("LunarVoice returned HTTP {}", response.status));
    }
    let body = String::from_utf8(response.body)
        .map_err(|_| "LunarVoice returned non-UTF8 data".to_string())?;
    let transcription: serde_json::Value = serde_json::from_str(&body)
        .map_err(|error| format!("LunarVoice returned invalid JSON: {error}"))?;
    serde_json::to_string(&ToolOutput {
        status: "success",
        transcription,
    })
    .map_err(|error| format!("Serialize output error: {error}"))
}

fn validate_format(format: &str) -> Result<(), String> {
    match format {
        "wav" | "ogg" | "opus" | "mp3" | "mp4" | "webm" | "flac" | "m4a" | "aac" => Ok(()),
        _ => Err(format!("Unsupported audio format: {format}")),
    }
}

fn validate_service_url(url: &str) -> Result<String, String> {
    let url = url.trim_end_matches('/');
    if url.contains('?') || url.contains('#') {
        return Err("LunarVoice URL must not contain a query or fragment".to_string());
    }
    let authority = url
        .strip_prefix("http://")
        .ok_or_else(|| "LunarVoice URL must use http://".to_string())?
        .split('/')
        .next()
        .unwrap_or_default();
    let host = if let Some(ipv6) = authority.strip_prefix('[') {
        ipv6.split(']').next().unwrap_or_default()
    } else {
        authority
            .rsplit_once(':')
            .map(|(host, _)| host)
            .unwrap_or(authority)
    };
    if !ALLOWED_HOSTS.contains(&host) {
        return Err(format!("LunarVoice URL host '{host}' is not loopback"));
    }
    if authority.is_empty() || url["http://".len()..].contains('/') {
        return Err("LunarVoice URL must be a base URL without a path".to_string());
    }
    Ok(url.to_string())
}

fn default_format() -> String {
    "wav".to_string()
}

fn default_language() -> String {
    "auto".to_string()
}

fn default_url() -> String {
    DEFAULT_VOICE_URL.to_string()
}

export!(VoiceTranscribeTool);

const SCHEMA: &str = r#"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "title": "VoiceTranscribeParams",
  "required": ["audio"],
  "properties": {
    "audio": {
      "type": "string",
      "description": "Standard base64-encoded audio bytes"
    },
    "format": {
      "type": "string",
      "enum": ["wav", "ogg", "opus", "mp3", "mp4", "webm", "flac", "m4a", "aac"],
      "default": "wav"
    },
    "language": {
      "type": "string",
      "default": "auto"
    },
    "model": {
      "type": "string",
      "description": "Loaded LunarVoice STT model name; omit for the tenant default"
    },
    "service_url": {
      "type": "string",
      "default": "http://127.0.0.1:8090",
      "description": "Loopback LunarVoice base URL. Host-injected tenant URL takes precedence."
    }
  }
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_url_validation_is_strict() {
        assert!(validate_service_url("http://127.0.0.1:8090").is_ok());
        assert!(validate_service_url("http://localhost:18090/").is_ok());
        assert!(validate_service_url("http://[::1]:8090").is_ok());
        assert!(validate_service_url("https://127.0.0.1:8090").is_err());
        assert!(validate_service_url("http://10.0.0.1:8090").is_err());
        assert!(validate_service_url("http://127.0.0.1:8090/path").is_err());
    }

    #[test]
    fn formats_are_bounded_to_supported_decoder_inputs() {
        assert!(validate_format("opus").is_ok());
        assert!(validate_format("exe").is_err());
    }
}
