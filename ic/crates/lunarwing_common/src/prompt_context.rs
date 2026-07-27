//! Bounded conversation context shared with external workers.

use std::fmt;

use serde::{Deserialize, Serialize};

pub const DEFAULT_PROMPT_CONTEXT_MAX_MESSAGES: usize = 12;
pub const DEFAULT_PROMPT_CONTEXT_MAX_CHARS_PER_MESSAGE: usize = 2_000;
pub const DEFAULT_PROMPT_CONTEXT_MAX_TOTAL_CHARS: usize = 12_000;

pub const HARD_PROMPT_CONTEXT_MAX_MESSAGES: usize = 50;
pub const HARD_PROMPT_CONTEXT_MAX_CHARS_PER_MESSAGE: usize = 10_000;
pub const HARD_PROMPT_CONTEXT_MAX_TOTAL_CHARS: usize = 50_000;

pub const HARD_PROMPT_CONTEXT_LIMITS: PromptContextLimits = PromptContextLimits {
    max_messages: HARD_PROMPT_CONTEXT_MAX_MESSAGES,
    max_chars_per_message: HARD_PROMPT_CONTEXT_MAX_CHARS_PER_MESSAGE,
    max_total_chars: HARD_PROMPT_CONTEXT_MAX_TOTAL_CHARS,
};

/// Configurable prompt-context bounds. Values are clamped to hard ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptContextLimits {
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,
    #[serde(default = "default_max_chars_per_message")]
    pub max_chars_per_message: usize,
    #[serde(default = "default_max_total_chars")]
    pub max_total_chars: usize,
}

impl PromptContextLimits {
    #[must_use]
    pub fn bounded(self) -> Self {
        Self {
            max_messages: self.max_messages.min(HARD_PROMPT_CONTEXT_MAX_MESSAGES),
            max_chars_per_message: self
                .max_chars_per_message
                .min(HARD_PROMPT_CONTEXT_MAX_CHARS_PER_MESSAGE),
            max_total_chars: self
                .max_total_chars
                .min(HARD_PROMPT_CONTEXT_MAX_TOTAL_CHARS),
        }
    }
}

impl Default for PromptContextLimits {
    fn default() -> Self {
        Self {
            max_messages: DEFAULT_PROMPT_CONTEXT_MAX_MESSAGES,
            max_chars_per_message: DEFAULT_PROMPT_CONTEXT_MAX_CHARS_PER_MESSAGE,
            max_total_chars: DEFAULT_PROMPT_CONTEXT_MAX_TOTAL_CHARS,
        }
    }
}

const fn default_max_messages() -> usize {
    DEFAULT_PROMPT_CONTEXT_MAX_MESSAGES
}

const fn default_max_chars_per_message() -> usize {
    DEFAULT_PROMPT_CONTEXT_MAX_CHARS_PER_MESSAGE
}

const fn default_max_total_chars() -> usize {
    DEFAULT_PROMPT_CONTEXT_MAX_TOTAL_CHARS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptContextRole {
    User,
    Assistant,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptContextMessage {
    pub role: PromptContextRole,
    pub content: String,
}

impl fmt::Debug for PromptContextMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PromptContextMessage")
            .field("role", &self.role)
            .field("content_chars", &self.content.chars().count())
            .finish()
    }
}

/// Select newest messages within limits while preserving chronological order.
#[must_use]
pub fn select_prompt_context(
    messages: &[PromptContextMessage],
    limits: PromptContextLimits,
) -> Vec<PromptContextMessage> {
    select_prompt_context_by(messages, limits, |message| {
        Some((message.role, message.content.as_str()))
    })
}

/// Select context from another message type without cloning unselected content.
#[must_use]
pub fn select_prompt_context_by<'a, T>(
    messages: &'a [T],
    limits: PromptContextLimits,
    mut project: impl FnMut(&'a T) -> Option<(PromptContextRole, &'a str)>,
) -> Vec<PromptContextMessage> {
    let limits = limits.bounded();
    if limits.max_messages == 0 || limits.max_chars_per_message == 0 || limits.max_total_chars == 0
    {
        return Vec::new();
    }

    let mut selected = Vec::with_capacity(limits.max_messages.min(messages.len()));
    let mut remaining_chars = limits.max_total_chars;

    for message in messages.iter().rev() {
        if selected.len() == limits.max_messages || remaining_chars == 0 {
            break;
        }
        let Some((role, content)) = project(message) else {
            continue;
        };
        if content.trim().is_empty() {
            continue;
        }

        let max_chars = limits.max_chars_per_message.min(remaining_chars);
        let content: String = content.chars().take(max_chars).collect();
        let content_chars = content.chars().count();
        if content_chars == 0 {
            continue;
        }

        remaining_chars -= content_chars;
        selected.push(PromptContextMessage { role, content });
    }

    selected.reverse();
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: PromptContextRole, content: &str) -> PromptContextMessage {
        PromptContextMessage {
            role,
            content: content.to_string(),
        }
    }

    #[test]
    fn limits_default_and_clamp_to_hard_ceilings() {
        assert_eq!(
            PromptContextLimits::default(),
            PromptContextLimits {
                max_messages: 12,
                max_chars_per_message: 2_000,
                max_total_chars: 12_000,
            }
        );

        assert_eq!(
            PromptContextLimits {
                max_messages: usize::MAX,
                max_chars_per_message: usize::MAX,
                max_total_chars: usize::MAX,
            }
            .bounded(),
            HARD_PROMPT_CONTEXT_LIMITS
        );
    }

    #[test]
    fn newest_messages_are_returned_in_chronological_order() {
        let messages = [
            message(PromptContextRole::User, "old"),
            message(PromptContextRole::Assistant, "middle"),
            message(PromptContextRole::User, "new"),
        ];
        let selected = select_prompt_context(
            &messages,
            PromptContextLimits {
                max_messages: 2,
                max_chars_per_message: 20,
                max_total_chars: 20,
            },
        );

        assert_eq!(selected, messages[1..]);
    }

    #[test]
    fn per_message_and_total_limits_are_unicode_safe() {
        let messages = [
            message(PromptContextRole::User, "old"),
            message(PromptContextRole::Assistant, "蟹蟹蟹蟹"),
            message(PromptContextRole::User, "newest"),
        ];
        let selected = select_prompt_context(
            &messages,
            PromptContextLimits {
                max_messages: 3,
                max_chars_per_message: 4,
                max_total_chars: 6,
            },
        );

        assert_eq!(
            selected,
            vec![
                message(PromptContextRole::Assistant, "蟹蟹"),
                message(PromptContextRole::User, "newe"),
            ]
        );
    }

    #[test]
    fn empty_messages_and_zero_limits_produce_no_context() {
        let messages = [
            message(PromptContextRole::User, ""),
            message(PromptContextRole::User, "x"),
        ];
        assert_eq!(
            select_prompt_context(
                &messages,
                PromptContextLimits {
                    max_messages: 1,
                    max_chars_per_message: 1,
                    max_total_chars: 1,
                }
            ),
            vec![message(PromptContextRole::User, "x")]
        );
        assert!(
            select_prompt_context(
                &messages,
                PromptContextLimits {
                    max_messages: 0,
                    ..PromptContextLimits::default()
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn debug_output_redacts_content() {
        let message = message(PromptContextRole::User, "private conversation text");
        let debug = format!("{message:?}");

        assert!(debug.contains("content_chars: 25"));
        assert!(!debug.contains("private conversation text"));
    }
}
