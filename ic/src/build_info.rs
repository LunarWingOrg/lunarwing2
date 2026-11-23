//! Build identity for the startup banner (WI#870).
//!
//! `CARGO_PKG_VERSION` alone cannot tell two builds of the same version
//! apart: three consecutive 2.2.2.0 test builds logged the identical
//! `Starting LunarWing v2.2.2` line, so a tester reading `daemon.log` had to
//! ask the operator which build was running. The commit and build time are
//! embedded by `build.rs` and printed alongside the version.

use chrono::{DateTime, Utc};

/// Package version from `Cargo.toml` (e.g. `2.2.2`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Source revision embedded at build time: a short commit, `-dirty` when the
/// checkout had uncommitted tracked changes, or `unknown` when the build could
/// not determine one (no `.git`, unreadable checkout, no override).
pub const COMMIT: &str = env!("LUNARWING_BUILD_COMMIT");

/// Build start time as a Unix timestamp (seconds), from `SOURCE_DATE_EPOCH`
/// when set and the wall clock otherwise.
pub const BUILD_EPOCH: u64 = parse_epoch(env!("LUNARWING_BUILD_EPOCH"));

/// Decimal `u64` parse usable in a const initializer; `0` for anything that
/// is not all digits.
const fn parse_epoch(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut value: u64 = 0;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < b'0' || byte > b'9' {
            return 0;
        }
        value = value * 10 + (byte - b'0') as u64;
        index += 1;
    }
    value
}

/// The one-line identity printed by the startup banner, e.g.
/// `v2.2.2 (commit f8cf3c08, built 2026-11-23T18:04:11Z)`.
///
/// Every field is always present so the line has a stable shape for
/// `grep`/log-diff comparisons; a build without a commit says `unknown`
/// rather than dropping the field.
pub fn banner_identity() -> String {
    format!("v{} (commit {}, built {})", VERSION, COMMIT, build_time())
}

/// Build time as RFC 3339 UTC, or `unknown` when the embedded timestamp is
/// unusable (a clock before the Unix epoch, or a zero placeholder).
fn build_time() -> String {
    if BUILD_EPOCH == 0 {
        return "unknown".to_string();
    }
    let seconds = i64::try_from(BUILD_EPOCH).unwrap_or(i64::MAX);
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|time| time.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_identity_carries_version_and_commit() {
        let line = banner_identity();
        assert!(line.starts_with(&format!("v{VERSION} (commit ")), "{line}");
        assert!(line.contains(", built "), "{line}");
        assert!(line.ends_with(')'), "{line}");
    }

    #[test]
    fn banner_identity_is_stable_across_calls() {
        assert_eq!(banner_identity(), banner_identity());
    }

    #[test]
    fn build_time_is_rfc3339_utc_or_unknown() {
        let rendered = build_time();
        assert!(
            rendered == "unknown" || rendered.ends_with('Z'),
            "unexpected build time format: {rendered}"
        );
    }

    #[test]
    fn parse_epoch_accepts_digits_only() {
        assert_eq!(parse_epoch("1795458136"), 1_795_458_136);
        assert_eq!(parse_epoch(""), 0);
        assert_eq!(parse_epoch("unknown"), 0);
        assert_eq!(parse_epoch("12a"), 0);
    }
}
