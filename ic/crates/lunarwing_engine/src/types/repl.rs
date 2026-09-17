//! Validation at the model-output / executable-Python boundary (WI#161).
//!
//! Reject tainted drafts rather than guessing which fragment the model intended
//! to execute. Tags and fences inside Python strings/comments remain data.

use crate::types::error::{EngineError, ModelFailureKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplArgumentCorruption {
    #[error("invalid_json")]
    InvalidJson,
    #[error("missing_code")]
    MissingCode,
    #[error("think_tag")]
    ThinkTag,
    #[error("argument_tag")]
    ArgumentTag,
    #[error("nested_fence")]
    NestedFence,
    #[error("unclosed_fence")]
    UnclosedFence,
    #[error("inspection_limit")]
    InspectionLimit,
}

impl From<ReplArgumentCorruption> for EngineError {
    fn from(signature: ReplArgumentCorruption) -> Self {
        Self::Llm {
            kind: ModelFailureKind::ReplArgumentCorruption,
            reason: signature.to_string(),
        }
    }
}

pub fn is_repl_action(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("repl")
}

/// Structured REPL calls must carry non-empty Python, never an empty/void
/// payload. Do not inspect arbitrary tools' string arguments as executable code.
pub fn validate_repl_arguments(
    arguments: &serde_json::Value,
) -> Result<(), ReplArgumentCorruption> {
    let code = arguments
        .get("code")
        .and_then(serde_json::Value::as_str)
        .ok_or(ReplArgumentCorruption::MissingCode)?;
    validate_repl_code(code)
}

pub fn validate_repl_code(code: &str) -> Result<(), ReplArgumentCorruption> {
    if code.trim().is_empty() {
        return Err(ReplArgumentCorruption::MissingCode);
    }
    PythonLiterals::default().scan(code)
}

/// Extract complete, line-delimited CodeAct fences. A nested opener is not a
/// closer: accepting it would execute a truncated (possibly valid) prefix.
/// Keep the historical preference: repl, python, py, then bare fences.
pub fn extract_repl_code(text: &str) -> Result<Option<String>, ReplArgumentCorruption> {
    let mut groups: [Vec<String>; 4] = std::array::from_fn(|_| Vec::new());
    let mut lines = text.split_inclusive('\n');
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        // Skip other languages and longer Markdown fences as a whole, so a
        // Python example nested inside a documentation fence is not executed.
        let rank = ["repl", "python", "py", ""]
            .iter()
            .position(|marker| *marker == info);
        let Some(rank) = rank else {
            let width = trimmed.bytes().take_while(|b| *b == b'`').count();
            for line in lines.by_ref() {
                if line.trim() == "`".repeat(width) {
                    break;
                }
            }
            continue;
        };
        let code = read_code_fence(&mut lines)?;
        groups[rank].push(code);
    }
    Ok(groups
        .into_iter()
        .find(|blocks| !blocks.is_empty())
        .map(|blocks| blocks.join("\n\n")))
}

fn read_code_fence<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
) -> Result<String, ReplArgumentCorruption> {
    let mut code = String::new();
    let mut literals = PythonLiterals::default();
    for line in lines {
        if literals.quote.is_none() && line.trim() == "```" {
            if code.trim().is_empty() {
                return Err(ReplArgumentCorruption::MissingCode);
            }
            return Ok(code.trim().to_string());
        }
        literals.scan(line)?;
        code.push_str(line);
    }
    Err(ReplArgumentCorruption::UnclosedFence)
}

/// Small lexical shield, not a Python parser. Monty still owns syntax checking
/// before any effect can run. Prefixes (r/b/f) don't change quote boundaries.
#[derive(Default)]
struct PythonLiterals {
    quote: Option<(u8, usize)>,
}

impl PythonLiterals {
    fn scan(&mut self, code: &str) -> Result<(), ReplArgumentCorruption> {
        let bytes = code.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if let Some((quote, width)) = self.quote {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i..].starts_with(&[quote; 3][..width]) {
                    self.quote = None;
                    i += width;
                } else {
                    i += 1;
                }
                continue;
            }
            match bytes[i] {
                b'#' => {
                    i += bytes[i..]
                        .iter()
                        .position(|b| *b == b'\n')
                        .unwrap_or(bytes.len() - i);
                }
                quote @ (b'\'' | b'"') => {
                    let width = if bytes[i..].starts_with(&[quote; 3]) {
                        3
                    } else {
                        1
                    };
                    self.quote = Some((quote, width));
                    i += width;
                }
                b'`' if bytes[i..].starts_with(b"```") => {
                    return Err(ReplArgumentCorruption::NestedFence);
                }
                b'<' => {
                    // ASCII marker positions are UTF-8 boundaries even though
                    // ordinary non-ASCII code is traversed byte by byte.
                    if let Some(signature) = tag_signature(&code[i..]) {
                        return Err(signature);
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
        Ok(())
    }
}

fn tag_signature(text: &str) -> Option<ReplArgumentCorruption> {
    // Bound lookahead: a long Python expression containing many '<' bytes
    // must not turn marker inspection into a quadratic scan.
    let end = text.as_bytes().iter().take(64).position(|b| *b == b'>')?;
    let tag = text[1..end].trim().trim_start_matches('/');
    if ["think", "thinking", "analysis", "reasoning"]
        .iter()
        .any(|name| tag.eq_ignore_ascii_case(name))
    {
        Some(ReplArgumentCorruption::ThinkTag)
    } else if ["arg_value", "arg_key", "tool_call", "function", "parameter"]
        .iter()
        .any(|name| {
            tag.eq_ignore_ascii_case(name)
                || tag
                    .split_once('=')
                    .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name))
        })
    {
        Some(ReplArgumentCorruption::ArgumentTag)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_incident_signatures_without_salvaging_a_prefix() {
        for (code, expected) in [
            (
                "</think>result = await mission_list()\nprint(result)",
                ReplArgumentCorruption::ThinkTag,
            ),
            (
                "print(1)\n</arg_value>: Complete a required step",
                ReplArgumentCorruption::ArgumentTag,
            ),
            (
                "print(1)\n```repl\nprint(2)",
                ReplArgumentCorruption::NestedFence,
            ),
        ] {
            assert_eq!(validate_repl_code(code), Err(expected));
            assert_eq!(
                extract_repl_code(&format!("```repl\n{code}\n```")),
                Err(expected)
            );
        }
    }

    #[test]
    fn quoted_markers_and_comments_are_data() {
        let code = "# </think> ```repl\ns = r'''</think>\n```repl\n```\n</arg_value>'''\nprint(s, \"```\", '<think>')";
        assert_eq!(validate_repl_code(code), Ok(()));
        assert_eq!(
            extract_repl_code(&format!("```repl\n{code}\n```")),
            Ok(Some(code.into()))
        );
    }

    #[test]
    fn rejects_void_and_fenced_structured_arguments() {
        for arguments in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"code": " "}),
            serde_json::json!({"code": 42}),
        ] {
            assert_eq!(
                validate_repl_arguments(&arguments),
                Err(ReplArgumentCorruption::MissingCode)
            );
        }
        assert_eq!(
            validate_repl_arguments(&serde_json::json!({"code": "```repl\nprint(1)\n```"})),
            Err(ReplArgumentCorruption::NestedFence)
        );
    }

    #[test]
    fn unclosed_fence_never_becomes_executable() {
        assert_eq!(
            extract_repl_code("```repl\nprint(1)"),
            Err(ReplArgumentCorruption::UnclosedFence)
        );
        assert_eq!(extract_repl_code("```json\n{}\n```"), Ok(None));
        assert_eq!(
            extract_repl_code("````markdown\n```repl\nprint(1)\n```\n````"),
            Ok(None)
        );
    }

    #[test]
    fn preserves_language_priority_without_promoting_other_blocks() {
        assert_eq!(
            extract_repl_code("```python\nignore()\n```\n```repl\nrun()\n```"),
            Ok(Some("run()".into()))
        );
    }
}
