//! History and persistence layer.
//!
//! Stores job history, conversations, and actions in PostgreSQL for:
//! - Audit trail
//! - Learning from past executions
//! - Analytics and metrics

#[cfg(feature = "postgres")]
mod analytics;
/// Durable conversation-role census shared by every history reader and writer.
pub mod roles;
mod store;

#[cfg(feature = "postgres")]
pub use analytics::{JobStats, ToolStats};
#[cfg(feature = "postgres")]
pub use store::Store;
pub use store::{
    AgentJobRecord, AgentJobSummary, ConversationMessage, ConversationSummary, JobEventRecord,
    LlmCallRecord, SandboxJobRecord, SandboxJobSummary, SettingRow,
};
