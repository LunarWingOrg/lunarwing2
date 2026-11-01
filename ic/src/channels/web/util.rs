//! Shared utility functions for the web gateway.

use crate::channels::StatusUpdate;
use crate::channels::web::types::{ReasoningSummary, ToolCallInfo, ToolDecisionDto, TurnInfo};
use crate::history::roles::{
    ASSISTANT_ROLE, INTERRUPTED_ROLE, TOOL_CALLS_ROLE, TURN_RECOVERY_ROLE, USER_ROLE,
};

pub fn reasoning_summary(tool_calls: &[ToolCallInfo]) -> Option<ReasoningSummary> {
    let names: Vec<String> = tool_calls.iter().map(|call| call.name.clone()).collect();
    let StatusUpdate::ReasoningUpdate {
        narrative,
        decisions,
    } = StatusUpdate::reasoning_for_tools(&names)?
    else {
        return None;
    };
    Some(ReasoningSummary {
        narrative,
        decisions: decisions
            .into_iter()
            .map(|decision| ToolDecisionDto {
                tool_name: decision.tool_name,
                rationale: decision.rationale,
            })
            .collect(),
    })
}

/// Truncate a string to at most `max_bytes` bytes at a char boundary, appending "...".
///
/// If the input is wrapped in `<tool_output …>…</tool_output>` and truncation
/// removes the closing tag, the tag is re-appended so downstream XML parsers
/// never see an unclosed element.
pub fn truncate_preview(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Walk backwards from max_bytes to find a valid char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = format!("{}...", &s[..end]);

    // Re-close <tool_output> if truncation cut through the closing tag.
    if s.starts_with("<tool_output") && !result.ends_with("</tool_output>") {
        result.push_str("\n</tool_output>");
    }

    result
}

/// Truncate a string to at most `max_bytes` bytes, keeping both the head and
/// the tail and eliding the middle.
///
/// List-tool envelopes serialize pagination/completion metadata
/// (`next_cursor`, `truncated`, `complete`, `resume_page`) at the very end, so
/// a head-only cut (see `truncate_preview`) amputates the cursor and a later
/// "next page" request then has to improvise one (WI#349, WI#351). The marker
/// always separates the halves. Byte-budgeted and char-boundary safe; the
/// result never exceeds `max_bytes`. Input already within budget passes
/// through unchanged.
///
/// Unlike `truncate_preview` this never needs to re-close `<tool_output>`:
/// the preserved tail retains the closing tag whenever the input carried one.
pub fn truncate_preview_middle(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let marker = format!("\n... [output truncated to {max_bytes} bytes, middle elided] ...\n");
    let Some(keep) = max_bytes
        .checked_sub(marker.len())
        .filter(|keep| *keep >= 2)
    else {
        // The marker is pure ASCII, so char-count == byte-count here.
        return marker.chars().take(max_bytes).collect();
    };
    let head_bytes = keep / 2;
    let tail_bytes = keep - head_bytes;
    let head_end = s.floor_char_boundary(head_bytes);
    let tail_start = s.ceil_char_boundary(s.len() - tail_bytes);
    let mut result = String::with_capacity(max_bytes);
    result.push_str(&s[..head_end]);
    result.push_str(&marker);
    result.push_str(&s[tail_start..]);
    result
}

#[derive(serde::Deserialize)]
struct HistoryRecovery {
    user_message_id: uuid::Uuid,
    state: String,
    completed_at: Option<String>,
    recovery: crate::channels::web::types::TurnRecoveryInfo,
}

/// Pair user/tool/assistant rows and overlay recovery markers by user-message id.
pub fn build_turns_from_db_messages(
    messages: &[crate::history::ConversationMessage],
) -> Vec<TurnInfo> {
    let recoveries: std::collections::HashMap<_, _> = messages
        .iter()
        .filter(|msg| msg.role == TURN_RECOVERY_ROLE)
        .filter_map(|msg| serde_json::from_str::<HistoryRecovery>(&msg.content).ok())
        .filter(|status| {
            matches!(
                status.state.as_str(),
                "Retrying" | "Processing" | "Completed" | "Failed" | "Interrupted"
            )
        })
        .map(|status| (status.user_message_id, status))
        .collect();
    let mut turns = Vec::new();
    let mut turn_number = 0;
    let mut iter = messages
        .iter()
        .filter(|msg| msg.role != TURN_RECOVERY_ROLE)
        .peekable();

    while let Some(msg) = iter.next() {
        if msg.role == USER_ROLE {
            let mut turn = TurnInfo {
                turn_number,
                user_input: msg.content.clone(),
                response: None,
                state: "Completed".to_string(),
                started_at: msg.created_at.to_rfc3339(),
                completed_at: None,
                tool_calls: Vec::new(),
                reasoning: None,
                recovery: None,
            };
            if let Some(next) = iter.peek()
                && next.role == TOOL_CALLS_ROLE
                && let Some(tc_msg) = iter.next()
            {
                turn.tool_calls = history_tool_calls(tc_msg);
            }
            if let Some(next) = iter.peek()
                && next.role == ASSISTANT_ROLE
                && let Some(assistant_msg) = iter.next()
            {
                turn.response = Some(assistant_msg.content.clone());
                turn.completed_at = Some(assistant_msg.created_at.to_rfc3339());
            }
            if turn.response.is_none()
                && let Some(next) = iter.peek()
                && next.role == INTERRUPTED_ROLE
                && let Some(marker) = iter.next()
            {
                turn.state = "Interrupted".to_string();
                turn.completed_at = Some(marker.created_at.to_rfc3339());
                // A late assistant row stays on the interrupted turn.
                if let Some(next) = iter.peek()
                    && next.role == ASSISTANT_ROLE
                    && let Some(assistant_msg) = iter.next()
                {
                    turn.response = Some(assistant_msg.content.clone());
                    turn.completed_at = Some(assistant_msg.created_at.to_rfc3339());
                }
            }
            if turn.response.is_none() && turn.state == "Completed" {
                turn.state = "Failed".to_string();
            }
            if let Some(status) = recoveries.get(&msg.id) {
                if turn.state != "Interrupted" {
                    turn.state = status.state.clone();
                    turn.completed_at = status.completed_at.clone().or(turn.completed_at);
                }
                turn.recovery = Some(status.recovery.clone());
            }
            turn.reasoning = reasoning_summary(&turn.tool_calls);
            turns.push(turn);
            turn_number += 1;
        } else if msg.role == ASSISTANT_ROLE {
            turns.push(TurnInfo {
                turn_number,
                user_input: String::new(),
                response: Some(msg.content.clone()),
                state: "Completed".to_string(),
                started_at: msg.created_at.to_rfc3339(),
                completed_at: Some(msg.created_at.to_rfc3339()),
                tool_calls: Vec::new(),
                reasoning: None,
                recovery: None,
            });
            turn_number += 1;
        }
    }
    turns
}

fn history_tool_calls(msg: &crate::history::ConversationMessage) -> Vec<ToolCallInfo> {
    match serde_json::from_str::<Vec<serde_json::Value>>(&msg.content) {
        Ok(calls) => calls
            .iter()
            .map(|call| ToolCallInfo {
                name: call["name"].as_str().unwrap_or("unknown").to_string(),
                has_result: call.get("result_preview").is_some(),
                has_error: call.get("error").is_some(),
                result_preview: call["result_preview"].as_str().map(String::from),
                error: call["error"].as_str().map(String::from),
            })
            .collect(),
        Err(error) => {
            tracing::warn!(message_id = %msg.id, "Malformed tool_calls JSON in DB, skipping: {error}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // ---- truncate_preview tests ----

    #[test]
    fn test_truncate_preview_short_string() {
        assert_eq!(truncate_preview("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_preview_exact_boundary() {
        assert_eq!(truncate_preview("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_preview_truncates_ascii() {
        assert_eq!(truncate_preview("hello world", 5), "hello...");
    }

    #[test]
    fn test_truncate_preview_empty_string() {
        assert_eq!(truncate_preview("", 10), "");
    }

    #[test]
    fn test_truncate_preview_multibyte_char_boundary() {
        // '€' is 3 bytes (E2 82 AC). "a€b" = [61, E2, 82, AC, 62] = 5 bytes
        // Truncating at max_bytes=3 should not split the euro sign.
        let s = "a€b";
        let result = truncate_preview(s, 3);
        // max_bytes=3 lands mid-€, so it walks back to byte 1 ("a")
        assert_eq!(result, "a...");
    }

    #[test]
    fn test_truncate_preview_emoji() {
        // '🦀' is 4 bytes. "hi🦀" = 6 bytes
        let s = "hi🦀";
        let result = truncate_preview(s, 4);
        // max_bytes=4 lands mid-🦀, walks back to byte 2 ("hi")
        assert_eq!(result, "hi...");
    }

    #[test]
    fn test_truncate_preview_cjk() {
        // CJK characters are 3 bytes each. "你好世界" = 12 bytes
        let s = "你好世界";
        let result = truncate_preview(s, 7);
        // max_bytes=7 lands mid-character (byte 7 is inside 世), walks back to 6 ("你好")
        assert_eq!(result, "你好...");
    }

    #[test]
    fn test_truncate_preview_zero_max_bytes() {
        assert_eq!(truncate_preview("hello", 0), "...");
    }

    #[test]
    fn test_truncate_preview_closes_tool_output_tag() {
        let s = "<tool_output name=\"search\">\nSome very long content here\n</tool_output>";
        // Truncate so it cuts before the closing tag
        let result = truncate_preview(s, 60);
        assert!(result.ends_with("</tool_output>"));
        assert!(result.contains("..."));
    }

    #[test]
    fn test_truncate_preview_no_extra_close_when_intact() {
        let s = "<tool_output name=\"echo\">\nshort\n</tool_output>";
        // The string is short enough not to be truncated
        let result = truncate_preview(s, 500);
        assert_eq!(result, s);
        // Should not have a duplicate closing tag
        assert_eq!(result.matches("</tool_output>").count(), 1);
    }

    #[test]
    fn test_truncate_preview_non_xml_unaffected() {
        let s = "Just a plain long string that gets truncated";
        let result = truncate_preview(s, 10);
        assert_eq!(result, "Just a pla...");
        assert!(!result.contains("</tool_output>"));
    }

    // ---- truncate_preview_middle tests ----

    #[test]
    fn test_truncate_preview_middle_within_budget_passes_through() {
        assert_eq!(truncate_preview_middle("short", 100), "short");
        assert_eq!(truncate_preview_middle("", 0), "");
    }

    #[test]
    fn test_truncate_preview_middle_preserves_pagination_tail() {
        let tail = r#""truncated": true, "complete": false, "next_cursor": "cursor-abc123"}"#;
        let s = format!("{{\"items\": [{}] , {}", "\"x\", ".repeat(500), tail);
        let out = truncate_preview_middle(&s, 1000);
        assert!(out.len() <= 1000);
        assert!(out.contains("middle elided"));
        assert!(out.contains("cursor-abc123"));
        assert!(out.contains("\"truncated\": true"));
        assert!(out.starts_with("{\"items\": ["));
    }

    #[test]
    fn test_truncate_preview_middle_char_boundary_safe() {
        // 'é' is 2 bytes; a mid-char head cut must floor, a mid-char tail cut
        // must ceil, and the result stays within budget.
        let s = "é".repeat(9000);
        let out = truncate_preview_middle(&s, 8191);
        assert!(out.len() <= 8191);
        assert!(out.contains("middle elided"));
        assert!(out.ends_with('é'));
    }

    #[test]
    fn test_truncate_preview_middle_tiny_budget_falls_back_to_marker() {
        let out = truncate_preview_middle(&"z".repeat(500), 10);
        assert!(out.len() <= 10);
    }

    #[test]
    fn test_truncate_preview_middle_keeps_tool_output_closing_tag() {
        let s = format!(
            "<tool_output name=\"list\">\n{}\n</tool_output>",
            "row\n".repeat(500)
        );
        let out = truncate_preview_middle(&s, 1000);
        assert!(out.len() <= 1000);
        assert!(out.ends_with("</tool_output>"));
    }

    // ---- build_turns_from_db_messages tests ----

    fn make_msg(role: &str, content: &str, offset_ms: i64) -> crate::history::ConversationMessage {
        crate::history::ConversationMessage {
            id: Uuid::new_v4(),
            role: role.to_string(),
            content: content.to_string(),
            created_at: chrono::Utc::now() + chrono::TimeDelta::milliseconds(offset_ms),
        }
    }

    #[test]
    fn census_bookkeeping_roles_never_become_turns() {
        // WI#518: a bookkeeping row may sit anywhere in the history, but it
        // must never open a turn, become a response, or leak its payload.
        for role in crate::history::roles::BOOKKEEPING_ROLES {
            let payload = format!("{role} payload");
            let messages = vec![
                make_msg(role, &payload, 0),
                make_msg(USER_ROLE, "question", 1),
                make_msg(ASSISTANT_ROLE, "answer", 2),
                make_msg(role, &payload, 3),
            ];

            let turns = build_turns_from_db_messages(&messages);

            assert_eq!(turns.len(), 1, "role {role} created a turn");
            assert_eq!(turns[0].user_input, "question", "role {role}");
            assert_eq!(
                turns[0].response.as_deref(),
                Some("answer"),
                "role {role} replaced the response"
            );
            assert!(
                !format!("{turns:?}").contains(&payload),
                "role {role} leaked its payload into the rendered turn"
            );
        }
    }

    #[test]
    fn test_build_turns_complete() {
        let messages = vec![
            make_msg("user", "Hello", 0),
            make_msg("assistant", "Hi!", 1000),
            make_msg("user", "How?", 2000),
            make_msg("assistant", "Good", 3000),
        ];
        let turns = build_turns_from_db_messages(&messages);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user_input, "Hello");
        assert_eq!(turns[0].response.as_deref(), Some("Hi!"));
        assert_eq!(turns[0].state, "Completed");
        assert_eq!(turns[1].user_input, "How?");
        assert_eq!(turns[1].response.as_deref(), Some("Good"));
    }

    #[test]
    fn test_build_turns_incomplete() {
        let messages = vec![make_msg("user", "Hello", 0)];
        let turns = build_turns_from_db_messages(&messages);
        assert_eq!(turns.len(), 1);
        assert!(turns[0].response.is_none());
        assert_eq!(turns[0].state, "Failed");
    }

    #[test]
    fn test_build_turns_with_tool_calls() {
        let tc_json = serde_json::json!([
            {"name": "shell", "result_preview": "output"},
            {"name": "http", "error": "timeout"}
        ]);
        let messages = vec![
            make_msg("user", "Run it", 0),
            make_msg("tool_calls", &tc_json.to_string(), 500),
            make_msg("assistant", "Done", 1000),
        ];
        let turns = build_turns_from_db_messages(&messages);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 2);
        assert_eq!(turns[0].tool_calls[0].name, "shell");
        assert!(turns[0].tool_calls[0].has_result);
        assert_eq!(turns[0].tool_calls[1].name, "http");
        assert!(turns[0].tool_calls[1].has_error);
        assert_eq!(turns[0].response.as_deref(), Some("Done"));
        let reasoning = turns[0].reasoning.as_ref().expect("reasoning summary");
        assert_eq!(reasoning.decisions.len(), 2);
        assert_eq!(reasoning.decisions[0].tool_name, "shell");
        assert!(reasoning.decisions[0].rationale.contains("environment"));
    }

    #[test]
    fn test_build_turns_malformed_tool_calls() {
        let messages = vec![
            make_msg("user", "Hello", 0),
            make_msg("tool_calls", "not json", 500),
            make_msg("assistant", "Done", 1000),
        ];
        let turns = build_turns_from_db_messages(&messages);
        assert_eq!(turns.len(), 1);
        assert!(turns[0].tool_calls.is_empty());
        assert_eq!(turns[0].response.as_deref(), Some("Done"));
    }

    #[test]
    fn test_build_turns_standalone_assistant_messages() {
        // Routine conversations only have assistant messages (no user messages).
        let messages = vec![
            make_msg("assistant", "Routine executed: all checks passed", 0),
            make_msg("assistant", "Routine executed: found 2 issues", 5000),
        ];
        let turns = build_turns_from_db_messages(&messages);
        assert_eq!(turns.len(), 2);
        // Standalone assistant messages should have empty user_input
        assert_eq!(turns[0].user_input, "");
        assert_eq!(
            turns[0].response.as_deref(),
            Some("Routine executed: all checks passed")
        );
        assert_eq!(turns[0].state, "Completed");
        assert_eq!(turns[1].user_input, "");
        assert_eq!(
            turns[1].response.as_deref(),
            Some("Routine executed: found 2 issues")
        );
    }

    #[test]
    fn recovery_markers_are_bound_to_the_original_user_message() {
        let first = make_msg("user", "delegate once", 0);
        let second = make_msg("user", "later unrelated turn", 1000);
        let marker = serde_json::json!({
            "user_message_id": first.id, "state": "Retrying", "completed_at": null,
            "recovery": {"failure_kind": "repl_argument_corruption", "recovery_state": "retrying",
                "recovery_attempt": 1, "error": "Wait for the job id; do not re-dispatch."},
        });
        let turns = build_turns_from_db_messages(&[
            first,
            second,
            make_msg("turn_recovery", &marker.to_string(), 2000),
            make_msg("assistant", "second turn response", 3000),
        ]);
        assert_eq!(turns.len(), 2);
        let first = serde_json::to_value(&turns[0]).unwrap();
        assert_eq!(first["state"], "Retrying");
        assert_eq!(first["failure_kind"], "repl_argument_corruption");
        assert_eq!(first["recovery_state"], "retrying");
        assert!(first["completed_at"].is_null());
        assert_eq!(turns[1].state, "Completed");
        assert!(turns[1].recovery.is_none());
        assert_eq!(turns[1].response.as_deref(), Some("second turn response"));
    }

    #[test]
    fn latest_recovery_marker_wins_without_breaking_tool_history() {
        let user = make_msg("user", "delegate once", 0);
        let mut messages = vec![user.clone()];
        for (index, state, recovery_state) in [
            (1, "Retrying", "retrying"),
            (2, "Processing", "recovered"),
            (3, "Failed", "failed"),
        ] {
            let marker = serde_json::json!({
                "user_message_id": user.id, "state": state, "completed_at": "terminal timestamp",
                "recovery": {"failure_kind": "repl_argument_corruption", "recovery_state": recovery_state,
                    "recovery_attempt": 1, "error": "Safe classification"},
            });
            messages.push(make_msg("turn_recovery", &marker.to_string(), index));
        }
        messages.push(make_msg("turn_recovery", "malformed marker", 4));
        messages.push(make_msg(
            "tool_calls",
            r#"[{"name":"create_job","result_preview":"job id"}]"#,
            5,
        ));
        messages.push(make_msg("assistant", "terminal error", 6));
        let turns = build_turns_from_db_messages(&messages);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].state, "Failed");
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].response.as_deref(), Some("terminal error"));
        assert_eq!(turns[0].recovery.as_ref().unwrap().recovery_state, "failed");
    }

    #[test]
    fn test_build_turns_backward_compatible() {
        let messages = vec![
            make_msg("user", "Hello", 0),
            make_msg("assistant", "Hi!", 1000),
        ];
        let turns = build_turns_from_db_messages(&messages);
        assert_eq!(turns.len(), 1);
        assert!(turns[0].tool_calls.is_empty());
        assert!(turns[0].reasoning.is_none());
        assert_eq!(turns[0].state, "Completed");
    }
}
