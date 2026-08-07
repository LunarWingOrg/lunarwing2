//! Deterministic, bounded lorebook keyword activation and entry selection.

use std::cmp::Ordering;
use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::config::LorebookConfig;
use crate::lorebook::model::{LorebookEntry, SelectiveLogic};
use crate::lorebook::{LOREBOOK_HEADER, render_entry};

/// Maximum bytes in any keyword scan window.
pub const MAX_SCAN_TEXT_BYTES: usize = 128 * 1024;
/// Maximum bytes in the complete rendered lorebook block.
pub const MAX_PROMPT_BYTES: usize = 128 * 1024;
/// Maximum plaintext key comparisons in one activation pass.
pub const MAX_KEY_CHECKS: usize = 131_072;
/// Maximum aggregate haystack bytes inspected by plaintext key comparisons.
pub const MAX_MATCH_SCAN_BYTES: usize = 16 * 1024 * 1024;
/// Maximum aggregate raw and case-folded bytes retained by scan-window cache.
const MAX_CACHED_SCAN_WINDOW_BYTES: usize = MAX_SCAN_TEXT_BYTES * 2;
const MAX_CACHED_SCAN_WINDOWS: usize = LorebookConfig::MAX_SCAN_DEPTH + 1;

/// Conservative token estimate over fully rendered prompt text.
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

/// Match one plaintext key with optional case and whole-word constraints.
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

#[derive(Default)]
struct ScanWindowCache {
    windows: HashMap<usize, (Box<str>, Box<str>)>,
    bytes: usize,
}

impl ScanWindowCache {
    fn with_window<T>(
        &mut self,
        messages: &[String],
        requested_depth: usize,
        use_window: impl FnOnce(&str, &str) -> T,
    ) -> T {
        let effective_depth = requested_depth.min(messages.len());
        if let Some((raw, folded)) = self.windows.get(&effective_depth) {
            return use_window(raw, folded);
        }

        let raw = scan_window(messages, effective_depth).into_boxed_str();
        let folded = raw.to_lowercase().into_boxed_str();
        let window_bytes = raw.len().saturating_add(folded.len());
        let can_cache = self.windows.len() < MAX_CACHED_SCAN_WINDOWS
            && window_bytes <= MAX_CACHED_SCAN_WINDOW_BYTES.saturating_sub(self.bytes);
        if !can_cache {
            return use_window(&raw, &folded);
        }

        self.bytes = self.bytes.saturating_add(window_bytes);
        self.windows.insert(effective_depth, (raw, folded));
        let (raw, folded) = &self.windows[&effective_depth];
        use_window(raw, folded)
    }
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

/// Evaluate an entry's primary and optional-filter keys.
pub fn entry_keywords_match(entry: &LorebookEntry, scan_text: &str) -> bool {
    let folded = (!entry.case_sensitive.unwrap_or(false)).then(|| scan_text.to_lowercase());
    let mut budget = MatchBudget::default();
    entry_keywords_match_bounded(entry, scan_text, folded.as_deref(), &mut budget).unwrap_or(false)
}

#[derive(Default)]
struct MatchBudget {
    checks: usize,
    scanned_bytes: usize,
}

impl MatchBudget {
    fn charge(&mut self, haystack_bytes: usize) -> Result<(), ()> {
        if self.checks >= MAX_KEY_CHECKS
            || haystack_bytes > MAX_MATCH_SCAN_BYTES.saturating_sub(self.scanned_bytes)
        {
            return Err(());
        }
        self.checks += 1;
        self.scanned_bytes = self.scanned_bytes.saturating_add(haystack_bytes);
        Ok(())
    }
}

fn entry_keywords_match_bounded(
    entry: &LorebookEntry,
    scan_text: &str,
    folded_scan_text: Option<&str>,
    budget: &mut MatchBudget,
) -> Result<bool, ()> {
    if entry.keys.is_empty() {
        return Ok(false);
    }
    let case_sensitive = entry.case_sensitive.unwrap_or(false);
    let whole_words = entry.match_whole_words.unwrap_or(false);
    let mut matches = |key: &str| {
        if case_sensitive {
            budget.charge(scan_text.len())?;
            Ok(normalized_key_matches(scan_text, key, whole_words))
        } else {
            let folded_key = key.to_lowercase();
            let folded_scan_text = folded_scan_text.unwrap_or(scan_text);
            budget.charge(folded_scan_text.len())?;
            Ok(normalized_key_matches(
                folded_scan_text,
                &folded_key,
                whole_words,
            ))
        }
    };

    if !any_key_matches(&entry.keys, &mut matches)? {
        return Ok(false);
    }
    if !entry.selective || entry.secondary_keys.is_empty() {
        return Ok(true);
    }

    match entry.selective_logic {
        SelectiveLogic::AndAny => any_key_matches(&entry.secondary_keys, &mut matches),
        SelectiveLogic::NotAll => Ok(!all_keys_match(&entry.secondary_keys, &mut matches)?),
        SelectiveLogic::NotAny => Ok(!any_key_matches(&entry.secondary_keys, &mut matches)?),
        SelectiveLogic::AndAll => all_keys_match(&entry.secondary_keys, &mut matches),
    }
}

fn any_key_matches(
    keys: &[String],
    matches: &mut impl FnMut(&str) -> Result<bool, ()>,
) -> Result<bool, ()> {
    for key in keys {
        if matches(key)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn all_keys_match(
    keys: &[String],
    matches: &mut impl FnMut(&str) -> Result<bool, ()>,
) -> Result<bool, ()> {
    for key in keys {
        if !matches(key)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn priority_cmp(left: &LorebookEntry, right: &LorebookEntry) -> Ordering {
    right
        .order
        .cmp(&left.order)
        .then_with(|| left.source_path.cmp(&right.source_path))
        .then_with(|| left.source_index.cmp(&right.source_index))
        .then_with(|| left.entry_id.cmp(&right.entry_id))
}

fn render_cmp(left: &&LorebookEntry, right: &&LorebookEntry) -> Ordering {
    left.order
        .cmp(&right.order)
        .then_with(|| left.source_path.cmp(&right.source_path))
        .then_with(|| left.source_index.cmp(&right.source_index))
        .then_with(|| left.entry_id.cmp(&right.entry_id))
}

fn activation_seed(scope: &str, messages: &[String]) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update((scope.len() as u64).to_le_bytes());
    hasher.update(scope.as_bytes());
    for message in messages {
        hasher.update((message.len() as u64).to_le_bytes());
        hasher.update(message.as_bytes());
    }
    hasher
}

fn probability_passes(entry: &LorebookEntry, seed: &Sha256) -> bool {
    if entry.probability <= 0.0 {
        return false;
    }
    if entry.probability >= 100.0 {
        return true;
    }
    let mut hasher = seed.clone();
    hasher.update((entry.source_path.len() as u64).to_le_bytes());
    hasher.update(entry.source_path.as_bytes());
    hasher.update((entry.entry_id.len() as u64).to_le_bytes());
    hasher.update(entry.entry_id.as_bytes());
    let digest = hasher.finalize();
    let roll = u16::from_be_bytes([digest[0], digest[1]]) as u32 % 10_000;
    let threshold = (entry.probability * 100.0).round() as u32;
    roll < threshold
}

/// Activate and select entries for one prompt.
///
/// Budget selection uses descending SillyTavern `order`; constants only alter
/// trigger behavior and receive no hidden priority. Equal-order ties use stable
/// source path and entry order. Selected entries render in ascending `order`.
pub fn activate<'a>(
    entries: &'a [LorebookEntry],
    messages: &[String],
    activation_scope: &str,
    config: &LorebookConfig,
) -> Vec<&'a LorebookEntry> {
    if !config.enabled || config.max_entries == 0 || config.token_budget == 0 {
        return Vec::new();
    }

    let mut ordered: Vec<_> = entries.iter().collect();
    ordered.sort_by(|left, right| priority_cmp(left, right));
    let seed = activation_seed(activation_scope, messages);
    let mut windows = ScanWindowCache::default();
    let mut match_budget = MatchBudget::default();
    let mut matching_exhausted = false;
    let mut active = Vec::new();

    for entry in ordered {
        if !entry.enabled || entry.content.trim().is_empty() {
            continue;
        }
        let triggered = if entry.constant {
            true
        } else if matching_exhausted {
            continue;
        } else {
            let depth = entry
                .scan_depth
                .unwrap_or(config.scan_depth)
                .min(LorebookConfig::MAX_SCAN_DEPTH);
            match windows.with_window(messages, depth, |scan_text, folded_scan_text| {
                entry_keywords_match_bounded(
                    entry,
                    scan_text,
                    Some(folded_scan_text),
                    &mut match_budget,
                )
            }) {
                Ok(triggered) => triggered,
                Err(()) => {
                    tracing::warn!(
                        key_limit = MAX_KEY_CHECKS,
                        scan_byte_limit = MAX_MATCH_SCAN_BYTES,
                        "Lorebook match-work limit reached; skipping lower-priority keyed entries"
                    );
                    matching_exhausted = true;
                    continue;
                }
            }
        };
        if triggered && probability_passes(entry, &seed) {
            active.push(entry);
        }
    }

    let mut selected = apply_limits(active, config);
    selected.sort_by(render_cmp);
    selected
}

fn apply_limits<'a>(
    active: Vec<&'a LorebookEntry>,
    config: &LorebookConfig,
) -> Vec<&'a LorebookEntry> {
    let max_entries = config.max_entries.min(LorebookConfig::MAX_ACTIVE_ENTRIES);
    let token_budget = config.token_budget.min(LorebookConfig::MAX_TOKEN_BUDGET);
    let mut used_tokens = approx_tokens(LOREBOOK_HEADER);
    let mut used_bytes = LOREBOOK_HEADER.len();
    let mut kept = Vec::new();

    for entry in active {
        if kept.len() >= max_entries {
            break;
        }
        let rendered = render_entry(entry);
        let token_cost = approx_tokens(&rendered);
        let fits_tokens = token_cost <= token_budget.saturating_sub(used_tokens);
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

    fn entry(id: &str, keys: &[&str], content: &str) -> LorebookEntry {
        LorebookEntry {
            source_path: "lorebooks/test.json".to_string(),
            entry_id: id.to_string(),
            source_index: id.parse().unwrap_or(0),
            name: None,
            keys: keys.iter().map(|key| (*key).to_string()).collect(),
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
        LorebookConfig {
            enabled: true,
            ..LorebookConfig::default()
        }
    }

    #[test]
    fn keyword_case_and_whole_word_matching() {
        assert!(key_matches("Hello dragon", "DRAGON", false, false));
        assert!(!key_matches("Hello dragon", "DRAGON", true, false));
        assert!(key_matches("the king arrived", "king", false, true));
        assert!(!key_matches("liking", "king", false, true));
    }

    #[test]
    fn selective_logic_modes_match_import_contract() {
        let mut entry = entry("0", &["dragon"], "Lore");
        entry.selective = true;
        entry.secondary_keys = vec!["fire".to_string(), "cave".to_string()];

        entry.selective_logic = SelectiveLogic::AndAny;
        assert!(entry_keywords_match(&entry, "dragon fire"));
        entry.selective_logic = SelectiveLogic::AndAll;
        assert!(!entry_keywords_match(&entry, "dragon fire"));
        assert!(entry_keywords_match(&entry, "dragon fire cave"));
        entry.selective_logic = SelectiveLogic::NotAny;
        assert!(entry_keywords_match(&entry, "dragon"));
        assert!(!entry_keywords_match(&entry, "dragon cave"));
        entry.selective_logic = SelectiveLogic::NotAll;
        assert!(entry_keywords_match(&entry, "dragon cave"));
        assert!(!entry_keywords_match(&entry, "dragon fire cave"));
    }

    #[test]
    fn zero_scan_depth_does_not_scan_but_constants_activate() {
        let keyed = entry("0", &["dragon"], "keyed");
        let mut constant = entry("1", &[], "constant");
        constant.constant = true;
        let mut config = config();
        config.scan_depth = 0;
        let entries = [keyed, constant];
        let active = activate(&entries, &["dragon".to_string()], "thread", &config);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "constant");
    }

    #[test]
    fn descending_order_wins_budget_without_constant_boost() {
        let mut low_constant = entry("0", &[], "low constant");
        low_constant.constant = true;
        low_constant.order = 1;
        let mut high_keyed = entry("1", &["dragon"], "high keyed");
        high_keyed.order = 300;
        let mut config = config();
        config.max_entries = 1;
        let entries = [low_constant, high_keyed];
        let active = activate(&entries, &["dragon".to_string()], "thread", &config);
        assert_eq!(active[0].content, "high keyed");
    }

    #[test]
    fn selected_entries_render_ascending_after_priority_selection() {
        let mut low = entry("0", &[], "low");
        low.constant = true;
        low.order = 100;
        let mut high = entry("1", &[], "high");
        high.constant = true;
        high.order = 300;
        let entries = [high, low];
        let active = activate(&entries, &[], "thread", &config());
        let content: Vec<_> = active.iter().map(|entry| entry.content.as_str()).collect();
        assert_eq!(content, vec!["low", "high"]);
    }

    #[test]
    fn oversized_priority_entry_does_not_block_smaller_entry() {
        let mut large = entry("0", &[], &"x".repeat(10_000));
        large.constant = true;
        large.order = 300;
        let mut small = entry("1", &[], "small");
        small.constant = true;
        small.order = 200;
        let mut config = config();
        config.token_budget = approx_tokens(LOREBOOK_HEADER) + approx_tokens(&render_entry(&small));
        let entries = [large, small];
        let active = activate(&entries, &[], "thread", &config);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "small");
    }

    #[test]
    fn probability_is_stable_and_independent_of_unrelated_entries() {
        let mut target = entry("target", &[], "target");
        target.constant = true;
        target.probability = 50.0;
        let mut unrelated = entry("unrelated", &[], "unrelated");
        unrelated.constant = true;
        unrelated.probability = 50.0;
        unrelated.order = 999;
        let messages = ["same turn".to_string()];

        let alone = activate(
            std::slice::from_ref(&target),
            &messages,
            "thread",
            &config(),
        )
        .iter()
        .any(|entry| entry.entry_id == "target");
        let together = activate(&[unrelated, target], &messages, "thread", &config())
            .iter()
            .any(|entry| entry.entry_id == "target");
        assert_eq!(alone, together);
    }

    #[test]
    fn equal_order_ties_use_source_identity() {
        let mut later = entry("1", &[], "later");
        later.constant = true;
        later.source_path = "lorebooks/z.json".to_string();
        let mut earlier = entry("0", &[], "earlier");
        earlier.constant = true;
        earlier.source_path = "lorebooks/a.json".to_string();
        let entries = [later, earlier];
        let active = activate(&entries, &[], "thread", &config());
        assert_eq!(active[0].content, "earlier");
        assert_eq!(active[1].content, "later");
    }

    #[test]
    fn scan_window_is_utf8_safe_and_bounded() {
        let message = format!("dragon{}", "界".repeat(MAX_SCAN_TEXT_BYTES));
        let window = scan_window(&[message], 1);
        assert!(window.len() <= MAX_SCAN_TEXT_BYTES);
        assert!(window.starts_with("dragon"));
    }

    #[test]
    fn scan_window_cache_canonicalizes_one_message_depths() {
        let messages = ["X".repeat(MAX_SCAN_TEXT_BYTES)];
        let mut cache = ScanWindowCache::default();

        for depth in 1..=LorebookConfig::MAX_SCAN_DEPTH {
            cache.with_window(&messages, depth, |raw, folded| {
                assert_eq!(raw.len(), MAX_SCAN_TEXT_BYTES);
                assert_eq!(folded.len(), MAX_SCAN_TEXT_BYTES);
            });
        }

        assert_eq!(cache.windows.len(), 1);
        assert_eq!(cache.bytes, MAX_CACHED_SCAN_WINDOW_BYTES);
    }

    #[test]
    fn scan_window_cache_has_hard_aggregate_bounds() {
        let messages: Vec<_> = (0..LorebookConfig::MAX_SCAN_DEPTH)
            .map(|index| format!("{index:03}{}", "x".repeat(MAX_SCAN_TEXT_BYTES)))
            .collect();
        let mut cache = ScanWindowCache::default();

        for depth in 0..=LorebookConfig::MAX_SCAN_DEPTH {
            cache.with_window(&messages, depth, |raw, folded| {
                assert!(raw.len() <= MAX_SCAN_TEXT_BYTES);
                assert_eq!(folded, raw.to_lowercase());
            });
        }

        assert!(cache.bytes <= MAX_CACHED_SCAN_WINDOW_BYTES);
        assert!(cache.windows.len() <= MAX_CACHED_SCAN_WINDOWS);
    }

    #[test]
    fn uncached_expanding_unicode_fold_matches_normally() {
        let messages = ["İ界".repeat(MAX_SCAN_TEXT_BYTES / 3)];
        let mut cache = ScanWindowCache::default();

        cache.with_window(&messages, 1, |raw, folded| {
            assert!(raw.is_char_boundary(raw.len()));
            assert_eq!(folded, raw.to_lowercase());
        });

        assert!(cache.bytes <= MAX_CACHED_SCAN_WINDOW_BYTES);
        assert!(cache.windows.len() <= MAX_CACHED_SCAN_WINDOWS);
    }

    #[test]
    fn match_work_budget_bounds_full_window_scans() {
        let scan_text = "x".repeat(MAX_SCAN_TEXT_BYTES);
        let mut unmatched = entry("0", &[], "unused");
        unmatched.keys = (0..=MAX_MATCH_SCAN_BYTES / MAX_SCAN_TEXT_BYTES)
            .map(|index| format!("absent-{index}"))
            .collect();
        let mut budget = MatchBudget::default();

        assert!(
            entry_keywords_match_bounded(&unmatched, &scan_text, Some(&scan_text), &mut budget,)
                .is_err()
        );
        assert_eq!(budget.scanned_bytes, MAX_MATCH_SCAN_BYTES);
    }
}
