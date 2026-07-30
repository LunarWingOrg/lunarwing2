//! Lorebook data model with SillyTavern World Info compatibility.
//!
//! Parses both the SillyTavern export shape (`{"entries": {"0": {...}}}` with
//! `key`/`keysecondary`/`disable` fields) and a simpler native shape
//! (`{"entries": [{...}]}` with `keys`/`secondary_keys`/`enabled` fields).
//! Unknown fields are ignored so newer SillyTavern exports keep parsing.

use std::collections::HashMap;

use serde::Deserialize;

/// Errors produced when parsing a lorebook file.
#[derive(Debug, thiserror::Error)]
pub enum LorebookError {
    /// The file was not valid JSON or did not match a known lorebook shape.
    #[error("invalid lorebook JSON: {0}")]
    Parse(String),
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
    /// Insertion order; lower values appear earlier in the prompt block.
    /// SillyTavern default is 100.
    pub order: i64,
    /// Activation probability in percent, 0.0–100.0. 100 always activates.
    pub probability: f64,
    /// Case-sensitive keyword matching. Default false (SillyTavern default).
    pub case_sensitive: Option<bool>,
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
    #[serde(default, alias = "insertion_order")]
    order: Option<i64>,
    #[serde(default)]
    probability: Option<f64>,
    #[serde(default, alias = "useProbability")]
    use_probability: Option<bool>,
    #[serde(default, alias = "caseSensitive")]
    case_sensitive: Option<bool>,
    #[serde(default, alias = "scanDepth")]
    scan_depth: Option<usize>,
}

impl RawLorebookEntry {
    fn resolve(self) -> LorebookEntry {
        let enabled = self.enabled.unwrap_or(!self.disable.unwrap_or(false));
        // SillyTavern only applies probability when `useProbability` is true;
        // the native format treats any probability < 100 as active.
        let probability = match (self.use_probability, self.probability) {
            (Some(false), _) => 100.0,
            (_, Some(p)) => p.clamp(0.0, 100.0),
            _ => 100.0,
        };
        LorebookEntry {
            name: self.name.filter(|n| !n.trim().is_empty()),
            keys: self
                .keys
                .into_vec()
                .into_iter()
                .filter(|k| !k.trim().is_empty())
                .collect(),
            secondary_keys: self
                .secondary_keys
                .into_vec()
                .into_iter()
                .filter(|k| !k.trim().is_empty())
                .collect(),
            content: self.content,
            enabled,
            constant: self.constant,
            selective: self.selective,
            order: self.order.unwrap_or(100),
            probability,
            case_sensitive: self.case_sensitive,
            scan_depth: self.scan_depth,
        }
    }
}

/// SillyTavern exports entries as an object keyed by stringified ids; the
/// native format uses a plain array.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EntriesRepr {
    Map(HashMap<String, RawLorebookEntry>),
    List(Vec<RawLorebookEntry>),
}

impl EntriesRepr {
    fn into_vec(self) -> Vec<RawLorebookEntry> {
        match self {
            Self::Map(m) => m.into_values().collect(),
            Self::List(v) => v,
        }
    }
}

/// A lorebook file is either an object with an `entries` field (SillyTavern
/// export, optionally with metadata like `name`) or a bare entries
/// array/object (native shorthand).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LorebookFileRepr {
    Structured { entries: EntriesRepr },
    Bare(EntriesRepr),
}

/// Parse a lorebook file into resolved entries.
///
/// Entries with empty content are dropped; all other validation is lenient.
pub fn parse_lorebook(content: &str) -> Result<Vec<LorebookEntry>, LorebookError> {
    let repr: LorebookFileRepr =
        serde_json::from_str(content).map_err(|e| LorebookError::Parse(e.to_string()))?;
    let raw = match repr {
        LorebookFileRepr::Structured { entries } | LorebookFileRepr::Bare(entries) => entries,
    };
    Ok(raw
        .into_vec()
        .into_iter()
        .map(RawLorebookEntry::resolve)
        .filter(|e| !e.content.trim().is_empty())
        .collect())
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
                    "order": 50,
                    "position": 0,
                    "disable": false,
                    "probability": 80,
                    "useProbability": true,
                    "scanDepth": 4,
                    "caseSensitive": true,
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
        assert!(!e.constant);
        assert_eq!(e.order, 50);
        assert!((e.probability - 80.0).abs() < f64::EPSILON);
        assert_eq!(e.scan_depth, Some(4));
        assert_eq!(e.case_sensitive, Some(true));
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
    fn malformed_json_errors() {
        assert!(parse_lorebook("not json").is_err());
        assert!(parse_lorebook(r#"{"entries": 42}"#).is_err());
    }
}
