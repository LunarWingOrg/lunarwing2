//! Thread messages — the engine's own message type.
//!
//! Simpler than the main crate's `ChatMessage`. Bridge adapters handle
//! conversion between `ThreadMessage` and `ChatMessage`.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::provenance::Provenance;
use crate::types::step::ActionCall;

/// Role of a message participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    /// Result from a capability action (replaces "Tool" role).
    ActionResult,
}

/// Provider-bound content that must not enter durable engine state.
///
/// Raw image bytes are retained in memory for the active execution only. The
/// field that contains these parts on [`ThreadMessage`] is skipped by serde,
/// and this type's `Debug` implementation reports only metadata and byte size.
#[derive(Clone, PartialEq, Eq)]
pub enum TransientContentPart {
    Image { mime_type: String, data: Vec<u8> },
}

impl fmt::Debug for TransientContentPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Image { mime_type, data } => formatter
                .debug_struct("Image")
                .field("mime_type", mime_type)
                .field("size_bytes", &data.len())
                .finish(),
        }
    }
}

/// A message in a thread's conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadMessage {
    pub role: MessageRole,
    pub content: String,
    /// Multimodal content retained only for the active provider call path.
    #[serde(skip)]
    pub transient_content_parts: Vec<TransientContentPart>,
    /// Opaque key used to recover transient parts after orchestrator JSON flow.
    #[serde(skip)]
    pub transient_content_id: Option<Uuid>,
    pub provenance: Provenance,
    /// For ActionResult messages: the call ID this is responding to.
    pub action_call_id: Option<String>,
    /// For ActionResult messages: the action name.
    pub action_name: Option<String>,
    /// For Assistant messages: actions the LLM wants to execute.
    pub action_calls: Option<Vec<ActionCall>>,
    pub timestamp: DateTime<Utc>,
}

impl ThreadMessage {
    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
            transient_content_parts: Vec::new(),
            transient_content_id: None,
            provenance: Provenance::System,
            action_call_id: None,
            action_name: None,
            action_calls: None,
            timestamp: Utc::now(),
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            transient_content_parts: Vec::new(),
            transient_content_id: None,
            provenance: Provenance::User,
            action_call_id: None,
            action_name: None,
            action_calls: None,
            timestamp: Utc::now(),
        }
    }

    /// Create a user message with provider-bound multimodal content.
    pub fn user_with_transient_parts(
        content: impl Into<String>,
        transient_content_parts: Vec<TransientContentPart>,
    ) -> Self {
        let transient_content_id = (!transient_content_parts.is_empty()).then(Uuid::new_v4);
        Self {
            role: MessageRole::User,
            content: content.into(),
            transient_content_parts,
            transient_content_id,
            provenance: Provenance::User,
            action_call_id: None,
            action_name: None,
            action_calls: None,
            timestamp: Utc::now(),
        }
    }

    /// Create an assistant text message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            transient_content_parts: Vec::new(),
            transient_content_id: None,
            provenance: Provenance::LlmGenerated,
            action_call_id: None,
            action_name: None,
            action_calls: None,
            timestamp: Utc::now(),
        }
    }

    /// Create an assistant message with action calls.
    pub fn assistant_with_actions(content: Option<String>, calls: Vec<ActionCall>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.unwrap_or_default(),
            transient_content_parts: Vec::new(),
            transient_content_id: None,
            provenance: Provenance::LlmGenerated,
            action_call_id: None,
            action_name: None,
            action_calls: Some(calls),
            timestamp: Utc::now(),
        }
    }

    /// Create an action result message.
    pub fn action_result(
        call_id: impl Into<String>,
        action_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let name: String = action_name.into();
        Self {
            role: MessageRole::ActionResult,
            content: content.into(),
            transient_content_parts: Vec::new(),
            transient_content_id: None,
            provenance: Provenance::ToolOutput {
                action_name: name.clone(),
            },
            action_call_id: Some(call_id.into()),
            action_name: Some(name),
            action_calls: None,
            timestamp: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_parts_are_redacted_and_not_serialized() {
        let message = ThreadMessage::user_with_transient_parts(
            "inspect image",
            vec![TransientContentPart::Image {
                mime_type: "image/png".to_string(),
                data: vec![1, 2, 3],
            }],
        );

        let debug = format!("{message:?}");
        assert!(debug.contains("size_bytes: 3"));
        assert!(!debug.contains("[1, 2, 3]"));

        let json = serde_json::to_string(&message).expect("message should serialize");
        assert!(!json.contains("transient_content_parts"));
        assert!(!json.contains("transient_content_id"));
        assert!(!json.contains("image/png"));

        let restored: ThreadMessage =
            serde_json::from_str(&json).expect("message should deserialize");
        assert!(restored.transient_content_parts.is_empty());
        assert_eq!(restored.transient_content_id, None);
    }
}
