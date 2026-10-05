//! Engine error types.

use std::fmt;

use crate::types::capability::EffectType;
use crate::types::thread::{ThreadId, ThreadState};

/// Safe classification for failures crossing the model-provider boundary.
///
/// Provider diagnostics stay on the host side. The execution loop exposes only
/// this bounded classification to model recovery prompts and user-visible
/// terminal messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFailureKind {
    Transient,
    RateLimited,
    InvalidOutput,
    ReplArgumentCorruption,
    ContextOverflow,
    Unavailable,
    Authentication,
    RequestRejected,
    /// The provider answered successfully but with no content because its
    /// content filter refused the request (WI#297). Deterministic and
    /// request-specific: re-sending the identical request cannot succeed.
    ContentFiltered,
    /// The provider answered successfully but with no visible content because
    /// the model spent its entire output budget first (`length`, WI#297).
    /// Deterministic: the identical request burns the same budget again.
    OutputBudgetExhausted,
    Internal,
}

impl ModelFailureKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::RateLimited => "rate_limited",
            Self::InvalidOutput => "invalid_output",
            Self::ReplArgumentCorruption => "repl_argument_corruption",
            Self::ContextOverflow => "context_overflow",
            Self::Unavailable => "unavailable",
            Self::Authentication => "authentication",
            Self::RequestRejected => "request_rejected",
            Self::ContentFiltered => "content_filtered",
            Self::OutputBudgetExhausted => "output_budget_exhausted",
            Self::Internal => "internal",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "transient" => Some(Self::Transient),
            "rate_limited" => Some(Self::RateLimited),
            "invalid_output" => Some(Self::InvalidOutput),
            "repl_argument_corruption" => Some(Self::ReplArgumentCorruption),
            "context_overflow" => Some(Self::ContextOverflow),
            "unavailable" => Some(Self::Unavailable),
            "authentication" => Some(Self::Authentication),
            "request_rejected" => Some(Self::RequestRejected),
            "content_filtered" => Some(Self::ContentFiltered),
            "output_budget_exhausted" => Some(Self::OutputBudgetExhausted),
            "internal" => Some(Self::Internal),
            _ => None,
        }
    }

    /// Whether one model-visible recovery request can reasonably change the
    /// outcome after provider-level retries and failover have been exhausted.
    pub const fn is_recoverable(self) -> bool {
        !matches!(
            self,
            Self::Authentication
                | Self::RequestRejected
                | Self::ContentFiltered
                | Self::OutputBudgetExhausted
        )
    }

    /// Safe control text for one model-visible recovery request.
    pub const fn recovery_instruction(self) -> &'static str {
        match self {
            Self::Transient => {
                "Model failure observation: the provider temporarily failed after normal retries. Continue from current state; finish concisely or choose another available approach."
            }
            Self::RateLimited => {
                "Model failure observation: the provider was rate limited after normal retries. Continue concisely from current state without repeating completed work."
            }
            Self::InvalidOutput => {
                "Model failure observation: the previous provider response was invalid. Repair response shape and continue without repeating completed work."
            }
            Self::ReplArgumentCorruption => {
                "REPL argument corruption: the previous draft was rejected before execution; no tools in that response ran. Emit a fresh, complete Python REPL block or valid structured tool arguments. Keep reasoning, think tags, XML argument tags, and nested fences out of executable code."
            }
            Self::ContextOverflow => {
                "Model failure observation: context overflowed and older context was removed. Use remaining context, finish concisely, and do not reconstruct dropped details."
            }
            Self::Unavailable => {
                "Model failure observation: the provider and configured fallback were unavailable. Service may have recovered; continue from current state and finish concisely."
            }
            Self::Authentication => {
                "Model authentication is unavailable. Ask the user/operator to configure or renew provider credentials before retrying."
            }
            Self::RequestRejected => {
                "The provider rejected this request. Ask the user/operator to review model and tool compatibility or choose another model."
            }
            Self::ContentFiltered => {
                "The provider refused to generate content for this request (content filter). Ask the user/operator to rephrase the request or choose another model."
            }
            Self::OutputBudgetExhausted => {
                "The model used its entire output budget before producing visible text. Ask the user/operator to raise the output token limit or choose another model."
            }
            Self::Internal => {
                "Model failure observation: the model gateway failed internally. Gateway may have recovered; continue from current state and finish concisely."
            }
        }
    }

    /// Safe user-facing terminal text. Never includes provider-supplied data.
    pub const fn terminal_message(self) -> &'static str {
        match self {
            Self::Transient => {
                "The model provider kept failing after retries and one recovery attempt. Retry later or configure a fallback model."
            }
            Self::RateLimited => {
                "The model provider remained rate limited. Wait before retrying, reduce request rate, or configure a fallback model."
            }
            Self::InvalidOutput => {
                "The model returned invalid output after one repair attempt. Retry with another model or review provider compatibility."
            }
            Self::ReplArgumentCorruption => {
                "The model repeatedly returned corrupted REPL arguments. The affected responses were not executed. Retry with another model or review provider output."
            }
            Self::ContextOverflow => {
                "The model context remained too large after bounded recovery. Start a new thread, reduce context, or use a larger-context model."
            }
            Self::Unavailable => {
                "The model remained unavailable after retries and fallback. Check provider health or configure another fallback model."
            }
            Self::Authentication => {
                "Model authentication failed. Configure or renew provider credentials, then retry."
            }
            Self::RequestRejected => {
                "The model provider rejected this request. Review model and tool compatibility or choose another model."
            }
            Self::ContentFiltered => {
                "The model provider refused to generate content for this request (content filter). Rephrase the request or choose another model."
            }
            Self::OutputBudgetExhausted => {
                "The model used its entire output budget before producing visible text. Raise the output token limit or choose another model."
            }
            Self::Internal => {
                "The model gateway failed after one recovery attempt. Check provider configuration and service logs, then retry."
            }
        }
    }
}

impl fmt::Display for ModelFailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Top-level engine error.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("thread error: {0}")]
    Thread(#[from] ThreadError),

    #[error("step error: {0}")]
    Step(#[from] StepError),

    #[error("capability error: {0}")]
    Capability(#[from] CapabilityError),

    #[error("store error: {reason}")]
    Store { reason: String },

    #[error("LLM {kind}: {reason}")]
    Llm {
        kind: ModelFailureKind,
        reason: String,
    },

    #[error("effect execution error: {reason}")]
    Effect { reason: String },

    /// Host policy stopped execution without exposing a reason to model/user.
    #[error("execution stopped")]
    ExecutionStopped,

    #[error("invalid state transition: {from} -> {to}")]
    InvalidTransition { from: ThreadState, to: ThreadState },

    #[error("thread not found: {0}")]
    ThreadNotFound(ThreadId),

    #[error("project not found: {0}")]
    ProjectNotFound(ProjectId),

    #[error("lease not found: {lease_id}")]
    LeaseNotFound { lease_id: String },

    #[error("lease expired for capability: {capability_name}")]
    LeaseExpired { capability_name: String },

    #[error("lease denied: {reason}")]
    LeaseDenied { reason: String },

    #[error("max iterations reached: {limit}")]
    MaxIterations { limit: usize },

    #[error("token limit exceeded: {used} of {limit}")]
    TokenLimitExceeded { used: u64, limit: u64 },

    #[error("consecutive error threshold exceeded: {count} errors (limit: {threshold})")]
    ConsecutiveErrors { count: u32, threshold: u32 },

    #[error("thread timeout: {elapsed:?} of {limit:?}")]
    Timeout {
        elapsed: std::time::Duration,
        limit: std::time::Duration,
    },

    #[error("skill error: {reason}")]
    Skill { reason: String },

    #[error("access denied: user '{user_id}' cannot access {entity}")]
    AccessDenied { user_id: String, entity: String },

    #[error("gate paused: {gate_name} requires {action_name}")]
    GatePaused {
        gate_name: String,
        action_name: String,
        call_id: String,
        parameters: Box<serde_json::Value>,
        resume_kind: Box<crate::gate::ResumeKind>,
        resume_output: Option<Box<serde_json::Value>>,
    },
}

use crate::types::project::ProjectId;

/// Thread-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum ThreadError {
    #[error("thread already running: {0}")]
    AlreadyRunning(ThreadId),

    #[error("thread is waiting for external input: {0}")]
    Waiting(ThreadId),

    #[error("thread is in terminal state: {0}")]
    Terminal(ThreadState),

    #[error("cannot spawn child: parent thread {0} is not running")]
    ParentNotRunning(ThreadId),
}

/// Step-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum StepError {
    #[error("step timed out after {0:?}")]
    Timeout(std::time::Duration),

    #[error("action not permitted by capability lease: {action}")]
    ActionDenied { action: String },
}

/// Capability-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    #[error("capability not found: {0}")]
    NotFound(String),

    #[error("effect type {effect:?} not permitted by policy")]
    EffectDenied { effect: EffectType },
}

// Display impls for types used in error messages that don't already impl Display.

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for ThreadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
