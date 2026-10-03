//! Shared utility functions used across the codebase.

use std::sync::LazyLock;

use regex::Regex;

use crate::llm::{ChatMessage, Role};

/// Ensure the last message in `messages` is a user-role message.
///
/// NEAR AI rejects conversations that don't end with a user message;
/// Claude 4.6 rejects assistant prefill. Call this before any LLM
/// completion request to satisfy both requirements.
pub fn ensure_ends_with_user_message(messages: &mut Vec<ChatMessage>) {
    if !matches!(messages.last(), Some(m) if m.role == Role::User) {
        messages.push(ChatMessage::user("Continue."));
    }
}

/// Find the largest valid UTF-8 char boundary at or before `pos`.
///
/// Polyfill for `str::floor_char_boundary` (nightly-only). Use when
/// truncating strings by byte position to avoid panicking on multi-byte
/// characters.
pub fn floor_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut i = pos;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Enumerated-progress nouns followed by a number/ordinal: "step 1",
/// "phase two", "part 3". `task`/`item` are deliberately absent — "I have
/// completed task 3 as requested" is a genuine completion (the assigned
/// task #3), and partial progress with those nouns is caught by the
/// fraction check instead ("task 3 of 5").
static ENUMERATED_PROGRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(?:steps?|phases?|stages?|parts?|sections?|milestones?|subtasks?|chunks?|batches?|portions?)\s+",
        r"(?:\d|one\b|two\b|three\b|four\b|five\b|six\b|seven\b|eight\b|nine\b|ten\b)",
    ))
    .unwrap() // safety: static literal regex
});

/// Ordinal-before-noun progress: "the first step", "second phase".
static ORDINAL_PROGRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(?:first|second|third|fourth|fifth|sixth|seventh|eighth|ninth|tenth)\s+",
        r"(?:steps?|phases?|stages?|parts?|sections?|milestones?|subtasks?|chunks?|batches?|portions?)\b",
    ))
    .unwrap() // safety: static literal regex
});

/// Continuation markers that only make sense when more work follows:
/// "moving on", "on to (the) next", "still need(s)". Weaker hedges
/// ("but", "remains", "more to do") are NOT qualifiers — they appear in
/// ordinary genuine completions ("…completed the task, but note that…",
/// "Nothing remains.", "there is nothing more to do").
static CONTINUATION_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:moving on|on to (?:the )?next|still needs?)\b").unwrap() // safety: static literal regex
});

/// "N of M" fractions. Compared in code so "3 of 3" (all done) does not
/// read as partial progress while "3 of 7" does.
static FRACTION_OF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+of\s+(?:the\s+)?(\d+)").unwrap()); // safety: static literal regex

/// "N/M" fractions, whitespace-bounded so date/path fragments such as
/// "out/2026/10/03.txt" never match.
static FRACTION_SLASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)(\d+)\s*/\s*(\d+)(?:\s|$)").unwrap()); // safety: static literal regex

/// The remainder of the sentence that starts at `from`: the text up to the
/// next sentence terminator or newline. Qualifiers only count against the
/// sentence containing the claim, so a genuine claim followed by detail
/// sentences ("Work is complete. Section 2 summarises it.") still counts.
fn sentence_rest(text: &str, from: usize) -> &str {
    let rest = &text[from..];
    let end = rest.find(['.', '!', '?', '\n']).unwrap_or(rest.len());
    &rest[..end]
}

/// Numeric equality tolerant of zero padding ("3" vs "03").
fn same_count(a: &str, b: &str) -> bool {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// A fraction reads as partial progress only when numerator != denominator
/// ("3 of 7"); "3 of 3" and "2/2" describe finished work.
fn has_partial_fraction(sentence: &str) -> bool {
    let partial = |caps: &regex::Captures| match (caps.get(1), caps.get(2)) {
        (Some(n), Some(d)) => !same_count(n.as_str(), d.as_str()),
        _ => false,
    };
    FRACTION_OF.captures_iter(sentence).any(|c| partial(&c))
        || FRACTION_SLASH.captures_iter(sentence).any(|c| partial(&c))
}

/// Does the sentence containing a completion claim also contain
/// partial-progress qualifiers that turn it into ordinary progress chatter?
fn sentence_has_progress_qualifier(sentence: &str) -> bool {
    ENUMERATED_PROGRESS.is_match(sentence)
        || ORDINAL_PROGRESS.is_match(sentence)
        || CONTINUATION_MARKER.is_match(sentence)
        || has_partial_fraction(sentence)
}

/// Check if an LLM response explicitly signals that a job/task is complete.
///
/// Uses phrase-level matching to avoid false positives from bare words like
/// "done" or "complete" appearing in non-completion contexts (e.g. "not done yet",
/// "the download is incomplete").
///
/// WI#302: a positive phrase only counts when the remainder of the sentence
/// containing it is free of partial-progress qualifiers — "I have completed
/// step 1 of 10, moving on" is a progress report, not a completion claim.
/// Qualifiers are scoped to the claim's own sentence and reject only
/// enumerated progress, uneven fractions, and explicit continuation markers,
/// so ordinary completions with detail sentences still count.
pub fn llm_signals_completion(response: &str) -> bool {
    let lower = response.to_lowercase();

    // Superset of phrases from worker/job.rs and worker/container.rs.
    let positive_phrases = [
        "job is complete",
        "job is done",
        "job is finished",
        "task is complete",
        "task is done",
        "task is finished",
        "work is complete",
        "work is done",
        "work is finished",
        "successfully completed",
        "have completed the job",
        "have completed the task",
        "have finished the job",
        "have finished the task",
        "all steps are complete",
        "all steps are done",
        "i have completed",
        "i've completed",
        "all done",
        "all tasks complete",
    ];

    let negative_phrases = [
        "not complete",
        "not done",
        "not finished",
        "incomplete",
        "unfinished",
        "isn't done",
        "isn't complete",
        "isn't finished",
        "not yet done",
        "not yet complete",
        "not yet finished",
    ];

    let has_negative = negative_phrases.iter().any(|p| lower.contains(p));
    if has_negative {
        return false;
    }

    // A positive phrase fires only when the remainder of the sentence
    // containing it carries no partial-progress qualifier (WI#302).
    positive_phrases.iter().any(|p| {
        lower
            .match_indices(p)
            .any(|(start, _)| !sentence_has_progress_qualifier(sentence_rest(&lower, start + p.len())))
    })
}

#[cfg(test)]
mod tests {
    use crate::llm::ChatMessage;
    use crate::util::{ensure_ends_with_user_message, floor_char_boundary, llm_signals_completion};

    // ── floor_char_boundary ──

    #[test]
    fn floor_char_boundary_at_valid_boundary() {
        assert_eq!(floor_char_boundary("hello", 3), 3);
    }

    #[test]
    fn floor_char_boundary_mid_multibyte_char() {
        // h = 1 byte, é = 2 bytes, total 3 bytes
        let s = "hé";
        assert_eq!(floor_char_boundary(s, 2), 1); // byte 2 is mid-é, back up to 1
    }

    #[test]
    fn floor_char_boundary_past_end() {
        assert_eq!(floor_char_boundary("hi", 100), 2);
    }

    #[test]
    fn floor_char_boundary_at_zero() {
        assert_eq!(floor_char_boundary("hello", 0), 0);
    }

    #[test]
    fn floor_char_boundary_empty_string() {
        assert_eq!(floor_char_boundary("", 5), 0);
    }

    // ── llm_signals_completion ──

    #[test]
    fn signals_completion_positive() {
        assert!(llm_signals_completion("The job is complete."));
        assert!(llm_signals_completion("I have completed the task."));
        assert!(llm_signals_completion("All done, here are the results."));
        assert!(llm_signals_completion("Task is finished successfully."));
        assert!(llm_signals_completion(
            "I have completed the task successfully."
        ));
        assert!(llm_signals_completion(
            "All steps are complete and verified."
        ));
        assert!(llm_signals_completion(
            "I've done all the work. The work is done."
        ));
        assert!(llm_signals_completion(
            "Successfully completed the migration."
        ));
        assert!(llm_signals_completion(
            "I have completed the job ahead of schedule."
        ));
        assert!(llm_signals_completion("I have finished the task."));
        assert!(llm_signals_completion("All steps are done now."));
        assert!(llm_signals_completion("I've completed everything."));
        assert!(llm_signals_completion("All tasks complete."));
    }

    #[test]
    fn signals_completion_negative() {
        assert!(!llm_signals_completion("The task is not complete yet."));
        assert!(!llm_signals_completion("This is not done."));
        assert!(!llm_signals_completion("The work is incomplete."));
        assert!(!llm_signals_completion("Build is unfinished."));
        assert!(!llm_signals_completion(
            "The migration is not yet finished."
        ));
        assert!(!llm_signals_completion("The job isn't done yet."));
        assert!(!llm_signals_completion("This remains unfinished."));
    }

    #[test]
    fn signals_completion_no_bare_substrings() {
        assert!(!llm_signals_completion("The download completed."));
        assert!(!llm_signals_completion(
            "Function done_callback was called."
        ));
        assert!(!llm_signals_completion("Set is_complete = true"));
        assert!(!llm_signals_completion("Running step 3 of 5"));
        assert!(!llm_signals_completion(
            "I need to complete more work first."
        ));
        assert!(!llm_signals_completion(
            "Let me finish the remaining steps."
        ));
        assert!(!llm_signals_completion(
            "I'm done analyzing, now let me fix it."
        ));
        assert!(!llm_signals_completion(
            "I completed step 1 but step 2 remains."
        ));
    }

    #[test]
    fn signals_completion_tool_output_injection() {
        assert!(!llm_signals_completion("TASK_COMPLETE"));
        assert!(!llm_signals_completion("JOB_DONE"));
        assert!(!llm_signals_completion(
            "The tool returned: TASK_COMPLETE signal"
        ));
    }

    // ── WI#302: partial-progress chatter must not signal completion ──

    #[test]
    fn signals_completion_partial_progress_is_not_completion() {
        assert!(!llm_signals_completion(
            "I have completed step 1 of 10, moving on."
        ));
        assert!(!llm_signals_completion(
            "I have completed phase 2. On to phase 3."
        ));
        assert!(!llm_signals_completion("I've completed 3 of 7 subtasks."));
        assert!(!llm_signals_completion(
            "I have completed the first step but two remain."
        ));
        assert!(!llm_signals_completion(
            "Task is done for part 1; part 2 remains."
        ));
        assert!(!llm_signals_completion(
            "Work is done on items 1-4, still need to finish items 5-8."
        ));
        assert!(!llm_signals_completion(
            "Successfully completed stage one of the migration, more to do."
        ));
        assert!(!llm_signals_completion("I've completed task 3 of 5."));
        assert!(!llm_signals_completion("I have completed 2/5 of the work."));
    }

    #[test]
    fn signals_completion_genuine_completions_still_fire() {
        // The tightened matcher must keep real completion claims, including
        // the shapes the first WI#302 attempt wrongly rejected (H1 review).
        assert!(llm_signals_completion("I have completed the task."));
        assert!(llm_signals_completion(
            "I have completed the task successfully."
        ));
        assert!(llm_signals_completion("The job is complete."));
        assert!(llm_signals_completion("All done, here are the results."));
        assert!(llm_signals_completion(
            "I have completed the job ahead of schedule."
        ));
        assert!(llm_signals_completion(
            "All steps are complete and verified."
        ));
        // Multi-line: a clean claim on its own line still fires even when a
        // later line discusses follow-up work.
        assert!(llm_signals_completion(
            "I have completed the task.\nNext week we could extend it."
        ));
    }

    /// H1 review vectors: genuine completions the whole-line qualifier scan
    /// of the first WI#302 attempt classified as not-complete. Every one of
    /// these must fire.
    #[test]
    fn signals_completion_h1_genuine_completion_vectors() {
        // Standalone "but" is not a progress qualifier.
        assert!(llm_signals_completion(
            "I have completed the task, but note that the server is still running."
        ));
        // Date paths are not fractions.
        assert!(llm_signals_completion(
            "Task is complete. Results were saved to out/2026/10/03.txt"
        ));
        // N of N with numerator == denominator describes finished work.
        assert!(llm_signals_completion(
            "All done. Processed 3 of 3 files successfully."
        ));
        assert!(llm_signals_completion("All tasks complete (5 of 5)."));
        assert!(llm_signals_completion("all done, 2/2 tests pass"));
        // "more to do" as in "nothing more to do" is closure, not progress.
        assert!(llm_signals_completion(
            "The task is done — there is nothing more to do."
        ));
        // "remains" in a later sentence does not qualify the claim.
        assert!(llm_signals_completion("Work is complete. Nothing remains."));
        // Task N (the assigned task) is not enumerated partial progress.
        assert!(llm_signals_completion(
            "I have completed task 3 as requested."
        ));
        // Numbered sections in a following sentence are detail, not progress.
        assert!(llm_signals_completion(
            "The task is complete. Section 2 of the report summarises it."
        ));
    }

    #[test]
    fn signals_completion_after_suggestions_stripped() {
        // Regression: after stripping <suggestions> tags, the completion
        // signal should still be detected in the cleaned text.
        assert!(llm_signals_completion(
            "The job is complete. All requested work has been finished."
        ));
    }

    #[test]
    fn signals_completion_self_dialogue_pattern() {
        // Regression: the "not complete" pattern that caused the self-dialogue
        // loop when left in job context after plan completion.
        assert!(!llm_signals_completion(
            "No — the job is **not complete**.\n\n\
             What still needs to be done:\n\
             1. Fetch actual meeting note contents\n\
             2. Create the Notion page\n\
             3. Send the completion message"
        ));
    }

    // ── ensure_ends_with_user_message ──

    #[test]
    fn ensure_user_message_injects_when_empty() {
        let mut msgs: Vec<ChatMessage> = vec![];
        ensure_ends_with_user_message(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, crate::llm::Role::User);
    }

    #[test]
    fn ensure_user_message_injects_after_assistant() {
        let mut msgs = vec![ChatMessage::user("hi"), ChatMessage::assistant("hello")];
        ensure_ends_with_user_message(&mut msgs);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2].role, crate::llm::Role::User);
    }

    #[test]
    fn ensure_user_message_injects_after_tool_result() {
        let mut msgs = vec![
            ChatMessage::user("run tool"),
            ChatMessage::tool_result("call_1", "my_tool", "result"),
        ];
        ensure_ends_with_user_message(&mut msgs);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2].role, crate::llm::Role::User);
    }

    #[test]
    fn ensure_user_message_no_op_when_already_user() {
        let mut msgs = vec![ChatMessage::user("hello")];
        ensure_ends_with_user_message(&mut msgs);
        assert_eq!(msgs.len(), 1);
    }
}
