//! Per-channel outbound message budgets.
//!
//! Some channels ride transports with hard throughput metering — DarkIRC
//! today: DarkFi meters the network, so each IRC message carries a bounded
//! payload and consecutive messages must be spaced several seconds apart.
//! A reply that ignores those limits either floods the transport or spends
//! minutes trickling out as dozens of paced chunks.
//!
//! The budget is applied twice:
//! - **Prompt guidance** so the agent writes replies that fit — injected by
//!   the legacy prompt builder (`crate::llm::reasoning`) and by the Engine V2
//!   bridge via `ThreadConfig::channel_context`.
//! - **Deterministic enforcement** at the outbound boundary (the bundled
//!   `BeforeOutbound` hook) as a backstop when the model overshoots anyway.
//!
//! Keep the DarkIRC numbers in sync with the adapter defaults
//! (`darkirc_adapter.py`: `DARKIRC_MAX_MESSAGE_BYTES`,
//! `DARKIRC_PRIVMSG_INTERVAL`).

/// Delivery constraints of a rate-metered channel.
#[derive(Debug, Clone, Copy)]
pub struct ChannelMessageBudget {
    /// Channel name this budget applies to.
    pub channel: &'static str,
    /// Max payload bytes per transport message (one IRC chunk for DarkIRC).
    pub max_bytes_per_message: usize,
    /// Mandatory pause between transport messages, in seconds.
    pub min_send_interval_secs: u32,
    /// How many messages a reply should normally use (prompt guidance only).
    pub recommended_max_messages: usize,
    /// Hard ceiling enforced at the outbound boundary.
    pub hard_max_messages: usize,
}

/// DarkIRC: 390-byte host chunks fit in at most six 400-byte adapter chunks
/// after `[i/N] ` numbering prefixes, paced ~7s apart.
pub const DARKIRC_BUDGET: ChannelMessageBudget = ChannelMessageBudget {
    channel: "darkirc",
    max_bytes_per_message: 390,
    min_send_interval_secs: 7,
    recommended_max_messages: 3,
    hard_max_messages: 6,
};

/// Look up the message budget for a channel, if it has one.
pub fn budget_for_channel(channel: &str) -> Option<&'static ChannelMessageBudget> {
    match channel {
        "darkirc" => Some(&DARKIRC_BUDGET),
        _ => None,
    }
}

/// Marker appended when a reply is truncated to fit the hard budget. The full
/// reply is already persisted in conversation history, so the agent can honor
/// a follow-up "continue" from memory.
const TRUNCATION_MARKER: &str =
    " […truncated to fit this channel's limits — say 'continue' for the rest]";

/// How far back (in bytes) from the hard cut to look for a natural break.
const NATURAL_BREAK_WINDOW: usize = 120;

impl ChannelMessageBudget {
    /// Hard ceiling in payload bytes for one reply.
    pub fn hard_max_bytes(&self) -> usize {
        self.max_bytes_per_message * self.hard_max_messages
    }

    /// Recommended ceiling in payload bytes for one reply.
    pub fn recommended_max_bytes(&self) -> usize {
        self.max_bytes_per_message * self.recommended_max_messages
    }

    /// Prompt block describing the constraint to the agent.
    pub fn prompt_guidance(&self) -> String {
        format!(
            "- This conversation runs over {channel}, a strictly rate-metered transport: \
each message carries at most ~{bytes} bytes and consecutive messages are spaced \
~{interval}s apart, so long replies arrive slowly as numbered parts.\n\
- Keep replies short: aim for a single message (~{bytes} bytes); never use more \
than {rec} messages (~{rec_bytes} bytes) for one reply.\n\
- Plain text only — no markdown, headings, code fences, or tables.\n\
- For anything longer, send a compact summary and offer to expand on request.\n\
- Replies beyond {hard} messages (~{hard_bytes} bytes) are truncated \
automatically before delivery.",
            channel = self.channel,
            bytes = self.max_bytes_per_message,
            interval = self.min_send_interval_secs,
            rec = self.recommended_max_messages,
            rec_bytes = self.recommended_max_bytes(),
            hard = self.hard_max_messages,
            hard_bytes = self.hard_max_bytes(),
        )
    }

    /// Enforce the hard budget on an outbound reply.
    ///
    /// Returns `None` when the content already fits. Otherwise returns the
    /// content truncated at a UTF-8-safe boundary (preferring a newline or
    /// space near the cut) with [`TRUNCATION_MARKER`] appended; the result is
    /// guaranteed to fit `hard_max_bytes()`.
    pub fn enforce(&self, content: &str) -> Option<String> {
        let limit = self.hard_max_bytes();
        if content.len() <= limit {
            return None;
        }

        let budget = limit.saturating_sub(TRUNCATION_MARKER.len());
        let mut cut = content.floor_char_boundary(budget);

        // Prefer a natural break near the cut so we don't slice mid-word.
        let search_from = cut.saturating_sub(NATURAL_BREAK_WINDOW);
        let head = &content[..cut];
        let natural = head
            .rfind('\n')
            .filter(|&i| i >= search_from)
            .or_else(|| head.rfind(' ').filter(|&i| i >= search_from));
        if let Some(i) = natural {
            cut = i;
        }

        Some(format!(
            "{}{}",
            content[..cut].trim_end(),
            TRUNCATION_MARKER
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darkirc_has_a_budget_other_channels_do_not() {
        assert!(budget_for_channel("darkirc").is_some());
        assert!(budget_for_channel("gateway").is_none());
        assert!(budget_for_channel("xmpp").is_none());
        assert!(budget_for_channel("").is_none());
    }

    #[test]
    fn enforce_passes_content_within_budget() {
        let budget = DARKIRC_BUDGET;
        assert!(budget.enforce("short reply").is_none());
        let exactly_at_limit = "a".repeat(budget.hard_max_bytes());
        assert!(budget.enforce(&exactly_at_limit).is_none());
    }

    #[test]
    fn enforce_truncates_over_budget_with_marker() {
        let budget = DARKIRC_BUDGET;
        let long = "word ".repeat(1_000); // 5 KB > 2.4 KB hard budget
        let truncated = budget.enforce(&long).expect("must truncate");
        assert!(truncated.len() <= budget.hard_max_bytes());
        assert!(truncated.ends_with(TRUNCATION_MARKER));
        // The kept prefix must be real content from the original.
        let kept = &truncated[..truncated.len() - TRUNCATION_MARKER.len()];
        assert!(long.starts_with(kept.trim_end()) || kept.starts_with("word"));
    }

    #[test]
    fn enforce_is_utf8_safe_on_multibyte_content() {
        // Regression guard per repo rules: no mid-codepoint slicing on
        // model-generated text (emoji/CJK heavy replies must not panic).
        let budget = DARKIRC_BUDGET;
        for content in [
            "🐴".repeat(1_500),
            "é".repeat(4_000),
            "日本語のテキスト".repeat(400),
            format!("{}é🐴日", "x".repeat(budget.hard_max_bytes() - 2)),
        ] {
            let truncated = budget.enforce(&content).expect("over budget");
            assert!(truncated.len() <= budget.hard_max_bytes());
            assert!(truncated.ends_with(TRUNCATION_MARKER));
            // Valid UTF-8 by construction (String), but verify the prefix is
            // a char-boundary-aligned cut of the original.
            let kept = &truncated[..truncated.len() - TRUNCATION_MARKER.len()];
            let kept_trimmed = kept.trim_end();
            assert!(content.starts_with(kept_trimmed));
        }
    }

    #[test]
    fn enforce_prefers_natural_breaks() {
        let budget = DARKIRC_BUDGET;
        // Sentences separated by spaces: the cut should land on a space,
        // not mid-word.
        let long = "lorem ipsum dolor sit amet ".repeat(200);
        let truncated = budget.enforce(&long).expect("over budget");
        let kept = &truncated[..truncated.len() - TRUNCATION_MARKER.len()];
        assert!(
            long.split(' ').any(|w| kept.ends_with(w)),
            "cut should land at a word boundary, got tail: {:?}",
            &kept[kept.len().saturating_sub(20)..]
        );
    }

    #[test]
    fn prompt_guidance_mentions_the_numbers() {
        let text = DARKIRC_BUDGET.prompt_guidance();
        assert!(text.contains("darkirc"));
        assert!(text.contains("390"));
        assert!(text.contains("7"));
        assert!(text.contains("Plain text only"));
    }

    #[test]
    fn darkirc_hard_budget_fits_six_numbered_adapter_frames() {
        const ADAPTER_PAYLOAD_BYTES: usize = 400;
        const NUMBERING_PREFIX_BYTES: usize = 6; // "[6/6] "
        assert_eq!(DARKIRC_BUDGET.hard_max_bytes(), 2_340);
        assert!(
            DARKIRC_BUDGET.hard_max_bytes()
                <= DARKIRC_BUDGET.hard_max_messages
                    * (ADAPTER_PAYLOAD_BYTES - NUMBERING_PREFIX_BYTES)
        );
    }
}
