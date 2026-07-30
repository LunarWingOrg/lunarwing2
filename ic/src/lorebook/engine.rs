//! Lorebook activation engine: keyword scanning and entry selection.
//!
//! Pure functions over resolved [`LorebookEntry`] values and recent
//! conversation text, so the whole selection path is unit-testable without
//! a workspace or database.

use rand::Rng;

use crate::lorebook::LorebookConfig;
use crate::lorebook::model::LorebookEntry;

/// Rough token estimate: ~4 characters per token, plus a small constant for
/// the wrapper markup that formatting adds around each entry.
pub fn approx_tokens(content: &str) -> usize {
    content.chars().count().div_ceil(4) + 8
}

/// Case-insensitive substring match by default (SillyTavern semantics).
pub fn key_matches(text: &str, key: &str, case_sensitive: bool) -> bool {
    if key.is_empty() {
        return false;
    }
    if case_sensitive {
        text.contains(key)
    } else {
        text.to_lowercase().contains(&key.to_lowercase())
    }
}

/// Join the newest `depth` messages into one scannable text block.
fn scan_window(messages: &[String], depth: usize) -> String {
    let depth = depth.max(1);
    messages
        .iter()
        .rev()
        .take(depth)
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Keyword predicate: primary keys must match, and for selective entries a
/// secondary key must match as well.
pub fn entry_keywords_match(entry: &LorebookEntry, scan_text: &str) -> bool {
    if entry.keys.is_empty() {
        return false;
    }
    let case_sensitive = entry.case_sensitive.unwrap_or(false);
    let primary = entry
        .keys
        .iter()
        .any(|k| key_matches(scan_text, k, case_sensitive));
    if !primary {
        return false;
    }
    if entry.selective && !entry.secondary_keys.is_empty() {
        return entry
            .secondary_keys
            .iter()
            .any(|k| key_matches(scan_text, k, case_sensitive));
    }
    true
}

/// Whether a single entry activates against the given scan text.
fn entry_activates(entry: &LorebookEntry, scan_text: &str, rng: &mut impl Rng) -> bool {
    if !entry.enabled || entry.content.trim().is_empty() {
        return false;
    }
    let triggered = entry.constant || entry_keywords_match(entry, scan_text);
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
/// - Results are sorted by ascending `order`, capped at `max_entries`, and
///   trimmed to `token_budget` approximate tokens (0 = unlimited). Entries
///   that do not fit the remaining budget are skipped, so smaller entries
///   can still be included.
pub fn activate<'a>(
    entries: &'a [LorebookEntry],
    messages: &[String],
    config: &LorebookConfig,
) -> Vec<&'a LorebookEntry> {
    let mut rng = rand::thread_rng();
    let mut active: Vec<&LorebookEntry> = entries
        .iter()
        .filter(|e| {
            if e.constant {
                return entry_activates(e, "", &mut rng);
            }
            let depth = e.scan_depth.unwrap_or(config.scan_depth);
            entry_activates(e, &scan_window(messages, depth), &mut rng)
        })
        .collect();
    active.sort_by_key(|e| e.order);
    active.truncate(config.max_entries);
    apply_budget(active, config.token_budget)
}

fn apply_budget(active: Vec<&LorebookEntry>, token_budget: usize) -> Vec<&LorebookEntry> {
    if token_budget == 0 {
        return active;
    }
    let mut used = 0usize;
    let mut kept = Vec::new();
    for entry in active {
        let cost = approx_tokens(&entry.content);
        if used + cost > token_budget {
            continue;
        }
        used += cost;
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
            order: 100,
            probability: 100.0,
            case_sensitive: None,
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
    fn results_sorted_by_order_and_capped() {
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
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn budget_skips_oversized_entries_but_keeps_smaller() {
        let mut big = entry(&[], &"x".repeat(400));
        big.constant = true;
        big.order = 1;
        let mut small = entry(&[], "small");
        small.constant = true;
        small.order = 2;
        let mut cfg = config();
        cfg.token_budget = 20;
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
        assert!(key_matches("Hello World", "world", false));
        assert!(!key_matches("Hello World", "world", true));
        assert!(!key_matches("text", "", false));
    }
}
