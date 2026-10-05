#![no_main]
use lunarwing_safety::{Sanitizer, Severity, neutralize_role_token_openers};
use libfuzzer_sys::fuzz_target;

/// True when `token` occurs in `haystack` in any ASCII case variant without
/// a backslash directly before it (the escape form `\[INST]` contains the
/// literal as a substring, so a plain `contains` is wrong).
fn contains_unescaped_ascii_token(haystack: &str, token: &str) -> bool {
    let bytes = haystack.as_bytes();
    let token = token.as_bytes();
    let mut i = 0;
    while i + token.len() <= bytes.len() {
        if bytes[i..i + token.len()].eq_ignore_ascii_case(token) {
            if i == 0 || bytes[i - 1] != b'\\' {
                return true;
            }
            i += token.len();
        } else {
            i += 1;
        }
    }
    false
}

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let sanitizer = Sanitizer::new();

        // Exercise the main sanitization path
        let result = sanitizer.sanitize(s);
        // Verify invariant: warnings should have valid ranges
        for w in &result.warnings {
            assert!(w.location.end <= s.len()); // safety: fuzz harness — panicking is the oracle
        }
        // The `<|` opener break is unconditional, so no live opener survives.
        assert!(!result.content.contains("<|")); // safety: fuzz harness — panicking is the oracle

        // WI#289 documented matrix (detection stricter than neutralization):
        let has_critical = result.warnings.iter().any(|w| w.severity == Severity::Critical);
        if has_critical {
            // Critical content is escaped: no NUL survives removal, and no
            // ASCII-case variant of the instruction tokens survives unescaped.
            assert!(!result.content.contains('\0')); // safety: fuzz harness — panicking is the oracle
            assert!(!result.content.contains("|>")); // safety: fuzz harness — panicking is the oracle
            assert!(!contains_unescaped_ascii_token(&result.content, "[INST]")); // safety: fuzz harness — panicking is the oracle
            assert!(!contains_unescaped_ascii_token(&result.content, "[/INST]")); // safety: fuzz harness — panicking is the oracle
        } else {
            // No Critical pattern: only the `<|` opener break applies.
            assert_eq!(result.content, neutralize_role_token_openers(s)); // safety: fuzz harness — panicking is the oracle
        }
    }
});
