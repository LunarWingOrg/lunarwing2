//! LLM provider error types.

use std::time::Duration;

/// LLM provider errors.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("Provider {provider} request failed: {reason}")]
    RequestFailed { provider: String, reason: String },

    #[error("Provider {provider} rate limited, retry after {retry_after:?}")]
    RateLimited {
        provider: String,
        retry_after: Option<Duration>,
    },

    #[error("Invalid response from {provider}: {reason}")]
    InvalidResponse { provider: String, reason: String },

    #[error("REPL argument corruption from {provider}: {signature}")]
    ReplArgumentCorruption {
        provider: String,
        signature: lunarwing_engine::types::repl::ReplArgumentCorruption,
    },

    #[error("Context length exceeded: {used} tokens used, {limit} allowed")]
    ContextLengthExceeded { used: usize, limit: usize },

    #[error("Model {model} not available on provider {provider}")]
    ModelNotAvailable { provider: String, model: String },

    #[error("Authentication failed for provider {provider}")]
    AuthFailed { provider: String },

    #[error("Session expired for provider {provider}")]
    SessionExpired { provider: String },

    #[error("Session renewal failed for provider {provider}: {reason}")]
    SessionRenewalFailed { provider: String, reason: String },

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl LlmError {
    /// Safe failure class for recovery prompts and terminal guidance.
    ///
    /// Provider-supplied diagnostics must not be copied into model-visible
    /// recovery controls. Keep this vocabulary bounded and operator-actionable.
    pub(crate) fn recovery_kind(&self) -> &'static str {
        match self {
            Self::RequestFailed { reason, .. } if super::retry::is_oversized_request(reason) => {
                "context_overflow"
            }
            Self::RequestFailed { .. } | Self::SessionRenewalFailed { .. } | Self::Io(_) => {
                "transient"
            }
            Self::RateLimited { .. } => "rate_limited",
            Self::InvalidResponse { .. } | Self::Json(_) => "invalid_output",
            Self::ReplArgumentCorruption { .. } => "repl_argument_corruption",
            Self::ContextLengthExceeded { .. } => "context_overflow",
            Self::ModelNotAvailable { .. } => "unavailable",
            Self::AuthFailed { .. } | Self::SessionExpired { .. } => "authentication",
            Self::Http(error) => match error.status() {
                Some(status)
                    if status == reqwest::StatusCode::UNAUTHORIZED
                        || status == reqwest::StatusCode::FORBIDDEN =>
                {
                    "authentication"
                }
                Some(status)
                    if status == reqwest::StatusCode::NOT_FOUND
                        || status == reqwest::StatusCode::GONE =>
                {
                    "unavailable"
                }
                Some(status) if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE => {
                    "context_overflow"
                }
                Some(status) if status == reqwest::StatusCode::TOO_MANY_REQUESTS => "rate_limited",
                Some(status) if status.is_client_error() => "request_rejected",
                _ => "transient",
            },
        }
    }

    /// Safe one-shot instruction for a model-visible recovery request.
    ///
    /// `None` means retrying the unchanged provider request cannot unlock the
    /// failure; callers should return [`Self::terminal_guidance`] immediately.
    pub(crate) fn recovery_instruction(&self) -> Option<&'static str> {
        match self.recovery_kind() {
            "transient" => Some(
                "Model failure observation: the provider temporarily failed after normal retries. Continue from current state; finish concisely or choose another available approach.",
            ),
            "invalid_output" => Some(
                "Model failure observation: the previous provider response was invalid. Repair the response shape and continue without repeating completed work.",
            ),
            "repl_argument_corruption" => Some(
                lunarwing_engine::ModelFailureKind::ReplArgumentCorruption.recovery_instruction(),
            ),
            "context_overflow" => Some(
                "Model failure observation: the context overflowed after compaction. Use the remaining context, finish concisely, and do not reconstruct dropped details.",
            ),
            "unavailable" => Some(
                "Model failure observation: the provider or selected model was unavailable. Service may have recovered; continue from current state and finish concisely.",
            ),
            _ => None,
        }
    }

    /// Safe terminal next steps after provider recovery is unavailable or spent.
    pub(crate) fn terminal_guidance(&self) -> &'static str {
        match self.recovery_kind() {
            "transient" => {
                "The model provider kept failing after retries and one recovery attempt. Retry later or configure a fallback model."
            }
            "rate_limited" => {
                "The model provider remained rate limited. Wait before retrying, reduce request rate, or configure a fallback model."
            }
            "invalid_output" => {
                "The model returned invalid output after one repair attempt. Retry with another model or review provider compatibility."
            }
            "repl_argument_corruption" => {
                lunarwing_engine::ModelFailureKind::ReplArgumentCorruption.terminal_message()
            }
            "context_overflow" => {
                "The model context remained too large after bounded recovery. Start a new thread, reduce context, or use a larger-context model."
            }
            "unavailable" => {
                "The selected model remained unavailable. Choose an available model or configure a fallback model, then retry."
            }
            "authentication" => {
                "Model authentication failed. Configure or renew provider credentials, then retry."
            }
            "request_rejected" => {
                "The model provider rejected this request. Review model and tool compatibility or choose another model."
            }
            _ => {
                "The model gateway failed after one recovery attempt. Check provider configuration and service logs, then retry."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LlmError;

    #[test]
    fn transient_provider_failure_has_bounded_recovery_guidance() {
        let error = LlmError::RequestFailed {
            provider: "test-provider".to_string(),
            reason: "temporary upstream failure".to_string(),
        };

        assert_eq!(error.recovery_kind(), "transient");
        assert!(error.recovery_instruction().is_some());
        assert!(
            !error
                .terminal_guidance()
                .contains("temporary upstream failure")
        );
    }

    #[test]
    fn authentication_failure_skips_unchanged_retry() {
        let error = LlmError::AuthFailed {
            provider: "test-provider".to_string(),
        };

        assert_eq!(error.recovery_kind(), "authentication");
        assert!(error.recovery_instruction().is_none());
        assert!(
            error
                .terminal_guidance()
                .contains("renew provider credentials")
        );
    }

    #[test]
    fn oversized_request_is_classified_as_context_overflow() {
        let error = LlmError::RequestFailed {
            provider: "test-provider".to_string(),
            reason: "maximum context length exceeded".to_string(),
        };

        assert_eq!(error.recovery_kind(), "context_overflow");
        assert!(error.terminal_guidance().contains("larger-context model"));
    }
}
