//! Write-time secret scrubbing for daemon log output (WI#287).
//!
//! The web log stream was the only scrubbed log sink: process stderr (the
//! journal on systemd, `daemon.stderr.log` on OpenRC/launchd) and any log
//! copy shown to the agent got raw text. [`LogScrubber`] applies one policy to
//! every sink: the default [`LeakDetector`] shapes, log-specific shapes
//! (credentials embedded in URLs, token query parameters), and the exact
//! values of secrets the process was configured with — the generic shapes
//! cannot recognize a tenant's own gateway token or database password.
//!
//! Records containing a block-class secret are replaced whole with
//! [`REDACTED_LOG_RECORD`] (the web log stream's existing behavior); redact-class
//! matches are masked in place so the rest of the record stays useful.
//!
//! Limits: only text routed through a scrubbed sink is covered (not raw
//! `println!`/`eprintln!` or panic output, not other processes' logs), and a
//! secret is only recognized by a known shape or a configured value.

use std::borrow::Cow;

use regex::Regex;

use crate::leak_detector::{LeakAction, LeakDetector, LeakPattern, LeakSeverity};

/// Replacement for a whole log record that carried a block-class secret.
pub const REDACTED_LOG_RECORD: &str = "[log message redacted: contained blocked secret]";

/// Shortest configured secret value matched literally. Shorter values are
/// too likely to be ordinary words to redact everywhere they appear.
pub const MIN_KNOWN_SECRET_LEN: usize = 12;

/// Scrubs secrets out of log records before they are written anywhere.
pub struct LogScrubber {
    detector: LeakDetector,
}

impl LogScrubber {
    /// Default leak patterns, log-specific patterns, and an exact-value
    /// pattern for each known secret of at least [`MIN_KNOWN_SECRET_LEN`]
    /// bytes (shorter or empty values are ignored).
    pub fn new<S: AsRef<str>>(known_secrets: &[S]) -> Self {
        let mut detector = LeakDetector::new();
        for pattern in log_patterns() {
            detector.add_pattern(pattern);
        }
        for secret in known_secrets {
            let secret = secret.as_ref().trim();
            if secret.len() < MIN_KNOWN_SECRET_LEN {
                continue;
            }
            if let Ok(regex) = Regex::new(&regex::escape(secret)) {
                detector.add_pattern(LeakPattern {
                    name: "configured_secret".to_string(),
                    regex,
                    severity: LeakSeverity::Critical,
                    action: LeakAction::Redact,
                });
            }
        }
        Self { detector }
    }

    /// Scrub one log record. Never emits tracing events, so it is safe to
    /// call from inside a tracing writer or layer.
    pub fn scrub<'a>(&self, record: &'a str) -> Cow<'a, str> {
        let result = self.detector.scan(record);
        if result.should_block {
            return Cow::Borrowed(REDACTED_LOG_RECORD);
        }
        match result.redacted_content {
            Some(redacted) => Cow::Owned(redacted),
            None => Cow::Borrowed(record),
        }
    }
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
            // ?token=… — gateway URLs carry the bearer token as a query param.
            name: "url_token_query".to_string(),
            regex: Regex::new(
                r"(?i)[?&](?:token|access_token|auth_token|api_key|apikey|key|secret)=[A-Za-z0-9._~%+/=-]{8,}",
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
        let scrubbed = scrubber
            .scrub("db connect failed: postgres://lunarwing:hunter2hunter2@db.internal:5432/lw");
        assert!(!scrubbed.contains("hunter2"), "{scrubbed}");
        assert!(scrubbed.contains("db.internal:5432"), "{scrubbed}");

        let scrubbed =
            scrubber.scrub("gateway ready at http://127.0.0.1:3000/?token=0123456789abcdef0123");
        assert!(!scrubbed.contains("0123456789abcdef0123"), "{scrubbed}");
        assert!(scrubbed.contains("gateway ready at"), "{scrubbed}");
    }

    #[test]
    fn configured_secret_values_are_masked_even_without_a_known_shape() {
        // A 64-hex gateway token matches no block/redact default shape.
        let token = "4f0c2a9d8e7b6a5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c";
        let scrubber = LogScrubber::new(&[token, "short", ""]);
        let record = format!("auth header was Bearer-less: {token} (user=alice)");
        let scrubbed = scrubber.scrub(&record);
        assert!(!scrubbed.contains(token), "{scrubbed}");
        assert!(scrubbed.contains("(user=alice)"), "{scrubbed}");
        // Values below the minimum length are ignored rather than matched.
        assert_eq!(scrubber.scrub("a short note"), "a short note");
    }
}
