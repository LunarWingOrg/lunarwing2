//! Lorebook data model with SillyTavern World Info compatibility.
//!
//! Parses both the SillyTavern export shape (`{"entries": {"0": {...}}}` with
//! `key`/`keysecondary`/`disable` fields) and a simpler native shape
//! (`{"entries": [{...}]}` with `keys`/`secondary_keys`/`enabled` fields).
//! Unknown fields are ignored so newer SillyTavern exports keep parsing.

use std::cmp::Ordering;

use serde::Deserialize;
use serde_json::Value;

/// Hard cap for one lorebook document before JSON parsing.
pub const MAX_LOREBOOK_FILE_BYTES: usize = 1024 * 1024;
/// Hard cap for entries accepted from one lorebook document.
pub const MAX_ENTRIES_PER_FILE: usize = 1024;
/// Hard cap for trigger keys retained on one entry.
pub const MAX_KEYS_PER_ENTRY: usize = 128;
/// Hard cap for one trigger key.
pub const MAX_KEY_BYTES: usize = 256;
/// Hard cap for an entry display name.
pub const MAX_NAME_BYTES: usize = 512;
/// Hard cap for one entry's prompt content.
pub const MAX_ENTRY_CONTENT_BYTES: usize = 64 * 1024;

/// Errors produced when parsing a lorebook file.
#[derive(Debug, thiserror::Error)]
pub enum LorebookError {
    /// The file was not valid JSON or did not match a known lorebook shape.
    #[error("invalid lorebook JSON: {0}")]
    Parse(String),
}

/// SillyTavern optional-filter logic (`selectiveLogic`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SelectiveLogic {
    /// Primary key and any secondary key must match.
    #[default]
    AndAny,
    /// Primary key must match, but not every secondary key may match.
    NotAll,
    /// Primary key must match and no secondary key may match.
    NotAny,
    /// Primary key and every secondary key must match.
    AndAll,
}

impl SelectiveLogic {
    fn from_sillytavern(value: Option<u8>) -> Self {
        match value {
            Some(1) => Self::NotAll,
            Some(2) => Self::NotAny,
            Some(3) => Self::AndAll,
            _ => Self::AndAny,
        }
    }
}

/// A single keyword-triggered lore entry.
#[derive(Debug, Clone, PartialEq)]
pub struct LorebookEntry {
    /// Display name (SillyTavern `comment`, native `name`). Used only for the
    /// prompt block label; never matched against conversation text.
    pub name: Option<String>,
    /// Primary trigger keywords (SillyTavern `key`).
    pub keys: Vec<String>,
    /// Secondary keywords required in addition when `selective` is set
    /// (SillyTavern `keysecondary`).
    pub secondary_keys: Vec<String>,
    /// Reference text injected into the prompt when the entry activates.
    pub content: String,
    /// Whether the entry can activate. Inverse of SillyTavern `disable`.
    pub enabled: bool,
    /// Always active, no keyword match required (SillyTavern `constant`).
    pub constant: bool,
    /// When true, a secondary keyword must also match (SillyTavern `selective`).
    pub selective: bool,
    /// Logic applied to secondary keywords when `selective` is true.
    pub selective_logic: SelectiveLogic,
    /// Insertion order; lower values appear earlier in the prompt block.
    /// SillyTavern default is 100.
    pub order: i64,
    /// Activation probability in percent, 0.0–100.0. 100 always activates.
    pub probability: f64,
    /// Case-sensitive keyword matching. Default false (SillyTavern default).
    pub case_sensitive: Option<bool>,
    /// Require plaintext keys to match at word boundaries.
    pub match_whole_words: Option<bool>,
    /// Per-entry scan depth in messages. Overrides the global scan depth
    /// (SillyTavern `scanDepth`).
    pub scan_depth: Option<usize>,
}

/// Tolerates `"key": "single"` as well as `"key": ["a", "b"]`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringList {
    One(String),
    Many(Vec<String>),
}

impl Default for StringList {
    fn default() -> Self {
        Self::Many(Vec::new())
    }
}

impl StringList {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(s) => vec![s],
            Self::Many(v) => v,
        }
    }
}

/// Raw serde view of one entry, covering both field naming conventions.
#[derive(Debug, Deserialize)]
struct RawLorebookEntry {
    #[serde(default, alias = "comment")]
    name: Option<String>,
    #[serde(default, alias = "key")]
    keys: StringList,
    #[serde(default, alias = "keysecondary", alias = "secondary_keys")]
    secondary_keys: StringList,
    #[serde(default)]
    content: String,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    disable: Option<bool>,
    #[serde(default)]
    constant: bool,
    #[serde(default)]
    selective: bool,
    #[serde(default, alias = "selectiveLogic")]
    selective_logic: Option<u8>,
    #[serde(default, alias = "insertion_order")]
    order: Option<i64>,
    #[serde(default)]
    probability: Option<f64>,
    #[serde(default, alias = "useProbability")]
    use_probability: Option<bool>,
    #[serde(default, alias = "caseSensitive")]
    case_sensitive: Option<bool>,
    #[serde(default, alias = "matchWholeWords")]
    match_whole_words: Option<bool>,
    #[serde(default, alias = "scanDepth")]
    scan_depth: Option<usize>,
}

impl RawLorebookEntry {
    fn resolve(self) -> Option<LorebookEntry> {
        if self.content.trim().is_empty() || self.content.len() > MAX_ENTRY_CONTENT_BYTES {
            return None;
        }
        let enabled = self.enabled.unwrap_or(!self.disable.unwrap_or(false));
        // SillyTavern only applies probability when `useProbability` is true;
        // the native format treats any probability < 100 as active.
        let probability = match (self.use_probability, self.probability) {
            (Some(false), _) => 100.0,
            (_, Some(p)) => p.clamp(0.0, 100.0),
            _ => 100.0,
        };
        Some(LorebookEntry {
            name: self
                .name
                .filter(|n| !n.trim().is_empty() && n.len() <= MAX_NAME_BYTES),
            keys: clean_keys(self.keys),
            secondary_keys: clean_keys(self.secondary_keys),
            content: self.content,
            enabled,
            constant: self.constant,
            selective: self.selective,
            selective_logic: SelectiveLogic::from_sillytavern(self.selective_logic),
            order: self.order.unwrap_or(100),
            probability,
            case_sensitive: self.case_sensitive,
            match_whole_words: self.match_whole_words,
            scan_depth: self.scan_depth,
        })
    }
}

fn clean_keys(keys: StringList) -> Vec<String> {
    keys.into_vec()
        .into_iter()
        .filter(|key| !key.trim().is_empty() && key.len() <= MAX_KEY_BYTES)
        .take(MAX_KEYS_PER_ENTRY)
        .collect()
}

fn entry_id_cmp(left: &str, right: &str) -> Ordering {
    match (left.parse::<u64>(), right.parse::<u64>()) {
        (Ok(left_number), Ok(right_number)) => {
            left_number.cmp(&right_number).then_with(|| left.cmp(right))
        }
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => left.cmp(right),
    }
}

fn into_entry_values(value: Value) -> Result<Vec<(String, Value)>, LorebookError> {
    match value {
        Value::Array(values) => Ok(values
            .into_iter()
            .enumerate()
            .map(|(index, value)| (index.to_string(), value))
            .collect()),
        Value::Object(values) => {
            let mut values: Vec<_> = values.into_iter().collect();
            values.sort_by(|(left, _), (right, _)| entry_id_cmp(left, right));
            Ok(values)
        }
        _ => Err(LorebookError::Parse(
            "entries must be an array or object".to_string(),
        )),
    }
}

/// Parse a lorebook file into resolved entries.
///
/// Malformed or oversized individual entries are skipped so valid siblings
/// still load. Invalid outer JSON/shape and oversized files reject the file.
pub fn parse_lorebook(content: &str) -> Result<Vec<LorebookEntry>, LorebookError> {
    if content.len() > MAX_LOREBOOK_FILE_BYTES {
        return Err(LorebookError::Parse(format!(
            "file exceeds {MAX_LOREBOOK_FILE_BYTES} byte limit"
        )));
    }

    let value: Value =
        serde_json::from_str(content).map_err(|e| LorebookError::Parse(e.to_string()))?;
    let entries = match value {
        Value::Object(mut object) if object.contains_key("entries") => {
            object.remove("entries").unwrap_or(Value::Null)
        }
        value => value,
    };
    let values = into_entry_values(entries)?;
    if values.len() > MAX_ENTRIES_PER_FILE {
        tracing::warn!(
            count = values.len(),
            limit = MAX_ENTRIES_PER_FILE,
            "Lorebook entry limit reached; ignoring remaining entries"
        );
    }

    let mut parsed = Vec::new();
    for (entry_id, value) in values.into_iter().take(MAX_ENTRIES_PER_FILE) {
        match serde_json::from_value::<RawLorebookEntry>(value) {
            Ok(raw) => parsed.extend(raw.resolve()),
            Err(error) => {
                tracing::warn!(entry = %entry_id, %error, "Skipping malformed lorebook entry");
            }
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sillytavern_export_shape() {
        let json = r#"{
            "entries": {
                "0": {
                    "uid": 0,
                    "key": ["dragon", "drake"],
                    "keysecondary": ["fire"],
                    "comment": "Dragon lore",
                    "content": "Dragons are extinct.",
                    "constant": false,
                    "selective": true,
                    "selectiveLogic": 3,
                    "order": 50,
                    "position": 0,
                    "disable": false,
                    "probability": 80,
                    "useProbability": true,
                    "scanDepth": 4,
                    "caseSensitive": true,
                    "matchWholeWords": true,
                    "vectorized": false
                }
            }
        }"#;
        let entries = parse_lorebook(json).expect("ST export should parse");
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.name.as_deref(), Some("Dragon lore"));
        assert_eq!(e.keys, vec!["dragon", "drake"]);
        assert_eq!(e.secondary_keys, vec!["fire"]);
        assert!(e.enabled);
        assert!(e.selective);
        assert_eq!(e.selective_logic, SelectiveLogic::AndAll);
        assert!(!e.constant);
        assert_eq!(e.order, 50);
        assert!((e.probability - 80.0).abs() < f64::EPSILON);
        assert_eq!(e.scan_depth, Some(4));
        assert_eq!(e.case_sensitive, Some(true));
        assert_eq!(e.match_whole_words, Some(true));
    }

    #[test]
    fn parses_native_list_shape() {
        let json = r#"{
            "name": "My book",
            "entries": [
                {"keys": ["moon"], "content": "The moon is hollow.", "constant": false},
                {"keys": [], "content": "Always on.", "constant": true, "enabled": true}
            ]
        }"#;
        let entries = parse_lorebook(json).expect("native shape should parse");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].keys, vec!["moon"]);
        assert_eq!(entries[0].order, 100);
        assert!(entries[1].constant);
    }

    #[test]
    fn parses_bare_array_and_map() {
        let array = r#"[{"keys": ["a"], "content": "A"}]"#;
        assert_eq!(parse_lorebook(array).expect("bare array").len(), 1);
        let map = r#"{"0": {"keys": ["b"], "content": "B"}}"#;
        assert_eq!(parse_lorebook(map).expect("bare map").len(), 1);
    }

    #[test]
    fn disable_inverts_enabled() {
        let json = r#"[{"keys": ["a"], "content": "A", "disable": true}]"#;
        let entries = parse_lorebook(json).expect("parse");
        assert!(!entries[0].enabled);
    }

    #[test]
    fn explicit_enabled_wins_over_disable() {
        let json = r#"[{"keys": ["a"], "content": "A", "disable": false, "enabled": false}]"#;
        let entries = parse_lorebook(json).expect("parse");
        assert!(!entries[0].enabled);
    }

    #[test]
    fn probability_ignored_when_use_probability_false() {
        let json =
            r#"[{"keys": ["a"], "content": "A", "probability": 10, "useProbability": false}]"#;
        let entries = parse_lorebook(json).expect("parse");
        assert!((entries[0].probability - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn single_string_key_accepted() {
        let json = r#"[{"keys": "solo", "content": "A"}]"#;
        let entries = parse_lorebook(json).expect("parse");
        assert_eq!(entries[0].keys, vec!["solo"]);
    }

    #[test]
    fn empty_content_entries_dropped() {
        let json = r#"[{"keys": ["a"], "content": "  "}, {"keys": ["b"], "content": "B"}]"#;
        let entries = parse_lorebook(json).expect("parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "B");
    }

    #[test]
    fn map_entries_have_deterministic_numeric_order() {
        let json = r#"{
            "entries": {
                "10": {"content": "ten", "constant": true},
                "alpha": {"content": "alpha", "constant": true},
                "2": {"content": "two", "constant": true}
            }
        }"#;
        let entries = parse_lorebook(json).expect("parse");
        let contents: Vec<_> = entries.iter().map(|entry| entry.content.as_str()).collect();
        assert_eq!(contents, vec!["two", "ten", "alpha"]);
    }

    #[test]
    fn malformed_entry_does_not_drop_valid_siblings() {
        let json = r#"{
            "entries": {
                "0": {"content": "valid", "constant": true},
                "1": {"content": "invalid", "key": 42},
                "2": {"content": "also valid", "constant": true}
            }
        }"#;
        let entries = parse_lorebook(json).expect("outer shape remains valid");
        let contents: Vec<_> = entries.iter().map(|entry| entry.content.as_str()).collect();
        assert_eq!(contents, vec!["valid", "also valid"]);
    }

    #[test]
    fn oversized_entry_content_is_dropped() {
        let oversized = "x".repeat(MAX_ENTRY_CONTENT_BYTES + 1);
        let json = serde_json::json!({
            "entries": [
                {"content": oversized, "constant": true},
                {"content": "valid", "constant": true}
            ]
        });
        let entries = parse_lorebook(&json.to_string()).expect("parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "valid");
    }

    #[test]
    fn oversized_file_is_rejected_before_parsing() {
        let oversized = " ".repeat(MAX_LOREBOOK_FILE_BYTES + 1);
        assert!(parse_lorebook(&oversized).is_err());
    }

    #[test]
    fn selective_logic_values_are_resolved() {
        let json = r#"[
            {"content": "a", "selectiveLogic": 0},
            {"content": "b", "selectiveLogic": 1},
            {"content": "c", "selectiveLogic": 2},
            {"content": "d", "selectiveLogic": 3},
            {"content": "e", "selectiveLogic": 99}
        ]"#;
        let entries = parse_lorebook(json).expect("parse");
        let logic: Vec<_> = entries.iter().map(|entry| entry.selective_logic).collect();
        assert_eq!(
            logic,
            vec![
                SelectiveLogic::AndAny,
                SelectiveLogic::NotAll,
                SelectiveLogic::NotAny,
                SelectiveLogic::AndAll,
                SelectiveLogic::AndAny,
            ]
        );
    }

    #[test]
    fn malformed_json_errors() {
        assert!(parse_lorebook("not json").is_err());
        assert!(parse_lorebook(r#"{"entries": 42}"#).is_err());
    }
}
