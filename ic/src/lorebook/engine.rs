//! Lorebook activation engine: keyword scanning and entry selection.
//!
//! Pure functions over resolved [`LorebookEntry`] values and recent
//! conversation text, so the whole selection path is unit-testable without
//! a workspace or database.

use std::collections::HashMap;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use sha2::{Digest, Sha256};

use crate::lorebook::model::{LorebookEntry, SelectiveLogic};
use crate::lorebook::{LOREBOOK_HEADER, LorebookConfig, render_entry};

/// Hard cap for global and per-entry message scan depth.
pub const MAX_SCAN_DEPTH: usize = 100;
/// Hard byte cap for any keyword scan window.
pub const MAX_SCAN_TEXT_BYTES: usize = 256 * 1024;
/// Hard cap for entries injected during one turn.
pub const MAX_ACTIVE_ENTRIES: usize = 100;
/// Hard cap for configured approximate prompt tokens. Zero still means no
/// token cap, but [`MAX_PROMPT_BYTES`] always applies.
pub const MAX_TOKEN_BUDGET: usize = 32 * 1024;
/// Hard byte cap for the complete rendered lorebook prompt block.
pub const MAX_PROMPT_BYTES: usize = 256 * 1024;

/// Conservative token estimate: four ASCII characters per token and two
/// tokens per non-ASCII scalar. Callers pass fully rendered prompt text so
/// wrappers and labels count toward the budget.
pub fn approx_tokens(content: &str) -> usize {
    let (ascii, non_ascii) = content.chars().fold((0usize, 0usize), |counts, ch| {
        if ch.is_ascii() {
            (counts.0.saturating_add(1), counts.1)
        } else {
            (counts.0, counts.1.saturating_add(1))
        }
    });
    ascii
        .div_ceil(4)
        .saturating_add(non_ascii.saturating_mul(2))
}

/// Case-insensitive substring match by default (SillyTavern semantics).
pub fn key_matches(text: &str, key: &str, case_sensitive: bool, whole_words: bool) -> bool {
    if key.is_empty() {
        return false;
    }
    let (text, key) = if case_sensitive {
        (
            std::borrow::Cow::Borrowed(text),
            std::borrow::Cow::Borrowed(key),
        )
    } else {
        (
            std::borrow::Cow::Owned(text.to_lowercase()),
            std::borrow::Cow::Owned(key.to_lowercase()),
        )
    };
    normalized_key_matches(&text, &key, whole_words)
}

fn normalized_key_matches(text: &str, key: &str, whole_words: bool) -> bool {
    if whole_words {
        contains_whole_key(text, key)
    } else {
        text.contains(key)
    }
}

fn contains_whole_key(text: &str, key: &str) -> bool {
    let require_left_boundary = key.chars().next().is_some_and(is_word_character);
    let require_right_boundary = key.chars().next_back().is_some_and(is_word_character);
    text.match_indices(key).any(|(start, matched)| {
        let left_ok = !require_left_boundary
            || text[..start]
                .chars()
                .next_back()
                .is_none_or(|ch| !is_word_character(ch));
        let end = start + matched.len();
        let right_ok = !require_right_boundary
            || text[end..]
                .chars()
                .next()
                .is_none_or(|ch| !is_word_character(ch));
        left_ok && right_ok
    })
}

fn is_word_character(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Join the newest `depth` messages into one scannable text block.
fn scan_window(messages: &[String], depth: usize) -> String {
    let mut remaining = MAX_SCAN_TEXT_BYTES;
    let mut selected = Vec::new();
    for message in messages.iter().rev().take(depth) {
        let separator = usize::from(!selected.is_empty());
        let available = remaining.saturating_sub(separator);
        if available == 0 {
            break;
        }
        let text = utf8_prefix(message, available);
        selected.push(text);
        remaining = remaining.saturating_sub(separator + text.len());
        if text.len() < message.len() {
            break;
        }
    }
    selected.reverse();
    selected.join("\n")
}

pub(crate) fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Keyword predicate: primary keys must match, and for selective entries a
/// secondary key must match as well.
pub fn entry_keywords_match(entry: &LorebookEntry, scan_text: &str) -> bool {
    let folded = (!entry.case_sensitive.unwrap_or(false)).then(|| scan_text.to_lowercase());
    entry_keywords_match_with_folded(entry, scan_text, folded.as_deref())
}

fn entry_keywords_match_with_folded(
    entry: &LorebookEntry,
    scan_text: &str,
    folded_scan_text: Option<&str>,
) -> bool {
    if entry.keys.is_empty() {
        return false;
    }
    let case_sensitive = entry.case_sensitive.unwrap_or(false);
    let whole_words = entry.match_whole_words.unwrap_or(false);
    let matches = |key: &str| {
        if case_sensitive {
            normalized_key_matches(scan_text, key, whole_words)
        } else {
            let folded_key = key.to_lowercase();
            normalized_key_matches(
                folded_scan_text.unwrap_or(scan_text),
                &folded_key,
                whole_words,
            )
        }
    };
    let primary = entry.keys.iter().any(|key| matches(key));
    if !primary {
        return false;
    }
    if entry.selective && !entry.secondary_keys.is_empty() {
        return match entry.selective_logic {
            SelectiveLogic::AndAny => entry.secondary_keys.iter().any(|key| matches(key)),
            SelectiveLogic::NotAll => !entry.secondary_keys.iter().all(|key| matches(key)),
            SelectiveLogic::NotAny => !entry.secondary_keys.iter().any(|key| matches(key)),
            SelectiveLogic::AndAll => entry.secondary_keys.iter().all(|key| matches(key)),
        };
    }
    true
}

/// Whether a single entry activates against the given scan text.
fn entry_activates(
    entry: &LorebookEntry,
    scan_text: &str,
    folded_scan_text: Option<&str>,
    rng: &mut impl Rng,
) -> bool {
    if !entry.enabled || entry.content.trim().is_empty() {
        return false;
    }
    let triggered =
        entry.constant || entry_keywords_match_with_folded(entry, scan_text, folded_scan_text);
    if !triggered {
        return false;
    }
    if entry.probability < 100.0 && rng.gen_range(0.0..100.0) >= entry.probability {
        return false;
    }
    true
}

/// Select the entries that activate for this conversation.
///
/// - Constant entries always activate; others need a keyword match within
///   their scan depth (per-entry override or the global `config.scan_depth`).
/// - Selection prioritizes constant entries, then descending `order`, matching
///   SillyTavern budget priority. Selected entries render by ascending `order`.
/// - `max_entries`, approximate `token_budget`, and a hard rendered-byte limit
///   are applied while selecting. Oversized entries are skipped so later small
///   entries still receive consideration.
pub fn activate<'a>(
    entries: &'a [LorebookEntry],
    messages: &[String],
    config: &LorebookConfig,
) -> Vec<&'a LorebookEntry> {
    if !config.enabled || config.max_entries == 0 {
        return Vec::new();
    }

    let mut rng = activation_rng(messages);
    let mut windows: HashMap<usize, (String, String)> = HashMap::new();
    let mut ordered: Vec<_> = entries.iter().collect();
    ordered.sort_by(|left, right| {
        right
            .constant
            .cmp(&left.constant)
            .then_with(|| right.order.cmp(&left.order))
    });
    let mut active = Vec::new();
    for entry in ordered {
        let activated = if entry.constant {
            entry_activates(entry, "", None, &mut rng)
        } else {
            let depth = entry
                .scan_depth
                .unwrap_or(config.scan_depth)
                .min(MAX_SCAN_DEPTH);
            let (scan_text, folded_scan_text) = windows.entry(depth).or_insert_with(|| {
                let scan_text = scan_window(messages, depth);
                let folded = scan_text.to_lowercase();
                (scan_text, folded)
            });
            entry_activates(entry, scan_text, Some(folded_scan_text), &mut rng)
        };
        if activated {
            active.push(entry);
        }
    }

    let mut selected = apply_limits(active, config);
    selected.sort_by_key(|entry| entry.order);
    selected
}

fn activation_rng(messages: &[String]) -> StdRng {
    let mut hasher = Sha256::new();
    for message in messages {
        hasher.update((message.len() as u64).to_le_bytes());
        hasher.update(message.as_bytes());
    }
    StdRng::from_seed(hasher.finalize().into())
}

fn apply_limits<'a>(
    active: Vec<&'a LorebookEntry>,
    config: &LorebookConfig,
) -> Vec<&'a LorebookEntry> {
    let max_entries = config.max_entries.min(MAX_ACTIVE_ENTRIES);
    let token_budget = if config.token_budget == 0 {
        0
    } else {
        config.token_budget.min(MAX_TOKEN_BUDGET)
    };
    let mut used_tokens = approx_tokens(LOREBOOK_HEADER);
    let mut used_bytes = LOREBOOK_HEADER.len();
    let mut kept = Vec::new();
    for entry in active {
        if kept.len() >= max_entries {
            break;
        }
        let rendered = render_entry(entry);
        let token_cost = approx_tokens(&rendered);
        let fits_tokens =
            token_budget == 0 || token_cost <= token_budget.saturating_sub(used_tokens);
        let fits_bytes = rendered.len() <= MAX_PROMPT_BYTES.saturating_sub(used_bytes);
        if !fits_tokens || !fits_bytes {
            continue;
        }
        used_tokens = used_tokens.saturating_add(token_cost);
        used_bytes = used_bytes.saturating_add(rendered.len());
        kept.push(entry);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(keys: &[&str], content: &str) -> LorebookEntry {
        LorebookEntry {
            name: None,
            keys: keys.iter().map(|s| (*s).to_string()).collect(),
            secondary_keys: Vec::new(),
            content: content.to_string(),
            enabled: true,
            constant: false,
            selective: false,
            selective_logic: SelectiveLogic::AndAny,
            order: 100,
            probability: 100.0,
            case_sensitive: None,
            match_whole_words: None,
            scan_depth: None,
        }
    }

    fn config() -> LorebookConfig {
        LorebookConfig::default()
    }

    #[test]
    fn constant_activates_without_messages() {
        let mut e = entry(&[], "Always here");
        e.constant = true;
        let entries = [e];
        let active = activate(&entries, &[], &config());
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn keyword_match_is_case_insensitive_by_default() {
        let e = entry(&["Dragon"], "Lore");
        let messages = vec!["tell me about dragons".to_string()];
        assert_eq!(activate(&[e], &messages, &config()).len(), 1);
    }

    #[test]
    fn case_sensitive_entry_respects_case() {
        let mut e = entry(&["Dragon"], "Lore");
        e.case_sensitive = Some(true);
        let lower = vec!["dragons".to_string()];
        assert!(activate(&[e.clone()], &lower, &config()).is_empty());
        let exact = vec!["Dragon".to_string()];
        assert_eq!(activate(&[e], &exact, &config()).len(), 1);
    }

    #[test]
    fn selective_requires_secondary_key() {
        let mut e = entry(&["dragon"], "Lore");
        e.selective = true;
        e.secondary_keys = vec!["fire".to_string()];
        let only_primary = vec!["a dragon appeared".to_string()];
        assert!(activate(&[e.clone()], &only_primary, &config()).is_empty());
        let both = vec!["a dragon appeared, breathing fire".to_string()];
        assert_eq!(activate(&[e], &both, &config()).len(), 1);
    }

    #[test]
    fn selective_logic_modes_match_sillytavern() {
        let mut e = entry(&["dragon"], "Lore");
        e.selective = true;
        e.secondary_keys = vec!["fire".to_string(), "cave".to_string()];

        let one_secondary = vec!["dragon fire".to_string()];
        e.selective_logic = SelectiveLogic::AndAny;
        assert_eq!(
            activate(std::slice::from_ref(&e), &one_secondary, &config()).len(),
            1
        );
        e.selective_logic = SelectiveLogic::AndAll;
        assert!(activate(std::slice::from_ref(&e), &one_secondary, &config()).is_empty());
        e.selective_logic = SelectiveLogic::NotAll;
        assert_eq!(
            activate(std::slice::from_ref(&e), &one_secondary, &config()).len(),
            1
        );
        e.selective_logic = SelectiveLogic::NotAny;
        assert!(activate(std::slice::from_ref(&e), &one_secondary, &config()).is_empty());

        let no_secondary = vec!["dragon".to_string()];
        assert_eq!(activate(&[e], &no_secondary, &config()).len(), 1);
    }

    #[test]
    fn whole_word_matching_rejects_substrings() {
        let mut e = entry(&["king"], "Lore");
        e.match_whole_words = Some(true);
        assert!(activate(&[e.clone()], &["liking".to_string()], &config()).is_empty());
        assert_eq!(
            activate(&[e], &["the king arrived".to_string()], &config()).len(),
            1
        );
    }

    #[test]
    fn disabled_entry_never_activates() {
        let mut e = entry(&[], "Lore");
        e.constant = true;
        e.enabled = false;
        assert!(activate(&[e], &[], &config()).is_empty());
    }

    #[test]
    fn probability_zero_never_activates() {
        let mut e = entry(&[], "Lore");
        e.constant = true;
        e.probability = 0.0;
        assert!(activate(&[e], &[], &config()).is_empty());
    }

    #[test]
    fn higher_order_wins_cap_but_selected_entries_render_ascending() {
        let mut a = entry(&[], "A");
        a.constant = true;
        a.order = 300;
        let mut b = entry(&[], "B");
        b.constant = true;
        b.order = 100;
        let mut c = entry(&[], "C");
        c.constant = true;
        c.order = 200;
        let entries = [a, b, c];
        let active = activate(&entries, &[], &config());
        let contents: Vec<&str> = active.iter().map(|e| e.content.as_str()).collect();
        assert_eq!(contents, vec!["B", "C", "A"]);

        let mut cfg = config();
        cfg.max_entries = 2;
        let mut a = entry(&[], "A");
        a.constant = true;
        a.order = 300;
        let mut b = entry(&[], "B");
        b.constant = true;
        b.order = 100;
        let mut c = entry(&[], "C");
        c.constant = true;
        c.order = 200;
        let entries = [a, b, c];
        let active = activate(&entries, &[], &cfg);
        let contents: Vec<&str> = active.iter().map(|e| e.content.as_str()).collect();
        assert_eq!(contents, vec!["C", "A"]);
    }

    #[test]
    fn constant_entries_win_cap_before_keyword_entries() {
        let mut constant = entry(&[], "constant");
        constant.constant = true;
        constant.order = 1;
        let mut keyed = entry(&["dragon"], "keyed");
        keyed.order = 999;
        let mut cfg = config();
        cfg.max_entries = 1;
        let entries = [keyed, constant];
        let active = activate(&entries, &["dragon".to_string()], &cfg);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "constant");
    }

    #[test]
    fn budget_skips_oversized_entries_but_keeps_smaller() {
        let mut big = entry(&[], &"x".repeat(400));
        big.constant = true;
        big.order = 300;
        let mut small = entry(&[], "small");
        small.constant = true;
        small.order = 200;
        let mut cfg = config();
        cfg.token_budget = approx_tokens(LOREBOOK_HEADER) + approx_tokens(&render_entry(&small));
        let entries = [big, small];
        let active = activate(&entries, &[], &cfg);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "small");
    }

    #[test]
    fn zero_budget_means_unlimited() {
        let mut big = entry(&[], &"x".repeat(10_000));
        big.constant = true;
        let mut cfg = config();
        cfg.token_budget = 0;
        let entries = [big];
        assert_eq!(activate(&entries, &[], &cfg).len(), 1);
    }

    #[test]
    fn scan_depth_limits_how_far_back_we_look() {
        let e = entry(&["dragon"], "Lore");
        // "dragon" is 3 messages back; depth 2 must miss it, depth 3 must hit.
        let messages = vec![
            "dragons were mentioned".to_string(),
            "something else".to_string(),
            "unrelated".to_string(),
        ];
        let mut shallow = config();
        shallow.scan_depth = 2;
        assert!(activate(std::slice::from_ref(&e), &messages, &shallow).is_empty());
        let mut deep = config();
        deep.scan_depth = 3;
        assert_eq!(activate(&[e], &messages, &deep).len(), 1);
    }

    #[test]
    fn scan_window_is_utf8_safe_and_byte_bounded() {
        let message = format!("dragon{}", "界".repeat(MAX_SCAN_TEXT_BYTES));
        let window = scan_window(&[message], 1);
        assert!(window.len() <= MAX_SCAN_TEXT_BYTES);
        assert!(window.starts_with("dragon"));
        assert!(window.is_char_boundary(window.len()));
    }

    #[test]
    fn zero_scan_depth_scans_no_messages_but_constants_still_activate() {
        let keyed = entry(&["dragon"], "keyed");
        let mut constant = entry(&[], "constant");
        constant.constant = true;
        let mut cfg = config();
        cfg.scan_depth = 0;
        let entries = [keyed, constant];
        let active = activate(&entries, &["dragon".to_string()], &cfg);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "constant");
    }

    #[test]
    fn per_entry_scan_depth_overrides_global() {
        let mut e = entry(&["dragon"], "Lore");
        e.scan_depth = Some(3);
        let messages = vec![
            "dragons".to_string(),
            "else".to_string(),
            "unrelated".to_string(),
        ];
        let mut cfg = config();
        cfg.scan_depth = 1;
        assert_eq!(activate(&[e], &messages, &cfg).len(), 1);
    }

    #[test]
    fn key_matches_basics() {
        assert!(key_matches("Hello World", "world", false, false));
        assert!(!key_matches("Hello World", "world", true, false));
        assert!(!key_matches("text", "", false, false));
        assert!(key_matches("a dragon!", "dragon", false, true));
        assert!(!key_matches("dragons", "dragon", false, true));
    }

    #[test]
    fn disabled_config_does_no_activation_work() {
        let mut e = entry(&[], "constant");
        e.constant = true;
        let mut cfg = config();
        cfg.enabled = false;
        assert!(activate(&[e], &[], &cfg).is_empty());
    }

    #[test]
    fn probability_roll_is_stable_for_same_context() {
        let mut e = entry(&[], "random");
        e.constant = true;
        e.probability = 50.0;
        let messages = ["stable turn context".to_string()];
        let first = !activate(std::slice::from_ref(&e), &messages, &config()).is_empty();
        for _ in 0..10 {
            assert_eq!(
                !activate(std::slice::from_ref(&e), &messages, &config()).is_empty(),
                first
            );
        }
    }

    #[test]
    fn hard_byte_limit_applies_when_token_budget_is_unlimited() {
        let content = "x".repeat(MAX_PROMPT_BYTES / 2);
        let entries: Vec<_> = (0..4)
            .map(|index| {
                let mut entry = entry(&[], &content);
                entry.constant = true;
                entry.order = index;
                entry
            })
            .collect();
        let mut cfg = config();
        cfg.token_budget = 0;
        let active = activate(&entries, &[], &cfg);
        let rendered_bytes = LOREBOOK_HEADER.len()
            + active
                .iter()
                .map(|entry| render_entry(entry).len())
                .sum::<usize>();
        assert!(rendered_bytes <= MAX_PROMPT_BYTES);
        assert!(active.len() < entries.len());
    }

    #[test]
    fn hard_entry_limit_applies_to_programmatic_config() {
        let entries: Vec<_> = (0..=MAX_ACTIVE_ENTRIES)
            .map(|index| {
                let mut entry = entry(&[], &format!("entry {index}"));
                entry.constant = true;
                entry
            })
            .collect();
        let mut cfg = config();
        cfg.max_entries = usize::MAX;
        cfg.token_budget = 0;
        assert_eq!(activate(&entries, &[], &cfg).len(), MAX_ACTIVE_ENTRIES);
    }
}
