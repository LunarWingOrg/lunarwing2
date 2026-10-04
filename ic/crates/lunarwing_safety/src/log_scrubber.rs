//! Write-time secret scrubbing for daemon log output (WI#287).
//!
//! Every daemon log sink goes through one [`LogScrubber`]: process stderr (the
//! journal on systemd; on OpenRC and launchd the service's error log —
//! `daemon.stderr.log` from `lunarwing service install`, `lunarwing.err` from
//! the committed OpenRC template and multi-tenant OpenRC), the web log
//! stream, and the read-only copy the shell sandbox sees at `/tenant-logs`.
//!
//! What is masked:
//!
//! - the default [`LeakDetector`] shapes, with the 64-hex heuristic redacting
//!   instead of warning: 64 hex characters is the gateway bearer token's own
//!   shape, so any standalone 64-hex value (SHA-256 digests included) shows
//!   as `[REDACTED]` in daemon logs;
//! - log-specific shapes: credentials in URLs (`scheme://user:password@`),
//!   secret-named query parameters (`?token=`, `&client_secret=`,
//!   `&password=`, `X-Amz-Signature=`, ...), and `Authorization:`/`Bearer`
//!   values including `+`, `/`, `=` and `%`, also inside quoted header-map
//!   dumps;
//! - the exact values of secrets the process knows, at least
//!   [`MIN_KNOWN_SECRET_LEN`] bytes: given to [`LogScrubber::new`] or learned
//!   later through [`LogScrubber::add_secret`] (the daemon registers the
//!   gateway token and the secrets-store overlay this way).
//!
//! Every occurrence of every known value is found, overlapping ones
//! included, and all ranges (known values and shapes, in the original
//! record's coordinates) are merged before one masking pass. Overlapping or
//! nested matches therefore never re-emit the tail of a secret. Records
//! containing a block-class secret are replaced whole with
//! [`REDACTED_LOG_RECORD`] (the web log stream's existing behavior);
//! redact-class matches are masked in place so the rest of the record stays
//! useful.
//!
//! Limits: only text routed through a scrubbed sink is covered (not raw
//! `println!`/`eprintln!` or panic output, not other processes' logs); a
//! secret is recognized only by a known shape or a known value; URL userinfo
//! whose password holds an unencoded `/` or `@` is not matched by shape.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use aho_corasick::{AhoCorasick, MatchKind};
use regex::Regex;

use crate::leak_detector::{
    LeakAction, LeakDetector, LeakPattern, LeakSeverity, apply_redactions, default_patterns,
};

/// Replacement for a whole log record that carried a block-class secret.
pub const REDACTED_LOG_RECORD: &str = "[log message redacted: contained blocked secret]";

/// Shortest known secret value matched literally. Shorter values are too
/// likely to be ordinary words to redact everywhere they appear.
pub const MIN_KNOWN_SECRET_LEN: usize = 12;

/// Scrubs secrets out of log records before they are written anywhere.
pub struct LogScrubber {
    detector: LeakDetector,
    known: RwLock<KnownSecrets>,
    /// Serializes [`LogScrubber::add_secrets`], which builds the new automaton
    /// without holding `known`: scrubbing never waits on a build, and a log
    /// record raised while building cannot deadlock on its own scrub.
    adding: Mutex<()>,
}

/// Known secret values and the automaton that finds them. Deliberately not
/// `Debug`: the values must never end up in a log line themselves.
#[derive(Default)]
struct KnownSecrets {
    values: Vec<String>,
    matcher: Option<Arc<AhoCorasick>>,
}

impl LogScrubber {
    /// Default leak shapes (64-hex redacting), the log-specific shapes, and
    /// the given known secret values (values shorter than
    /// [`MIN_KNOWN_SECRET_LEN`] after trimming, or empty, are ignored).
    pub fn new<S: AsRef<str>>(known_secrets: &[S]) -> Self {
        let scrubber = Self {
            detector: LeakDetector::with_patterns(log_detector_patterns()),
            known: RwLock::new(KnownSecrets::default()),
            adding: Mutex::new(()),
        };
        scrubber.add_secrets(known_secrets.iter().map(AsRef::as_ref));
        scrubber
    }

    /// Learn one more secret value after construction (WI#287 review S4): a
    /// token generated or loaded from settings, a value injected from the
    /// secrets store. Returns whether it was added (`false` for a duplicate
    /// or a value shorter than [`MIN_KNOWN_SECRET_LEN`]).
    pub fn add_secret(&self, secret: &str) -> bool {
        self.add_secrets(std::iter::once(secret)) > 0
    }

    /// Batch form of [`Self::add_secret`]; returns how many values were
    /// added. The matcher is rebuilt once per batch; if that fails, the
    /// previous matcher and values stay in place. Never emits tracing events
    /// (a log sink may be waiting on this scrubber).
    pub fn add_secrets<'s>(&self, secrets: impl IntoIterator<Item = &'s str>) -> usize {
        let _adding = self.adding.lock().unwrap_or_else(PoisonError::into_inner);
        let mut values = self
            .known
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values
            .clone();
        let before = values.len();
        for secret in secrets {
            let secret = secret.trim();
            if secret.len() >= MIN_KNOWN_SECRET_LEN && !values.iter().any(|v| v == secret) {
                values.push(secret.to_string());
            }
        }
        let added = values.len() - before;
        if added == 0 {
            return 0;
        }
        // Standard match semantics are required for overlapping search.
        let Ok(matcher) = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(&values)
        else {
            return 0;
        };
        *self.known.write().unwrap_or_else(PoisonError::into_inner) = KnownSecrets {
            values,
            matcher: Some(Arc::new(matcher)),
        };
        added
    }

    /// Scrub one log record. Never emits tracing events, so it is safe to
    /// call from inside a tracing writer or layer.
    pub fn scrub<'a>(&self, record: &'a str) -> Cow<'a, str> {
        let result = self.detector.scan(record);
        if result.should_block {
            return Cow::Borrowed(REDACTED_LOG_RECORD);
        }
        let mut ranges: Vec<Range<usize>> = result
            .matches
            .iter()
            .filter(|found| found.action == LeakAction::Redact)
            .map(|found| found.location.clone())
            .collect();
        if !self.known_value_ranges(record, &mut ranges) {
            // Fail closed: never write a record that could not be searched.
            return Cow::Borrowed(REDACTED_LOG_RECORD);
        }
        if ranges.is_empty() {
            return Cow::Borrowed(record);
        }
        Cow::Owned(apply_redactions(record, &ranges))
    }

    /// Append the location of every occurrence of every known value,
    /// overlapping ones included. `false` when the search could not run.
    fn known_value_ranges(&self, record: &str, ranges: &mut Vec<Range<usize>>) -> bool {
        // Clone the automaton out so no lock is held while matching.
        let matcher = self
            .known
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .matcher
            .clone();
        let Some(matcher) = matcher else {
            return true;
        };
        let Ok(found) = matcher.try_find_overlapping_iter(record) else {
            return false;
        };
        ranges.extend(found.map(|m| m.start()..m.end()));
        true
    }
}

/// The default shapes with the 64-hex heuristic promoted from warn to
/// redact, plus the log-only shapes.
fn log_detector_patterns() -> Vec<LeakPattern> {
    let mut patterns = default_patterns();
    for pattern in &mut patterns {
        // Warn-only for tool output, but 64 hex characters is the gateway
        // token's own shape, so log sinks redact it (WI#287 review S4).
        if pattern.name == "high_entropy_hex" {
            pattern.action = LeakAction::Redact;
        }
    }
    patterns.extend(log_patterns());
    patterns
}

/// Shapes specific to log text, kept out of the shared default set because
/// the detector also guards tool output and outbound HTTP.
fn log_patterns() -> Vec<LeakPattern> {
    vec![
        LeakPattern {
            // postgres://user:password@host — connection errors echo URLs.
            name: "url_userinfo_credentials".to_string(),
            regex: Regex::new(r"(?i)\b[a-z][a-z0-9+.-]*://[^\s/:@]+:[^\s/@]+@").unwrap(), // safety: hardcoded literal
            severity: LeakSeverity::High,
            action: LeakAction::Redact,
        },
        LeakPattern {
            // ?token=, &client_secret=, &password=, X-Amz-Signature=, ...: the
            // name may carry a prefix that ends in `_`, `.` or `-`.
            name: "url_secret_query".to_string(),
            regex: Regex::new(
                r#"(?i)[?&;](?:[a-z0-9_.-]*[_.-])?(?:token|secret|password|passwd|pwd|apikey|key|sig|signature|session|auth|credential)=[^\s&#"'<>]{4,}"#,
            )
            .unwrap(), // safety: hardcoded literal
            severity: LeakSeverity::High,
            action: LeakAction::Redact,
        },
        LeakPattern {
            // Bearer values, URL-encoded or base64 (`+`, `/`, `=`, `%`).
            name: "log_bearer_token".to_string(),
            regex: Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9+/=._~%-]{16,}").unwrap(), // safety: hardcoded literal
            severity: LeakSeverity::High,
            action: LeakAction::Redact,
        },
        LeakPattern {
            // Authorization: Basic <base64>, Proxy-Authorization, and quoted
            // header-map dumps ({"authorization": "Basic ..."}).
            name: "log_auth_header".to_string(),
            regex: Regex::new(
                r#"(?i)\b(?:proxy-)?authorization"?\s*[:=]\s*"?[a-z]+\s+[A-Za-z0-9+/=._~%-]{8,}"#,
            )
            .unwrap(), // safety: hardcoded literal
            severity: LeakSeverity::High,
            action: LeakAction::Redact,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Secret-like fixtures are built at runtime (`repeat`, `format!`): a
    // literal of ten or more mixed characters after a token/key/secret word
    // trips the CI secret scan (gitleaks generic-api-key, WI#287 review S1).

    #[test]
    fn clean_records_pass_through_unchanged() {
        let scrubber = LogScrubber::new::<&str>(&[]);
        let record = "INFO lunarwing::agent: request completed status=200";
        assert!(matches!(scrubber.scrub(record), Cow::Borrowed(r) if r == record));
    }

    #[test]
    fn block_class_secret_replaces_the_whole_record() {
        let scrubber = LogScrubber::new::<&str>(&[]);
        let record = "uploading with access key AKIAIOSFODNN7EXAMPLE to the bucket";
        assert_eq!(scrubber.scrub(record), REDACTED_LOG_RECORD);
    }

    #[test]
    fn url_credentials_and_token_queries_are_masked_in_place() {
        let scrubber = LogScrubber::new::<&str>(&[]);
        let pw = "hunter2".repeat(2);
        let record = format!("db connect failed: postgres://lunarwing:{pw}@db.internal:5432/lw");
        let scrubbed = scrubber.scrub(&record);
        assert!(!scrubbed.contains("hunter2"), "{scrubbed}");
        assert!(scrubbed.contains("db.internal:5432"), "{scrubbed}");

        let token = "ab12".repeat(5);
        let record = format!("gateway ready at http://127.0.0.1:3000/?token={token}");
        let scrubbed = scrubber.scrub(&record);
        assert!(!scrubbed.contains(&token), "{scrubbed}");
        assert!(scrubbed.contains("gateway ready at"), "{scrubbed}");
    }

    #[test]
    fn configured_secret_values_are_masked_even_without_a_known_shape() {
        // Not hex, so only the configured value (not the 64-hex shape) can
        // match it.
        let token = "Zq7-".repeat(8);
        let scrubber = LogScrubber::new(&[token.as_str(), "short", ""]);
        let record = format!("auth header was Bearer-less: {token} (user=alice)");
        let scrubbed = scrubber.scrub(&record);
        assert!(!scrubbed.contains(&token), "{scrubbed}");
        assert!(scrubbed.contains("(user=alice)"), "{scrubbed}");
        // Values below the minimum length are ignored rather than matched.
        assert_eq!(scrubber.scrub("a short note"), "a short note");
    }

    /// WI#287 review S4: 64 hex characters is the gateway token's own shape,
    /// so log sinks redact it even when the value was never registered.
    #[test]
    fn high_entropy_hex_tokens_are_redacted_in_logs() {
        let token = "4f0c".repeat(16);
        let scrubber = LogScrubber::new::<&str>(&[]);
        let record = format!("auth ok token={token} user=alice");
        let scrubbed = scrubber.scrub(&record);
        assert!(!scrubbed.contains(&token), "{scrubbed}");
        assert!(scrubbed.contains("user=alice"), "{scrubbed}");
    }

    /// WI#287 review S4: values learned after construction (the gateway
    /// token, secrets-store values) are masked from then on; duplicates and
    /// short values are not added.
    #[test]
    fn secrets_added_after_construction_are_redacted() {
        let tok = format!("gw-{}", "Q7x".repeat(6));
        let scrubber = LogScrubber::new::<&str>(&[]);
        let record = format!("auth ok with {tok} for alice");
        assert_eq!(scrubber.scrub(&record), record);
        assert!(scrubber.add_secret(&tok));
        let scrubbed = scrubber.scrub(&record);
        assert!(!scrubbed.contains(&tok), "{scrubbed}");
        assert!(scrubbed.contains("for alice"), "{scrubbed}");
        assert!(!scrubber.add_secret(&tok));
        assert!(!scrubber.add_secret(&format!("  {tok}  ")));
        assert!(!scrubber.add_secret("short"));
        let other = format!("gw-{}", "Lm3".repeat(6));
        assert_eq!(scrubber.add_secrets([other.as_str(), tok.as_str()]), 1);
        let record = format!("{tok} then {other}");
        assert_eq!(scrubber.scrub(&record), "[REDACTED] then [REDACTED]");
    }

    /// WI#287 review S4: secret-named query parameters beyond `token=`, Basic
    /// credentials and Bearer values holding `+`, `/` or `%`, and quoted
    /// header-map dumps lose their secret part.
    #[test]
    fn widened_query_and_auth_header_shapes_are_masked() {
        let scrubber = LogScrubber::new::<&str>(&[]);
        let secret = "Q7".repeat(8);
        let short = "ab1".repeat(2);
        let basic = format!("{}+/{}", "dXNl".repeat(2), "cjpw".repeat(3));
        let encoded = format!("{}%2F{secret}", "abcd".repeat(5));
        let cases = [
            (
                format!("oauth http://h/cb?client_secret={secret}&state=x"),
                secret.clone(),
                Some("&state=x"),
            ),
            (
                format!("login http://h/?user=a&password={secret}"),
                secret.clone(),
                Some("?user=a"),
            ),
            (format!("login http://h/?pwd={short}"), short.clone(), None),
            (
                format!("req Authorization: Basic {basic} done"),
                basic.clone(),
                Some("done"),
            ),
            (
                format!("headers {{\"authorization\": \"Basic {basic}\"}}"),
                basic.clone(),
                None,
            ),
            (format!("hdr Bearer {encoded}"), secret.clone(), None),
        ];
        for (record, hidden, kept) in cases {
            let scrubbed = scrubber.scrub(&record);
            assert!(!scrubbed.contains(&hidden), "{record} -> {scrubbed}");
            if let Some(kept) = kept {
                assert!(scrubbed.contains(kept), "{record} -> {scrubbed}");
            }
        }
    }

    /// Guard: token counters and other ordinary `*_tokens=` fields are not
    /// query parameters and stay untouched (no copy either).
    #[test]
    fn token_counts_and_clean_records_are_untouched() {
        let scrubber = LogScrubber::new::<&str>(&[]);
        let record = "usage prompt_tokens=1234 completion_tokens=99 max_tokens=4096";
        assert!(matches!(scrubber.scrub(record), Cow::Borrowed(r) if r == record));
    }

    /// Guard: a 64-hex value inside a `?token=` value is one secret, masked
    /// once, with nothing of its tail left behind.
    #[test]
    fn nested_hex_inside_token_query_leaves_no_tail() {
        let scrubber = LogScrubber::new::<&str>(&[]);
        let record = format!(
            "GET /api/chat/events?token={}%2Ftail-of-token&x=1",
            "4f0c".repeat(16)
        );
        let scrubbed = scrubber.scrub(&record);
        assert!(!scrubbed.contains("tail-of-token"), "{scrubbed}");
        assert!(!scrubbed.contains("4f0c"), "{scrubbed}");
        assert!(scrubbed.contains("&x=1"), "{scrubbed}");
    }

    /// Guard (WI#287 review S4/S5): two registered secrets that overlap in a
    /// record (a suffix of one is a prefix of the other) are both masked.
    #[test]
    fn overlapping_known_secrets_never_leak_a_tail() {
        let head = "Kq".repeat(6);
        let first = format!("{head}{}", "Xy".repeat(2));
        let second = format!("{}{}", "Xy".repeat(2), "Pz".repeat(6));
        let scrubber = LogScrubber::new(&[first.as_str(), second.as_str()]);
        let record = format!("saw {head}{}{} end", "Xy".repeat(2), "Pz".repeat(6));
        let scrubbed = scrubber.scrub(&record);
        assert!(!scrubbed.contains("PzPz"), "{scrubbed}");
        assert!(!scrubbed.contains("KqKq"), "{scrubbed}");
        assert!(scrubbed.ends_with(" end"), "{scrubbed}");
    }

    /// WI#287 review S5: a registered secret that extends a shorter one must
    /// not leave its suffix behind when the shorter one is registered last.
    #[test]
    fn prefix_sharing_secrets_never_leak_a_suffix() {
        let short = "ab".repeat(8);
        let long = format!("{short}{}", "-tail".repeat(3));
        let scrubber = LogScrubber::new(&[long.as_str(), short.as_str()]);
        let record = format!("saw {long} in header");
        let scrubbed = scrubber.scrub(&record);
        assert!(!scrubbed.contains("-tail"), "{scrubbed}");
        assert!(!scrubbed.contains(&short), "{scrubbed}");
        assert!(scrubbed.contains("in header"), "{scrubbed}");
    }
}
