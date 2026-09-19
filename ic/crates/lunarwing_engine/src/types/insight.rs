//! Host-owned evidence and persistence contracts for conversation insights.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::conversation::ConversationId;
use crate::types::project::ProjectId;
use crate::types::thread::ThreadId;

/// A fresh, complete user input captured at ingestion, not reconstructed history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsightSource {
    pub message_id: Uuid,
    pub thread_id: ThreadId,
    pub conversation_id: Option<ConversationId>,
    pub timestamp: DateTime<Utc>,
    pub text: String,
    pub truncated: bool,
}

/// Durable ordering of completed source threads; independent of process counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsightCursor {
    pub completed_at: DateTime<Utc>,
    pub thread_id: ThreadId,
}

impl InsightCursor {
    pub fn is_after(&self, other: &Self) -> bool {
        (self.completed_at, self.thread_id.0) > (other.completed_at, other.thread_id.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightBatch {
    pub project_id: ProjectId,
    pub user_id: String,
    pub sources: Vec<InsightSource>,
    pub cursor: InsightCursor,
    /// Canonically ordered source identities, not a restart-sensitive counter.
    pub content_hash: String,
}

impl InsightBatch {
    pub fn recall_query(&self) -> String {
        self.sources
            .iter()
            .map(|source| source.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightKind {
    /// The content is a complete user statement, not a generated paraphrase.
    Explicit,
    /// A model hypothesis: persisted for inspection, excluded from automatic recall.
    Inferred,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsightCitation {
    pub thread_id: ThreadId,
    pub message_id: Uuid,
    /// Must equal the entire captured statement, including qualifications/negation.
    pub quote: String,
}

/// Model-facing request inside `metadata.insight`; all fields are validated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsightRequest {
    pub kind: InsightKind,
    pub evidence: Vec<InsightCitation>,
}

/// Host-produced record persisted in a MemoryDoc's metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightRecord {
    pub version: u32,
    pub kind: InsightKind,
    pub sources: Vec<InsightSource>,
    pub generated_by: ThreadId,
    pub runs: Vec<ThreadId>,
}

pub const INSIGHT_RECORD_VERSION: u32 = 1;
pub const INSIGHT_BATCH_THREADS: usize = 5;
pub const MAX_INSIGHT_SOURCE_CHARS: usize = 8_000;
pub const MAX_INSIGHT_THREAD_CHARS: usize = 16_000;
pub const MAX_INSIGHTS_PER_RUN: usize = 5;
