use secrecy::SecretString;

use crate::config::helpers::{optional_env, parse_bool_env, validate_base_url};
use crate::error::ConfigError;
use crate::settings::Settings;

/// Transcription pipeline configuration.
#[derive(Debug, Clone)]
pub struct TranscriptionConfig {
    /// Whether audio transcription is enabled.
    pub enabled: bool,
    /// Provider: "openai" (default), "chat_completions", or "lunarvoice".
    pub provider: String,
    /// OpenAI API key (reuses OPENAI_API_KEY).
    pub openai_api_key: Option<SecretString>,
    /// Explicit transcription API key (overrides provider-specific keys).
    pub api_key: Option<SecretString>,
    /// LLM API key (reuses LLM_API_KEY, used as fallback for chat_completions).
    pub llm_api_key: Option<SecretString>,
    /// Model to use (default depends on provider).
    pub model: String,
    /// Base URL override for the transcription API.
    pub base_url: Option<String>,
    /// Loopback LunarVoice sidecar URL.
    pub voice_service_url: Option<String>,
    /// LunarVoice bearer token.
    pub voice_auth_token: Option<SecretString>,
    /// Maximum synthesized audio seconds the sidecar accepts before rejecting
    /// the clip (mirrors the sidecar's `VOICE_MAX_TTS_AUDIO_SECS`); used by
    /// the gateway voice proxy's `text_too_long` pre-flight.
    pub voice_max_synth_audio_secs: u64,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "openai".to_string(),
            openai_api_key: None,
            api_key: None,
            llm_api_key: None,
            model: "whisper-1".to_string(),
            base_url: None,
            voice_service_url: None,
            voice_auth_token: None,
            voice_max_synth_audio_secs: 300,
        }
    }
}

impl TranscriptionConfig {
    pub(crate) fn resolve(settings: &Settings) -> Result<Self, ConfigError> {
        let enabled = parse_bool_env(
            "TRANSCRIPTION_ENABLED",
            settings.transcription.as_ref().is_some_and(|t| t.enabled),
        )?;

        let provider =
            optional_env("TRANSCRIPTION_PROVIDER")?.unwrap_or_else(|| "openai".to_string());

        let openai_api_key = optional_env("OPENAI_API_KEY")?.map(SecretString::from);
        let api_key = optional_env("TRANSCRIPTION_API_KEY")?.map(SecretString::from);
        let llm_api_key = optional_env("LLM_API_KEY")?.map(SecretString::from);
        let voice_auth_token = optional_env("VOICE_AUTH_TOKEN")?.map(SecretString::from);

        let default_model = match provider.as_str() {
            "chat_completions" => "google/gemini-2.0-flash-001",
            "lunarvoice" => "whisper-tiny",
            _ => "whisper-1",
        };
        let model =
            optional_env("TRANSCRIPTION_MODEL")?.unwrap_or_else(|| default_model.to_string());

        let base_url = optional_env("TRANSCRIPTION_BASE_URL")?;
        let voice_service_url = optional_env("VOICE_SERVICE_URL")?;
        let voice_max_synth_audio_secs =
            crate::config::helpers::parse_optional_env("VOICE_MAX_TTS_AUDIO_SECS", 300u64)?;

        // Validate base URL to prevent SSRF (#1103).
        if let Some(ref url) = base_url {
            validate_base_url(url, "TRANSCRIPTION_BASE_URL")?;
        }

        Ok(Self {
            enabled,
            provider,
            openai_api_key,
            api_key,
            llm_api_key,
            model,
            base_url,
            voice_service_url,
            voice_auth_token,
            voice_max_synth_audio_secs,
        })
    }

    /// Resolve the API key for the configured provider.
    ///
    /// Priority: `TRANSCRIPTION_API_KEY` > provider-specific key. LunarVoice is
    /// intentionally exempt and accepts only its dedicated loopback token.
    fn resolve_api_key(&self) -> Option<&SecretString> {
        if self.provider == "lunarvoice" {
            return self.voice_auth_token.as_ref();
        }
        self.api_key
            .as_ref()
            .or_else(|| match self.provider.as_str() {
                "chat_completions" => self.llm_api_key.as_ref().or(self.openai_api_key.as_ref()),
                _ => self.openai_api_key.as_ref(),
            })
    }

    /// Create the transcription provider if enabled and configured.
    pub fn create_provider(
        &self,
    ) -> Option<Box<dyn crate::llm::transcription::TranscriptionProvider>> {
        if !self.enabled {
            return None;
        }

        let api_key = self.resolve_api_key()?;

        match self.provider.as_str() {
            "lunarvoice" => {
                let service_url = self
                    .voice_service_url
                    .as_deref()
                    .unwrap_or("http://127.0.0.1:8090");
                match crate::llm::transcription::LunarVoiceTranscriptionProvider::new(
                    service_url,
                    api_key.clone(),
                ) {
                    Ok(provider) => {
                        tracing::info!(
                            model = %self.model,
                            "Audio transcription enabled via local LunarVoice"
                        );
                        Some(Box::new(provider.with_model(&self.model)))
                    }
                    Err(error) => {
                        tracing::error!(error = %error, "Invalid LunarVoice configuration");
                        None
                    }
                }
            }
            "chat_completions" => {
                tracing::info!(
                    model = %self.model,
                    "Audio transcription enabled via Chat Completions API"
                );

                let mut provider =
                    crate::llm::transcription::ChatCompletionsTranscriptionProvider::new(
                        api_key.clone(),
                    )
                    .with_model(&self.model);

                if let Some(ref base_url) = self.base_url {
                    provider = provider.with_base_url(base_url);
                }

                Some(Box::new(provider))
            }
            _ => {
                tracing::info!(
                    model = %self.model,
                    "Audio transcription enabled via OpenAI Whisper"
                );

                let mut provider =
                    crate::llm::transcription::OpenAiWhisperProvider::new(api_key.clone())
                        .with_model(&self.model);

                if let Some(ref base_url) = self.base_url {
                    provider = provider.with_base_url(base_url);
                }

                Some(Box::new(provider))
            }
        }
    }
}
