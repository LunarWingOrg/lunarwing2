//! Shared types and utilities for the LunarWing workspace.

mod event;
mod prompt_context;
mod util;

pub use event::{AppEvent, PlanStepDto, ToolDecisionDto};
pub use prompt_context::{
    DEFAULT_PROMPT_CONTEXT_MAX_CHARS_PER_MESSAGE, DEFAULT_PROMPT_CONTEXT_MAX_MESSAGES,
    DEFAULT_PROMPT_CONTEXT_MAX_TOTAL_CHARS, HARD_PROMPT_CONTEXT_LIMITS,
    HARD_PROMPT_CONTEXT_MAX_CHARS_PER_MESSAGE, HARD_PROMPT_CONTEXT_MAX_MESSAGES,
    HARD_PROMPT_CONTEXT_MAX_TOTAL_CHARS, PromptContextLimits, PromptContextMessage,
    PromptContextRole, select_prompt_context, select_prompt_context_by,
};
pub use util::truncate_preview;

/// Maximum worker agent loop iterations. Used by the orchestrator (server-side
/// clamp in `create_job_inner`) and the worker runtime (`worker/job.rs`).
/// A single source of truth prevents the two from drifting.
pub const MAX_WORKER_ITERATIONS: u32 = 500;
