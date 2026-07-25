//! Cavepony Rust/WASM tool for LunarWing.
//!
//! Ports Cavepony v0.3.0's phrase tokens and style dictionaries, plus a local
//! Tony Soprano-inspired mode, while keeping code, quoted text, and URLs outside
//! transformation. No host capabilities are imported beyond the standard WIT
//! interface.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

wit_bindgen::generate!({
    world: "sandboxed-tool",
    path: "../../wit/tool.wit",
});

use exports::lunarwing::agent::tool;

const MAX_INPUT_CHARACTERS: usize = 1536;
const MAX_INPUT_BYTES: usize = MAX_INPUT_CHARACTERS * 4;

// Keep the MIT notice in binary-only WASM distributions. Installed tools also
// carry the same notice in their capabilities sidecar.
#[used]
static CAVEPONY_LICENSE: [u8; include_bytes!("../LICENSE").len()] = *include_bytes!("../LICENSE");

const SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "action": {
      "type": "string",
      "enum": ["compress", "expand", "stats"],
      "description": "Operation to perform. stats compresses text and returns detailed size estimates."
    },
    "text": {
      "type": "string",
      "maxLength": 1536,
      "description": "Text to transform or measure. Limit is 1,536 Unicode characters (at most 6 KiB UTF-8) so expanded output remains within LunarWing's tool-output limit."
    },
    "mode": {
      "type": "string",
      "enum": ["tokens", "lite", "full", "ultra", "pony", "canterlot", "tony-soprano"],
      "default": "full",
      "description": "Compression or speech style. Ignored by expand. tokens only replaces known phrases; canterlot expands prose; tony-soprano applies contractions and blunt phrasing."
    }
  },
  "required": ["action", "text"],
  "additionalProperties": false
}"#;

struct CaveponyTool;

export!(CaveponyTool);

impl tool::Guest for CaveponyTool {
    fn execute(req: tool::Request) -> tool::Response {
        match execute_inner(&req.params) {
            Ok(output) => tool::Response {
                output: Some(output),
                error: None,
            },
            Err(error) => tool::Response {
                output: None,
                error: Some(error),
            },
        }
    }

    fn schema() -> String {
        SCHEMA.to_string()
    }

    fn description() -> String {
        "Compress, expand, or measure prose with Cavepony. Modes: tokens (phrase tokens only), lite, full, ultra, pony, canterlot, and tony-soprano. Fenced code, inline code, quoted text, and HTTP(S) URLs remain unchanged. Expansion restores canonical token phrases only; destructive compression and style substitutions are not reversible. Token counts are model-independent estimates."
            .to_string()
    }
}

#[derive(Debug, Deserialize)]
struct Params {
    action: String,
    text: String,
    mode: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Tokens,
    Lite,
    Full,
    Ultra,
    Pony,
    Canterlot,
    TonySoprano,
}

impl Mode {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("full").to_ascii_lowercase().as_str() {
            "tokens" => Ok(Self::Tokens),
            "lite" => Ok(Self::Lite),
            "full" => Ok(Self::Full),
            "ultra" => Ok(Self::Ultra),
            "pony" => Ok(Self::Pony),
            "canterlot" => Ok(Self::Canterlot),
            "tony-soprano" => Ok(Self::TonySoprano),
            other => Err(format!(
                "Invalid mode '{other}': expected tokens, lite, full, ultra, pony, canterlot, or tony-soprano"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Tokens => "tokens",
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
            Self::Pony => "pony",
            Self::Canterlot => "canterlot",
            Self::TonySoprano => "tony-soprano",
        }
    }

    fn drops_filler(self) -> bool {
        matches!(self, Self::Lite | Self::Full | Self::Ultra | Self::Pony)
    }

    fn drops_articles(self) -> bool {
        matches!(self, Self::Full | Self::Ultra | Self::Pony)
    }

    fn uses_destructive_rules(self) -> bool {
        !matches!(self, Self::Tokens)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct TokenMapping {
    phrase: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct TokenDictionary {
    mappings: Vec<TokenMapping>,
}

#[derive(Debug, Deserialize)]
struct RawSubstitution {
    pattern: String,
    replacement: String,
}

#[derive(Debug, Deserialize)]
struct SubstitutionDictionary {
    substitutions: Vec<RawSubstitution>,
}

struct CompiledSubstitution {
    pattern: Vec<String>,
    replacement: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct Metrics {
    original_bytes: usize,
    result_bytes: usize,
    original_characters: usize,
    result_characters: usize,
    original_words: usize,
    result_words: usize,
    estimated_original_tokens: usize,
    estimated_result_tokens: usize,
    byte_reduction_percent: f64,
    character_reduction_percent: f64,
    word_reduction_percent: f64,
    byte_based_token_estimate_reduction_percent: f64,
}

fn execute_inner(params_json: &str) -> Result<String, String> {
    let params: Params = serde_json::from_str(params_json)
        .map_err(|error| format!("Invalid parameters: {error}"))?;
    validate_input(&params.text)?;

    let output = match params.action.to_ascii_lowercase().as_str() {
        "compress" => {
            let mode = Mode::parse(params.mode.as_deref())?;
            let result = compress(&params.text, mode);
            serde_json::json!({
                "action": "compress",
                "mode": mode.as_str(),
                "text": result,
                "lossy": true,
                "uses_destructive_rules": mode.uses_destructive_rules(),
                "stats": metrics(&params.text, &result),
                "expansion_scope": "Expansion is canonical, not an exact round trip: shared or preexisting tokens, case, destructive transforms, and style transforms are lossy."
            })
        }
        "expand" => {
            let result = expand(&params.text);
            serde_json::json!({
                "action": "expand",
                "text": result,
                "canonical": true,
                "stats": metrics(&params.text, &result),
                "expansion_scope": "Only recognized Cavepony tokens were expanded; shared tokens use their first canonical phrase."
            })
        }
        "stats" => {
            let mode = Mode::parse(params.mode.as_deref())?;
            let result = compress(&params.text, mode);
            serde_json::json!({
                "action": "stats",
                "mode": mode.as_str(),
                "compressed_text": result,
                "lossy": true,
                "uses_destructive_rules": mode.uses_destructive_rules(),
                "stats": metrics(&params.text, &result),
                "token_estimate": "Four UTF-8 bytes per token; actual counts depend on the model tokenizer."
            })
        }
        other => {
            return Err(format!(
                "Invalid action '{other}': expected compress, expand, or stats"
            ));
        }
    };

    serde_json::to_string(&output).map_err(|error| format!("Failed to serialize output: {error}"))
}

fn validate_input(text: &str) -> Result<(), String> {
    let characters = text.chars().count();
    if characters > MAX_INPUT_CHARACTERS {
        return Err(format!(
            "'text' exceeds maximum length of {MAX_INPUT_CHARACTERS} Unicode characters"
        ));
    }
    if text.len() > MAX_INPUT_BYTES {
        return Err(format!(
            "'text' exceeds maximum UTF-8 size of {MAX_INPUT_BYTES} bytes"
        ));
    }
    Ok(())
}

fn compress(text: &str, mode: Mode) -> String {
    if text.is_empty() {
        return String::new();
    }

    if mode == Mode::Canterlot {
        return transform_preserving_literals(text, |segment| {
            let pony = apply_substitutions(segment, pony_substitutions());
            apply_substitutions(&pony, canterlot_substitutions())
        });
    }

    if mode == Mode::TonySoprano {
        return transform_preserving_literals(text, |segment| {
            apply_substitutions(segment, tony_soprano_substitutions())
        });
    }

    transform_preserving_literals(text, |segment| {
        let mut result = if mode == Mode::Pony {
            apply_substitutions(segment, pony_substitutions())
        } else {
            segment.to_string()
        };
        result = apply_phrase_tokens(&result);
        if mode.drops_filler() {
            result = drop_filler(&result);
        }
        if mode.drops_articles() {
            result = drop_articles(&result);
        }
        if mode == Mode::Ultra {
            result = abbreviate_ultra(&result);
        }
        cleanup_segment(&result)
    })
}

fn expand(text: &str) -> String {
    transform_preserving_literals(text, |segment| {
        let mut result = segment.to_string();
        for mapping in expansion_mappings() {
            result = replace_token(&result, &mapping.token, &mapping.phrase);
        }
        result
    })
}

fn apply_phrase_tokens(text: &str) -> String {
    let mut result = text.to_string();
    for mapping in phrase_mappings() {
        result = replace_bounded_literal(&result, &mapping.phrase, &mapping.token);
    }
    result
}

fn apply_substitutions(text: &str, substitutions: &[CompiledSubstitution]) -> String {
    let mut result = text.to_string();
    for substitution in substitutions {
        result = replace_bounded_pattern(&result, &substitution.pattern, &substitution.replacement);
    }
    result
}

fn preserve_initial_case(source: &str, replacement: &str) -> String {
    if source.chars().next().is_some_and(char::is_uppercase) {
        let mut chars = replacement.chars();
        if let Some(first) = chars.next() {
            return first.to_uppercase().collect::<String>() + chars.as_str();
        }
    }
    replacement.to_string()
}

fn drop_filler(text: &str) -> String {
    let mut result = text.to_string();
    for (phrase, allow_bang) in [
        ("sure", true),
        ("absolutely", true),
        ("thank you", false),
        ("thanks", false),
        ("would you mind", false),
        ("could you", false),
        ("would you", false),
        ("if you could", false),
        ("if you would", false),
    ] {
        let require_end_boundary = !matches!(phrase, "sure" | "absolutely");
        result = remove_bounded_phrase(&result, phrase, require_end_boundary, allow_bang, false);
    }
    result
}

fn drop_articles(text: &str) -> String {
    let mut result = text.to_string();
    for article in ["a", "an", "the"] {
        result = remove_bounded_phrase(&result, article, true, false, true);
    }
    remove_bounded_phrase(&result, "just", true, false, false)
}

fn abbreviate_ultra(text: &str) -> String {
    let mut result = text.to_string();
    for word in ["am", "is", "are", "was", "were", "be", "been", "being"] {
        result = remove_bounded_phrase(&result, word, true, false, true);
    }
    for (word, replacement) in ultra_patterns() {
        result = replace_bounded_literal(&result, word, replacement);
    }
    result
}

fn cleanup_segment(text: &str) -> String {
    let preserve_leading = text.chars().next().is_some_and(char::is_whitespace);
    let preserve_trailing = text.chars().next_back().is_some_and(char::is_whitespace);
    let collapsed = collapse_horizontal_whitespace(text);
    let collapsed = remove_whitespace_before_punctuation(&collapsed);
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return if preserve_leading || preserve_trailing {
            " ".to_string()
        } else {
            String::new()
        };
    }

    let mut result = String::with_capacity(trimmed.len() + 2);
    if preserve_leading {
        result.push(' ');
    }
    result.push_str(trimmed);
    if preserve_trailing {
        result.push(' ');
    }
    result
}

fn replace_bounded_literal(text: &str, literal: &str, replacement: &str) -> String {
    replace_bounded_matches(text, replacement, false, |start| {
        bounded_literal_end(text, start, literal)
    })
}

fn replace_bounded_pattern(text: &str, pattern: &[String], replacement: &str) -> String {
    replace_bounded_matches(text, replacement, true, |start| {
        bounded_pattern_end(text, start, pattern)
    })
}

fn replace_bounded_matches<F>(
    text: &str,
    replacement: &str,
    preserve_case: bool,
    mut match_end: F,
) -> String
where
    F: FnMut(usize) -> Option<usize>,
{
    let mut result = String::with_capacity(text.len());
    let mut copied_through = 0;
    let mut cursor = 0;

    while cursor < text.len() {
        if let Some(end) = match_end(cursor) {
            result.push_str(&text[copied_through..cursor]);
            if preserve_case {
                result.push_str(&preserve_initial_case(&text[cursor..end], replacement));
            } else {
                result.push_str(replacement);
            }
            cursor = end;
            copied_through = end;
        } else {
            cursor += text[cursor..].chars().next().map_or(1, char::len_utf8);
        }
    }
    result.push_str(&text[copied_through..]);
    result
}

fn bounded_literal_end(text: &str, start: usize, literal: &str) -> Option<usize> {
    let end = leading_bounded_literal_end(text, start, literal)?;
    if text[end..].chars().next().is_some_and(is_word_character) {
        return None;
    }
    Some(end)
}

fn leading_bounded_literal_end(text: &str, start: usize, literal: &str) -> Option<usize> {
    if text[..start]
        .chars()
        .next_back()
        .is_some_and(is_word_character)
    {
        return None;
    }
    literal_match_end(text, start, literal)
}

fn literal_match_end(text: &str, start: usize, literal: &str) -> Option<usize> {
    let mut cursor = start;
    for expected in literal.chars() {
        let actual = text[cursor..].chars().next()?;
        if !characters_equal_ignoring_case(actual, expected) {
            return None;
        }
        cursor += actual.len_utf8();
    }
    Some(cursor)
}

fn characters_equal_ignoring_case(left: char, right: char) -> bool {
    if !right.is_ascii_alphabetic() {
        return left == right;
    }
    let lower = right.to_ascii_lowercase();
    left == lower
        || left == lower.to_ascii_uppercase()
        || (lower == 'k' && left == 'K')
        || (lower == 's' && left == 'ſ')
}

fn bounded_pattern_end(text: &str, start: usize, pattern: &[String]) -> Option<usize> {
    if text[..start]
        .chars()
        .next_back()
        .is_some_and(is_word_character)
    {
        return None;
    }

    let mut cursor = start;
    for (index, part) in pattern.iter().enumerate() {
        if index > 0 {
            let whitespace_start = cursor;
            while let Some(character) = text[cursor..]
                .chars()
                .next()
                .filter(|character| character.is_whitespace())
            {
                cursor += character.len_utf8();
            }
            if cursor == whitespace_start {
                return None;
            }
        }
        cursor = literal_match_end(text, cursor, part)?;
    }

    text[cursor..]
        .chars()
        .next()
        .is_none_or(|character| !is_word_character(character))
        .then_some(cursor)
}

fn remove_bounded_phrase(
    text: &str,
    phrase: &str,
    require_end_boundary: bool,
    allow_bang: bool,
    require_trailing_whitespace: bool,
) -> String {
    replace_bounded_matches(text, "", false, |start| {
        let mut end = if require_end_boundary {
            bounded_literal_end(text, start, phrase)?
        } else {
            leading_bounded_literal_end(text, start, phrase)?
        };
        if allow_bang && text[end..].starts_with('!') {
            end += 1;
        }
        let whitespace_start = end;
        while let Some(character) = text[end..]
            .chars()
            .next()
            .filter(|character| character.is_whitespace())
        {
            end += character.len_utf8();
        }
        if require_trailing_whitespace && end == whitespace_start {
            return None;
        }
        Some(end)
    })
}

fn collapse_horizontal_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if !matches!(character, ' ' | '\t') {
            result.push(character);
            continue;
        }

        let mut run = 1;
        while characters
            .next_if(|candidate| matches!(candidate, ' ' | '\t'))
            .is_some()
        {
            run += 1;
        }
        if run == 1 {
            result.push(character);
        } else {
            result.push(' ');
        }
    }
    result
}

fn remove_whitespace_before_punctuation(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, ',' | '.' | ';' | ':' | '!' | '?') {
            while result.ends_with([' ', '\t']) {
                result.pop();
            }
        }
        result.push(character);
    }
    result
}

fn metrics(original: &str, result: &str) -> Metrics {
    let original_bytes = original.len();
    let result_bytes = result.len();
    let original_characters = original.chars().count();
    let result_characters = result.chars().count();
    let original_words = word_count(original);
    let result_words = word_count(result);
    let estimated_original_tokens = estimate_tokens(original);
    let estimated_result_tokens = estimate_tokens(result);

    Metrics {
        original_bytes,
        result_bytes,
        original_characters,
        result_characters,
        original_words,
        result_words,
        estimated_original_tokens,
        estimated_result_tokens,
        byte_reduction_percent: reduction_percent(original_bytes, result_bytes),
        character_reduction_percent: reduction_percent(original_characters, result_characters),
        word_reduction_percent: reduction_percent(original_words, result_words),
        byte_based_token_estimate_reduction_percent: reduction_percent(
            estimated_original_tokens,
            estimated_result_tokens,
        ),
    }
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

fn reduction_percent(original: usize, result: usize) -> f64 {
    if original == 0 {
        return 0.0;
    }
    let percent = (1.0 - result as f64 / original as f64) * 100.0;
    (percent * 100.0).round() / 100.0
}

fn token_dictionary() -> TokenDictionary {
    // Safety: include_str! embeds repository-controlled JSON validated by tests and jq.
    serde_json::from_str(include_str!("../token-dict.json"))
        .expect("embedded Cavepony token dictionary must be valid JSON")
}

fn substitution_dictionary(source: &str) -> SubstitutionDictionary {
    // Safety: callers pass repository-controlled include_str! data validated by tests and jq.
    serde_json::from_str(source)
        .expect("embedded Cavepony substitution dictionary must be valid JSON")
}

fn phrase_mappings() -> &'static [TokenMapping] {
    static MAPPINGS: OnceLock<Vec<TokenMapping>> = OnceLock::new();
    MAPPINGS.get_or_init(|| {
        let mut mappings = token_dictionary().mappings;
        mappings.sort_by_key(|mapping| std::cmp::Reverse(mapping.phrase.len()));
        mappings
    })
}

fn expansion_mappings() -> &'static [TokenMapping] {
    static MAPPINGS: OnceLock<Vec<TokenMapping>> = OnceLock::new();
    MAPPINGS.get_or_init(|| {
        let mut seen = HashSet::new();
        let mut mappings: Vec<_> = token_dictionary()
            .mappings
            .into_iter()
            .filter(|mapping| seen.insert(mapping.token.clone()))
            .collect();
        mappings.sort_by_key(|mapping| std::cmp::Reverse(mapping.token.len()));
        mappings
    })
}

fn pony_substitutions() -> &'static [CompiledSubstitution] {
    static SUBSTITUTIONS: OnceLock<Vec<CompiledSubstitution>> = OnceLock::new();
    SUBSTITUTIONS.get_or_init(|| {
        compile_substitutions(substitution_dictionary(include_str!("../pony-dict.json")))
    })
}

fn canterlot_substitutions() -> &'static [CompiledSubstitution] {
    static SUBSTITUTIONS: OnceLock<Vec<CompiledSubstitution>> = OnceLock::new();
    SUBSTITUTIONS.get_or_init(|| {
        compile_substitutions(substitution_dictionary(include_str!(
            "../canterlot-dict.json"
        )))
    })
}

fn tony_soprano_substitutions() -> &'static [CompiledSubstitution] {
    static SUBSTITUTIONS: OnceLock<Vec<CompiledSubstitution>> = OnceLock::new();
    SUBSTITUTIONS.get_or_init(|| {
        compile_substitutions(substitution_dictionary(include_str!(
            "../tony-soprano-dict.json"
        )))
    })
}

fn compile_substitutions(dictionary: SubstitutionDictionary) -> Vec<CompiledSubstitution> {
    dictionary
        .substitutions
        .into_iter()
        .map(|substitution| {
            let pattern = substitution
                .pattern
                .strip_prefix(r"\b")
                .and_then(|pattern| pattern.strip_suffix(r"\b"))
                .filter(|pattern| !pattern.is_empty())
                .expect("embedded Cavepony substitutions must use non-empty word boundaries");
            let parts: Vec<_> = pattern.split(r"\s+").map(str::to_string).collect();
            assert!(
                parts.iter().all(|part| {
                    !part.is_empty()
                        && !part.chars().any(|character| {
                            matches!(
                                character,
                                '\\' | '^'
                                    | '$'
                                    | '*'
                                    | '+'
                                    | '?'
                                    | '['
                                    | ']'
                                    | '('
                                    | ')'
                                    | '{'
                                    | '}'
                                    | '|'
                            )
                        })
                }),
                "embedded Cavepony substitutions must contain only bounded literals and \\s+"
            );
            CompiledSubstitution {
                pattern: parts,
                replacement: substitution.replacement,
            }
        })
        .collect()
}

fn ultra_patterns() -> &'static [(&'static str, &'static str)] {
    &[
        ("because", "bc"),
        ("approximately", "~"),
        ("important", "key"),
        ("problem", "bug"),
        ("issue", "bug"),
        ("configuration", "conf"),
        ("parameter", "param"),
        ("function", "fn"),
        ("variable", "var"),
        ("authentication", "auth"),
        ("information", "info"),
        ("application", "app"),
        ("environment", "env"),
        ("repository", "repo"),
        ("developer", "dev"),
        ("development", "dev"),
        ("production", "prod"),
        ("database", "db"),
        ("request", "req"),
        ("response", "res"),
        ("document", "doc"),
        ("reference", "ref"),
    ]
}

fn replace_token(text: &str, token: &str, phrase: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;

    while let Some(relative) = text[cursor..].find(token) {
        let start = cursor + relative;
        let end = start + token.len();
        if token_boundaries_match(text, start, end, token) {
            result.push_str(&text[cursor..start]);
            result.push_str(phrase);
            cursor = end;
        } else {
            result.push_str(&text[cursor..end]);
            cursor = end;
        }
    }
    result.push_str(&text[cursor..]);
    result
}

fn token_boundaries_match(text: &str, start: usize, end: usize, token: &str) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    let alpha_like = token
        .chars()
        .all(|character| character.is_alphanumeric() || character == '.');

    if alpha_like {
        return before.is_none_or(|character| !is_word_character(character))
            && after.is_none_or(|character| !is_word_character(character));
    }

    before.is_none_or(is_symbol_boundary) && after.is_none_or(is_symbol_boundary)
}

fn is_word_character(character: char) -> bool {
    if character.is_ascii() {
        return character.is_ascii_alphanumeric() || character == '_';
    }

    static WORD: OnceLock<regex_syntax::hir::ClassUnicode> = OnceLock::new();
    let word = WORD.get_or_init(|| {
        let hir = regex_syntax::Parser::new()
            .parse(r"\w")
            .expect("hardcoded Unicode Perl word class must parse");
        match hir.kind() {
            regex_syntax::hir::HirKind::Class(regex_syntax::hir::Class::Unicode(class)) => {
                class.clone()
            }
            _ => unreachable!("Unicode Perl word class must parse to a Unicode class"),
        }
    });
    word.ranges()
        .binary_search_by(|range| {
            if range.end() < character {
                std::cmp::Ordering::Less
            } else if range.start() > character {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn is_symbol_boundary(character: char) -> bool {
    !is_word_character(character)
}

#[derive(Clone, Copy)]
struct FenceMarker {
    character: char,
    length: usize,
}

fn transform_preserving_literals<F>(text: &str, mut transform: F) -> String
where
    F: FnMut(&str) -> String,
{
    let mut result = String::with_capacity(text.len());
    let mut fence: Option<FenceMarker> = None;

    for chunk in text.split_inclusive('\n') {
        let (line, newline) = chunk
            .strip_suffix('\n')
            .map_or((chunk, ""), |line| (line, "\n"));
        let trimmed = line.trim_start();

        if let Some(marker) = fence {
            result.push_str(line);
            if closes_fence(trimmed, marker) {
                fence = None;
            }
        } else if let Some(marker) = fence_marker(trimmed) {
            fence = Some(marker);
            result.push_str(line);
        } else {
            result.push_str(&transform_inline_literals(line, &mut transform));
        }
        result.push_str(newline);
    }

    result
}

fn fence_marker(line: &str) -> Option<FenceMarker> {
    let character = line.chars().next()?;
    if !matches!(character, '`' | '~') {
        return None;
    }
    let length = line
        .chars()
        .take_while(|candidate| *candidate == character)
        .count();
    (length >= 3).then_some(FenceMarker { character, length })
}

fn closes_fence(line: &str, marker: FenceMarker) -> bool {
    let length = line
        .chars()
        .take_while(|candidate| *candidate == marker.character)
        .count();
    length >= marker.length && line[length..].trim().is_empty()
}

fn transform_inline_literals<F>(line: &str, transform: &mut F) -> String
where
    F: FnMut(&str) -> String,
{
    let mut result = String::with_capacity(line.len());
    let mut cursor = 0;
    let mut plain_start = 0;

    while cursor < line.len() {
        let rest = &line[cursor..];
        let protected_end = if rest.starts_with('`') {
            Some(find_backtick_end(line, cursor))
        } else if let Some(end) = find_quote_end(line, cursor) {
            Some(end)
        } else if is_url_start(rest) {
            Some(find_url_end(line, cursor))
        } else {
            protected_token_end(line, cursor)
        };

        if let Some(end) = protected_end {
            result.push_str(&transform(&line[plain_start..cursor]));
            result.push_str(&line[cursor..end]);
            cursor = end;
            plain_start = end;
            continue;
        }

        cursor += rest.chars().next().map_or(1, char::len_utf8);
    }

    result.push_str(&transform(&line[plain_start..]));
    result
}

fn find_backtick_end(line: &str, start: usize) -> usize {
    let run = line[start..]
        .chars()
        .take_while(|character| *character == '`')
        .count();
    let delimiter = "`".repeat(run);
    let content_start = start + run;
    line[content_start..]
        .find(&delimiter)
        .map_or(line.len(), |relative| content_start + relative + run)
}

fn find_quote_end(line: &str, start: usize) -> Option<usize> {
    let opener = line[start..].chars().next()?;
    let closer = match opener {
        '"' => '"',
        '“' => '”',
        '‘' => '’',
        '\'' if quote_can_open(line, start) => '\'',
        _ => return None,
    };
    let mut escaped = false;
    let content_start = start + opener.len_utf8();
    for (relative, character) in line[content_start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
        } else if character == closer {
            return Some(content_start + relative + character.len_utf8());
        }
    }
    Some(line.len())
}

fn quote_can_open(line: &str, start: usize) -> bool {
    line[..start]
        .chars()
        .next_back()
        .is_none_or(|character| !is_word_character(character))
}

fn is_url_start(text: &str) -> bool {
    text.get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || text
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}

fn find_url_end(line: &str, start: usize) -> usize {
    for (relative, character) in line[start..].char_indices() {
        if character.is_whitespace() || matches!(character, '<' | '>' | '"') {
            return start + relative;
        }
    }
    line.len()
}

fn protected_token_end(line: &str, start: usize) -> Option<usize> {
    if !line[..start]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace)
    {
        return None;
    }

    let end = line[start..]
        .char_indices()
        .find_map(|(relative, character)| character.is_whitespace().then_some(start + relative))
        .unwrap_or(line.len());
    let token = line[start..end].trim_matches(|character: char| {
        matches!(
            character,
            ',' | ';' | ':' | '!' | '?' | '(' | ')' | '[' | ']' | '{' | '}'
        )
    });
    if token.is_empty() {
        return None;
    }

    let path_or_identifier = token.starts_with('/')
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with("~/")
        || token.starts_with("--")
        || token.contains('/')
        || token.contains('\\')
        || token.contains('_')
        || token.contains("::")
        || contains_internal_connector(token, '-')
        || contains_internal_connector(token, '.');
    path_or_identifier.then_some(end)
}

fn contains_internal_connector(token: &str, connector: char) -> bool {
    let characters: Vec<_> = token.chars().collect();
    characters.windows(3).any(|window| {
        is_word_character(window[0]) && window[1] == connector && is_word_character(window[2])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionaries_load_and_patterns_compile() {
        assert!(token_dictionary().mappings.len() > 80);
        assert!(phrase_mappings().len() > 80);
        assert!(pony_substitutions().len() > 80);
        assert!(canterlot_substitutions().len() > 90);
        assert!(tony_soprano_substitutions().len() > 50);
    }

    #[test]
    fn tokens_use_longest_phrase_first() {
        assert_eq!(
            compress("Please note that this works", Mode::Tokens),
            "NB this works"
        );
        assert_eq!(
            compress("due to the fact that rain", Mode::Tokens),
            "∵ rain"
        );
        assert_eq!(compress("Actually useful", Mode::Tokens), "act useful");
        assert_eq!(compress("Kind of useful", Mode::Tokens), "~ useful");
        assert_eq!(
            compress("actually\u{301} useful", Mode::Tokens),
            "actually\u{301} useful"
        );
        assert_eq!(
            compress("actually\u{200c} useful", Mode::Tokens),
            "actually\u{200c} useful"
        );
        assert_eq!(compress("ı think", Mode::Tokens), "ı think");
    }

    #[test]
    fn expand_uses_canonical_phrase_for_shared_token() {
        assert_eq!(
            expand("❤ help and ✓"),
            "I would be happy to help and that makes sense"
        );
        assert_eq!(expand("enact typology"), "enact typology");
        assert_eq!(expand("(❤)"), "(I would be happy to)");
        assert_eq!(expand("act"), "actually");
    }

    #[test]
    fn pony_mode_applies_pony_and_full_compression() {
        let result = compress("The human and the woman use their hands", Mode::Pony);
        assert_eq!(result, "pony and mare use their hooves");
        assert_eq!(compress("New\tYork", Mode::Pony), "Manehattan");
    }

    #[test]
    fn canterlot_mode_expands_deterministically() {
        assert_eq!(
            compress("Hello, you need help with a broken app", Mode::Canterlot),
            "This humble pony's splendid stallion, one discover it rather essential to provide assistance to with a in a state of disrepair application"
        );
    }

    #[test]
    fn tony_soprano_mode_applies_contractions_and_blunt_phrasing() {
        let input = "This problem is very bad because we are going to lose money. Keep `going to` and \"very bad\".";
        assert_eq!(
            compress(input, Mode::TonySoprano),
            "This situation is a real mess 'cause we're gonna lose money. Keep `going to` and \"very bad\"."
        );
    }

    #[test]
    fn ultra_mode_abbreviates_without_dropping_negation() {
        let result = compress(
            "The authentication configuration is not ready because the database is unavailable",
            Mode::Ultra,
        );
        assert_eq!(result, "auth conf not ready bc db unavailable");
        assert_eq!(compress("Authentication works", Mode::Ultra), "auth works");
    }

    #[test]
    fn protected_literals_are_unchanged() {
        let input = "in order to run `in order to`, open HTTPS://example.test/in-order-to and keep \"in order to\", 'in order to', and “in order to” at /etc/configuration/app for auth-middleware";
        let result = compress(input, Mode::Tokens);
        assert_eq!(
            result,
            "≫ run `in order to`, open HTTPS://example.test/in-order-to and keep \"in order to\", 'in order to', and “in order to” at /etc/configuration/app for auth-middleware"
        );
    }

    #[test]
    fn protected_identifiers_survive_ultra_mode() {
        let input = "configuration config.toml auth_middleware authentication-middleware /srv/configuration --configuration";
        assert_eq!(
            compress(input, Mode::Ultra),
            "conf config.toml auth_middleware authentication-middleware /srv/configuration --configuration"
        );
    }

    #[test]
    fn fenced_code_is_unchanged() {
        let input = "in order to run:\n```text\nin order to keep the code\n```\nin order to finish";
        let result = compress(input, Mode::Full);
        assert_eq!(
            result,
            "≫ run:\n```text\nin order to keep the code\n```\n≫ finish"
        );
    }

    #[test]
    fn longer_fence_is_not_closed_by_nested_shorter_fence() {
        let input =
            "````markdown\n```\nin order to keep the configuration\n```\n````\nin order to finish";
        let result = compress(input, Mode::Ultra);
        assert_eq!(
            result,
            "````markdown\n```\nin order to keep the configuration\n```\n````\n≫ finish"
        );
    }

    #[test]
    fn metrics_handle_empty_and_expanding_results() {
        assert_eq!(
            metrics("", ""),
            Metrics {
                original_bytes: 0,
                result_bytes: 0,
                original_characters: 0,
                result_characters: 0,
                original_words: 0,
                result_words: 0,
                estimated_original_tokens: 0,
                estimated_result_tokens: 0,
                byte_reduction_percent: 0.0,
                character_reduction_percent: 0.0,
                word_reduction_percent: 0.0,
                byte_based_token_estimate_reduction_percent: 0.0,
            }
        );
        assert!(metrics("hi", "hello there").byte_reduction_percent < 0.0);
    }

    #[test]
    fn execute_returns_structured_result() {
        let output = execute_inner(
            r#"{"action":"compress","text":"I would be happy to help","mode":"tokens"}"#,
        )
        .expect("valid request should execute");
        let value: serde_json::Value =
            serde_json::from_str(&output).expect("output should be JSON");
        assert_eq!(value["action"], "compress");
        assert_eq!(value["mode"], "tokens");
        assert_eq!(value["text"], "❤ help");
        assert_eq!(value["lossy"], true);
        assert_eq!(value["uses_destructive_rules"], false);
    }

    #[test]
    fn invalid_action_mode_and_large_input_are_rejected() {
        assert!(execute_inner(r#"{"action":"nope","text":"hello"}"#).is_err());
        assert!(execute_inner(r#"{"action":"compress","text":"hello","mode":"warp"}"#).is_err());
        assert!(validate_input(&"x".repeat(MAX_INPUT_CHARACTERS + 1)).is_err());
        assert!(validate_input(&"😀".repeat(MAX_INPUT_CHARACTERS)).is_ok());
        assert!(validate_input(&"😀".repeat(MAX_INPUT_CHARACTERS + 1)).is_err());
    }

    #[test]
    fn maximum_input_stays_under_default_output_cap_in_expanding_mode() {
        let input = "hi ".repeat(MAX_INPUT_CHARACTERS / 3);
        assert_eq!(input.chars().count(), MAX_INPUT_CHARACTERS);
        let params = serde_json::json!({
            "action": "compress",
            "text": input,
            "mode": "canterlot"
        });
        let output = execute_inner(&params.to_string()).expect("maximum input should execute");
        assert!(output.len() < 100_000, "output was {} bytes", output.len());
    }

    #[test]
    fn schema_is_valid_and_requires_flat_fields() {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).expect("schema should parse");
        assert_eq!(schema["required"], serde_json::json!(["action", "text"]));
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema["properties"]["mode"]["enum"]
            .as_array()
            .is_some_and(|modes| modes.contains(&serde_json::json!("tony-soprano"))));
        assert_eq!(
            schema["properties"]["text"]["maxLength"],
            MAX_INPUT_CHARACTERS
        );
    }
}
