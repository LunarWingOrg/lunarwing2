#![no_main]
use libfuzzer_sys::fuzz_target;
use lunarwing_safety::{Sanitizer, Severity, neutralize_role_token_openers};

/// True when every ASCII-case occurrence of `token` in `text` is immediately
/// preceded by a backslash — i.e. the sanitizer escaped all of them.
fn all_ascii_case_variants_escaped(text: &str, token: &str) -> bool {
    let bytes = text.as_bytes();
    let token = token.as_bytes();
    let mut i = 0;
    while i + token.len() <= bytes.len() {
        if bytes[i..i + token.len()].eq_ignore_ascii_case(token) {
            if i == 0 || bytes[i - 1] != b'\\' {
                return false;
            }
            i += token.len();
        } else {
            i += 1;
        }
    }
    true
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
        // WI#289: detection is deliberately stricter than neutralization, so
        // Critical detection does NOT imply modification — phrase patterns
        // (e.g. "ignore all previous") have no neutralized form. The pinned
        // matrix instead:
        let has_critical = result
            .warnings
            .iter()
            .any(|w| w.severity == Severity::Critical);
        if has_critical {
            // Critical ⇒ escape_content ran: null bytes are removed and every
            // ASCII-case variant of [INST]/[/INST] is escaped.
            assert!(!result.content.contains('\0')); // safety: fuzz harness — panicking is the oracle
            assert!(all_ascii_case_variants_escaped(&result.content, "[INST]")); // safety: fuzz harness — panicking is the oracle
            assert!(all_ascii_case_variants_escaped(&result.content, "[/INST]")); // safety: fuzz harness — panicking is the oracle
        } else {
            // No Critical ⇒ nothing ran but the unconditional <| opener break.
            assert_eq!(result.content, neutralize_role_token_openers(s)); // safety: fuzz harness — panicking is the oracle
        }
    }
});
