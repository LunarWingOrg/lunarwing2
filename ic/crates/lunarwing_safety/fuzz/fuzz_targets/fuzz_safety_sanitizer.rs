#![no_main]
use lunarwing_safety::Sanitizer;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let sanitizer = Sanitizer::new();

        // Exercise the main sanitization path
        let result = sanitizer.sanitize(s);
        // Verify invariant: warnings should have valid ranges
        for w in &result.warnings {
            assert!(w.location.end <= s.len());
        }
        // WI#289: detection is deliberately stricter than neutralization, so
        // Critical detection does NOT imply modification — phrase patterns
        // (e.g. "ignore all previous") have no neutralized form; only special
        // tokens, null bytes and line-start role labels are rewritten. What
        // must hold instead: unmodified output is byte-identical to the input.
        if !result.was_modified {
            assert_eq!(result.content, s);
        }
    }
});
