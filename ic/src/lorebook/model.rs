//! Lorebook data model and bounded SillyTavern World Info import subset.

use std::cmp::Ordering;

use serde::Deserialize;
use serde_json::Value;

/// Maximum bytes accepted in one lorebook JSON document.
pub const MAX_LOREBOOK_FILE_BYTES: usize = 1024 * 1024;
/// Maximum entries accepted from one document.
pub const MAX_ENTRIES_PER_FILE: usize = 512;
/// Maximum primary or secondary keys retained on one entry.
pub const MAX_KEYS_PER_ENTRY: usize = 64;
/// Maximum bytes accepted in one trigger key.
pub const MAX_KEY_BYTES: usize = 256;
/// Maximum bytes accepted in an entry label.
pub const MAX_NAME_BYTES: usize = 512;
/// Maximum bytes accepted in one entry's prompt content.
pub const MAX_ENTRY_CONTENT_BYTES: usize = 64 * 1024;
/// Maximum per-entry message scan depth.
pub const MAX_ENTRY_SCAN_DEPTH: usize = 100;

/// Errors produced while parsing a lorebook document.
#[derive(Debug, thiserror::Error)]
pub enum LorebookError {
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
    fn from_sillytavern(value: Option<u8>) -> Result<Self, &'static str> {
        match value {
            None | Some(0) => Ok(Self::AndAny),
            Some(1) => Ok(Self::NotAll),
            Some(2) => Ok(Self::NotAny),
            Some(3) => Ok(Self::AndAll),
            Some(_) => Err("unknown selectiveLogic value"),
        }
    }
}

/// One resolved lorebook entry.
#[derive(Debug, Clone, PartialEq)]
pub struct LorebookEntry {
    /// Stable source document path, used for deterministic ties and probability.
    pub source_path: String,
    /// Stable source entry identifier (SillyTavern map key or native list index).
    pub entry_id: String,
    /// Deterministic entry order within the parsed source document.
    pub source_index: usize,
    /// Display label (`comment` in SillyTavern, `name` in native documents).
    pub name: Option<String>,
    /// Primary trigger keys (`key` in SillyTavern).
    pub keys: Vec<String>,
    /// Secondary optional-filter keys (`keysecondary` in SillyTavern).
    pub secondary_keys: Vec<String>,
    /// Reference text injected after activation.
    pub content: String,
    /// Whether this entry can activate. Inverse of SillyTavern `disable`.
    pub enabled: bool,
    /// Activate without requiring primary keys.
    pub constant: bool,
    /// Apply optional-filter keys.
    pub selective: bool,
    /// Optional-filter behavior.
    pub selective_logic: SelectiveLogic,
    /// Insertion order. Larger values receive budget priority and render later.
    pub order: i64,
    /// Activation probability percentage.
    pub probability: f64,
    /// Case-sensitive plaintext key matching override.
    pub case_sensitive: Option<bool>,
    /// Whole-word plaintext key matching override.
    pub match_whole_words: Option<bool>,
    /// Per-entry message scan-depth override.
    pub scan_depth: Option<usize>,
}

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
            Self::One(value) => vec![value],
            Self::Many(values) => values,
        }
    }
}

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
    fn resolve(
        self,
        source_path: &str,
        entry_id: String,
        source_index: usize,
    ) -> Result<Option<LorebookEntry>, &'static str> {
        if self.content.trim().is_empty() {
            return Ok(None);
        }
        if self.content.len() > MAX_ENTRY_CONTENT_BYTES {
            return Err("content exceeds byte limit");
        }
        let name = self.name.and_then(|name| {
            if name.trim().is_empty() {
                None
            } else if name.len() > MAX_NAME_BYTES {
                tracing::warn!(
                    path = source_path,
                    entry = %entry_id,
                    limit = MAX_NAME_BYTES,
                    "Discarding oversized lorebook entry name"
                );
                None
            } else {
                Some(name)
            }
        });
        let keys = clean_keys(source_path, &entry_id, "primary", self.keys);
        let secondary_keys = clean_keys(source_path, &entry_id, "secondary", self.secondary_keys);
        let scan_depth = self.scan_depth.map(|depth| {
            if depth > MAX_ENTRY_SCAN_DEPTH {
                tracing::warn!(
                    path = source_path,
                    entry = %entry_id,
                    requested = depth,
                    limit = MAX_ENTRY_SCAN_DEPTH,
                    "Clamping lorebook entry scanDepth"
                );
            }
            depth.min(MAX_ENTRY_SCAN_DEPTH)
        });
        // JSON parsing cannot produce NaN, so clamping cannot panic.
        let probability = match (self.use_probability, self.probability) {
            (Some(false), _) => 100.0,
            (_, Some(value)) => value.clamp(0.0, 100.0),
            _ => 100.0,
        };

        Ok(Some(LorebookEntry {
            source_path: source_path.to_string(),
            entry_id,
            source_index,
            name,
            keys,
            secondary_keys,
            content: self.content,
            enabled: self.enabled.unwrap_or(!self.disable.unwrap_or(false)),
            constant: self.constant,
            selective: self.selective,
            selective_logic: SelectiveLogic::from_sillytavern(self.selective_logic)?,
            order: self.order.unwrap_or(100),
            probability,
            case_sensitive: self.case_sensitive,
            match_whole_words: self.match_whole_words,
            scan_depth,
        }))
    }
}

fn clean_keys(source_path: &str, entry_id: &str, field: &str, keys: StringList) -> Vec<String> {
    let mut cleaned = Vec::new();
    let mut empty = 0usize;
    let mut oversized = 0usize;
    let mut regex = 0usize;
    let mut excess = 0usize;
    for key in keys.into_vec() {
        if key.trim().is_empty() {
            empty += 1;
            continue;
        }
        if key.len() > MAX_KEY_BYTES {
            oversized += 1;
            continue;
        }
        if looks_like_javascript_regex(&key) {
            regex += 1;
            continue;
        }
        if cleaned.len() == MAX_KEYS_PER_ENTRY {
            excess += 1;
            continue;
        }
        cleaned.push(key);
    }
    if empty + oversized + regex + excess > 0 {
        tracing::warn!(
            path = source_path,
            entry = %entry_id,
            field,
            dropped_empty = empty,
            dropped_oversized = oversized,
            dropped_regex = regex,
            dropped_excess = excess,
            "Discarding invalid or excess lorebook keys"
        );
    }
    cleaned
}

/// SillyTavern `/pattern/flags` keys are unsupported; detect so they are
/// dropped rather than silently matched as literal slashes.
fn looks_like_javascript_regex(key: &str) -> bool {
    let Some(rest) = key.strip_prefix('/') else {
        return false;
    };
    let Some(end) = rest.rfind('/') else {
        return false;
    };
    rest[end + 1..].chars().all(|ch| ch.is_ascii_alphabetic())
}

/// SillyTavern object keys are insertion-ordered in practice; numeric keys
/// compare numerically so "10" sorts after "2" deterministically.
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

/// Parse a bounded SillyTavern export or native lorebook document.
///
/// Malformed or unsupported individual entries are skipped with warnings so a
/// legacy document can retain valid siblings. Invalid outer JSON/shape rejects
/// the complete document.
pub fn parse_lorebook(
    source_path: &str,
    content: &str,
) -> Result<Vec<LorebookEntry>, LorebookError> {
    if content.len() > MAX_LOREBOOK_FILE_BYTES {
        return Err(LorebookError::Parse(format!(
            "file exceeds {MAX_LOREBOOK_FILE_BYTES} byte limit"
        )));
    }

    let value: Value =
        serde_json::from_str(content).map_err(|error| LorebookError::Parse(error.to_string()))?;
    let entries = match value {
        Value::Object(mut object) if object.contains_key("entries") => {
            object.remove("entries").unwrap_or(Value::Null)
        }
        value => value,
    };
    let values = into_entry_values(entries)?;
    if values.len() > MAX_ENTRIES_PER_FILE {
        tracing::warn!(
            path = source_path,
            count = values.len(),
            limit = MAX_ENTRIES_PER_FILE,
            "Lorebook entry limit reached; ignoring remaining entries"
        );
    }

    let mut parsed = Vec::new();
    for (source_index, (entry_id, value)) in
        values.into_iter().take(MAX_ENTRIES_PER_FILE).enumerate()
    {
        match serde_json::from_value::<RawLorebookEntry>(value) {
            Ok(raw) => match raw.resolve(source_path, entry_id.clone(), source_index) {
                Ok(Some(entry)) => parsed.push(entry),
                Ok(None) => {}
                Err(reason) => tracing::warn!(
                    path = source_path,
                    entry = %entry_id,
                    reason,
                    "Skipping unsupported or oversized lorebook entry"
                ),
            },
            Err(error) => tracing::warn!(
                path = source_path,
                entry = %entry_id,
                %error,
                "Skipping malformed lorebook entry"
            ),
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sillytavern_subset_with_stable_identity() {
        let json = r#"{
            "entries": {
                "10": {"key": ["wyrm"], "content": "Ten", "selectiveLogic": 3},
                "2": {
                    "key": ["dragon"], "keysecondary": ["fire"],
                    "comment": "Dragon lore", "content": "Two", "selective": true,
                    "disable": false, "probability": 80, "useProbability": true,
                    "scanDepth": 4, "caseSensitive": true, "matchWholeWords": true
                }
            }
        }"#;
        let entries = parse_lorebook("lorebooks/world.json", json).expect("parse");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entry_id, "2");
        assert_eq!(entries[0].source_index, 0);
        assert_eq!(entries[0].source_path, "lorebooks/world.json");
        assert_eq!(entries[0].name.as_deref(), Some("Dragon lore"));
        assert_eq!(entries[0].selective_logic, SelectiveLogic::AndAny);
        assert_eq!(entries[0].scan_depth, Some(4));
        assert_eq!(entries[0].case_sensitive, Some(true));
        assert_eq!(entries[0].match_whole_words, Some(true));
        assert_eq!(entries[1].entry_id, "10");
        assert_eq!(entries[1].selective_logic, SelectiveLogic::AndAll);
    }

    #[test]
    fn parses_native_list_and_single_string_key() {
        let json = r#"{"entries":[
            {"keys":"moon","content":"The moon is hollow."},
            {"content":"Always active.","constant":true}
        ]}"#;
        let entries = parse_lorebook("lorebooks/native.json", json).expect("parse");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].keys, vec!["moon"]);
        assert_eq!(entries[1].entry_id, "1");
        assert!(entries[1].constant);
    }

    #[test]
    fn malformed_entries_do_not_drop_salvageable_siblings() {
        let json = r#"{"entries":{
            "0":{"content":"valid","constant":true},
            "1":{"content":"bad","key":42},
            "2":{"content":"regex","key":["/dragon/i"]},
            "3":{"content":"unknown logic","selectiveLogic":99},
            "4":{"content":"deep scan","scanDepth":101},
            "5":{"content":"also valid","constant":true}
        }}"#;
        let entries = parse_lorebook("lorebooks/mixed.json", json).expect("outer parse");
        let contents: Vec<_> = entries.iter().map(|entry| entry.content.as_str()).collect();
        assert_eq!(contents, vec!["valid", "regex", "deep scan", "also valid"]);
        assert!(entries[1].keys.is_empty());
        assert_eq!(entries[2].scan_depth, Some(MAX_ENTRY_SCAN_DEPTH));
    }

    #[test]
    fn salvages_optional_fields_and_truncates_valid_keys() {
        let mut keys = vec![
            serde_json::json!(""),
            serde_json::json!("k".repeat(MAX_KEY_BYTES + 1)),
            serde_json::json!("/dragon/i"),
            serde_json::json!("good"),
        ];
        keys.extend((0..70).map(|index| serde_json::json!(format!("key-{index:02}"))));
        let json = serde_json::json!({
            "entries": [{
                "name": "n".repeat(MAX_NAME_BYTES + 1),
                "keys": keys,
                "secondary_keys": ["", "s".repeat(MAX_KEY_BYTES + 1), "/fire/i", "fire"],
                "content": "salvaged",
                "scanDepth": MAX_ENTRY_SCAN_DEPTH + 50
            }]
        });

        let entries = parse_lorebook("lorebooks/salvage.json", &json.to_string()).expect("parse");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.content, "salvaged");
        assert!(entry.name.is_none());
        assert_eq!(entry.keys.len(), MAX_KEYS_PER_ENTRY);
        assert_eq!(entry.keys.first().map(String::as_str), Some("good"));
        assert_eq!(entry.keys.last().map(String::as_str), Some("key-62"));
        assert_eq!(entry.secondary_keys, vec!["fire"]);
        assert_eq!(entry.scan_depth, Some(MAX_ENTRY_SCAN_DEPTH));
    }

    #[test]
    fn disabled_and_probability_fields_resolve() {
        let json = r#"[
            {"content":"disabled","disable":true},
            {"content":"ignored probability","probability":5,"useProbability":false},
            {"content":"clamped","probability":200,"useProbability":true}
        ]"#;
        let entries = parse_lorebook("lorebooks/fields.json", json).expect("parse");
        assert!(!entries[0].enabled);
        assert_eq!(entries[1].probability, 100.0);
        assert_eq!(entries[2].probability, 100.0);
    }

    #[test]
    fn invalid_outer_shape_and_oversized_file_fail() {
        assert!(parse_lorebook("x.json", r#"{"entries":42}"#).is_err());
        let oversized = " ".repeat(MAX_LOREBOOK_FILE_BYTES + 1);
        assert!(parse_lorebook("x.json", &oversized).is_err());
    }
}
