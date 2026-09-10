use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

use super::{AudioFormat, TranscriptionError, TranscriptionProvider};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum LunarVoiceConfigError {
    #[error("VOICE_SERVICE_URL is not a valid URL")]
    InvalidUrl,
    #[error("VOICE_SERVICE_URL must use http://")]
    InvalidScheme,
    #[error("VOICE_SERVICE_URL must not contain credentials, query parameters, or a fragment")]
    InvalidComponents,
    #[error("VOICE_SERVICE_URL must point to localhost or a loopback IP address")]
    NonLoopbackHost,
    #[error("VOICE_SERVICE_URL must not contain a path")]
    InvalidPath,
    #[error("failed to build LunarVoice HTTP client: {0}")]
    Client(#[source] reqwest::Error),
}

pub struct LunarVoiceTranscriptionProvider {
    client: reqwest::Client,
    endpoint: Url,
    auth_token: SecretString,
    model: String,
}

impl LunarVoiceTranscriptionProvider {
    pub fn new(service_url: &str, auth_token: SecretString) -> Result<Self, LunarVoiceConfigError> {
        let endpoint = transcription_endpoint(service_url)?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(120))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(LunarVoiceConfigError::Client)?;
        Ok(Self {
            client,
            endpoint,
            auth_token,
            model: "whisper-tiny".to_string(),
        })
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }
}

#[async_trait]
impl TranscriptionProvider for LunarVoiceTranscriptionProvider {
    async fn transcribe(
        &self,
        audio_data: &[u8],
        format: AudioFormat,
    ) -> Result<String, TranscriptionError> {
        if audio_data.is_empty() {
            return Err(TranscriptionError::EmptyAudio);
        }
        let audio = base64::engine::general_purpose::STANDARD.encode(audio_data);
        let body = TranscribeRequest {
            audio: &audio,
            format: format.extension(),
            language: "auto",
            model: &self.model,
        };
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(self.auth_token.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(|error| TranscriptionError::RequestFailed(error.to_string()))?;
        if !response.status().is_success() {
            return Err(TranscriptionError::RequestFailed(format!(
                "LunarVoice returned HTTP {}",
                response.status().as_u16()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(TranscriptionError::RequestFailed(
                "LunarVoice response exceeded limit".into(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| TranscriptionError::RequestFailed(error.to_string()))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(TranscriptionError::RequestFailed(
                    "LunarVoice response exceeded limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let response: TranscribeResponse = serde_json::from_slice(&bytes)
            .map_err(|error| TranscriptionError::RequestFailed(error.to_string()))?;
        Ok(response.text)
    }
}

#[derive(Serialize)]
struct TranscribeRequest<'a> {
    audio: &'a str,
    format: &'static str,
    language: &'static str,
    model: &'a str,
}

#[derive(Deserialize)]
struct TranscribeResponse {
    text: String,
}

fn transcription_endpoint(service_url: &str) -> Result<Url, LunarVoiceConfigError> {
    voice_endpoint(service_url, "/v1/voice/transcribe")
}

pub fn synthesis_endpoint(service_url: &str) -> Result<Url, LunarVoiceConfigError> {
    voice_endpoint(service_url, "/v1/voice/synthesize")
}

/// Build a LunarVoice sidecar endpoint URL from a loopback base URL.
///
/// Shared by the STT provider and the gateway voice proxy (WI#131). Enforces
/// the same invariants for every LunarVoice caller: plain HTTP, loopback host
/// only, no credentials/query/fragment, and no caller-supplied path.
pub fn voice_endpoint(service_url: &str, path: &str) -> Result<Url, LunarVoiceConfigError> {
    let mut url = Url::parse(service_url).map_err(|_| LunarVoiceConfigError::InvalidUrl)?;
    if url.scheme() != "http" {
        return Err(LunarVoiceConfigError::InvalidScheme);
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(LunarVoiceConfigError::InvalidComponents);
    }
    let is_loopback = match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if !is_loopback {
        return Err(LunarVoiceConfigError::NonLoopbackHost);
    }
    if url.path() != "/" {
        return Err(LunarVoiceConfigError::InvalidPath);
    }
    url.set_path(path);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_requires_loopback_http_base_url() {
        assert_eq!(
            transcription_endpoint("http://127.0.0.1:8090")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:8090/v1/voice/transcribe"
        );
        assert!(transcription_endpoint("http://localhost:18090").is_ok());
        assert!(transcription_endpoint("https://127.0.0.1:8090").is_err());
        assert!(transcription_endpoint("http://192.168.1.5:8090").is_err());
        assert!(transcription_endpoint("http://127.0.0.1:8090/path").is_err());
    }

    #[test]
    fn synthesis_endpoint_reuses_loopback_invariants() {
        assert_eq!(
            synthesis_endpoint("http://127.0.0.1:8090")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:8090/v1/voice/synthesize"
        );
        assert!(synthesis_endpoint("https://localhost:8090").is_err());
        assert!(synthesis_endpoint("http://example.com").is_err());
    }
}
