//! Gate-wait analytics (WI#318 B4).
//!
//! Observes how long a gate waited between creation and resolution, labelled
//! only by bounded channel family and outcome. This is diagnostics for
//! operators; it is **not** a runtime estimate, a scheduling input, or a
//! promise about when a human will approve. Gate expiry remains a fail-closed
//! security timer and reads none of this data.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Fixed channel families, so the series set cannot grow with tenant input.
const MAX_SERIES: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GateWaitOutcome {
    Approved,
    Denied,
    Expired,
}

impl GateWaitOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Expired => "expired",
        }
    }
}

/// Canonical, bounded channel family for a gate's source channel.
///
/// Extension channel names are arbitrary strings, so they collapse to
/// `extension`; nothing derived from a channel name is exposed verbatim.
pub fn channel_family(channel: &str) -> &'static str {
    match channel {
        "web" => "web",
        "gateway" => "gateway",
        "xmpp" => "xmpp",
        "signal" => "signal",
        "repl" | "cli" => "repl",
        "http" => "http",
        "__bootstrap__" => "bootstrap",
        _ => "extension",
    }
}

/// Wait between gate creation and its terminal outcome.
#[derive(Debug, Clone, Copy, Default)]
struct SeriesStats {
    samples: u64,
    total_ms: u64,
    max_ms: u64,
    /// Exponential moving average of the wait, in milliseconds.
    ema_ms: f64,
}

impl SeriesStats {
    fn observe(&mut self, waited: Duration) {
        let millis = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
        self.samples = self.samples.saturating_add(1);
        self.total_ms = self.total_ms.saturating_add(millis);
        self.max_ms = self.max_ms.max(millis);
        self.ema_ms = if self.samples == 1 {
            millis as f64
        } else {
            self.ema_ms * 0.9 + millis as f64 * 0.1
        };
    }
}

/// Bounded, parameter-free accumulator shared by one [`PendingGateStore`].
///
/// [`PendingGateStore`]: super::store::PendingGateStore
#[derive(Debug, Default)]
pub struct GateWaitAnalytics {
    state: Mutex<WaitState>,
}

#[derive(Debug, Default)]
struct WaitState {
    series: BTreeMap<(&'static str, GateWaitOutcome), SeriesStats>,
    dropped: u64,
}

impl GateWaitAnalytics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one terminal outcome. Values are clamped to a day, and a
    /// negative or unrepresentable wait becomes zero rather than a sample that
    /// could skew the aggregate.
    pub fn record(&self, channel: &str, outcome: GateWaitOutcome, waited: Duration) {
        let waited = waited.min(Duration::from_secs(86_400));
        let family = channel_family(channel);
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if !state.series.contains_key(&(family, outcome)) && state.series.len() >= MAX_SERIES {
            state.dropped = state.dropped.saturating_add(1);
            return;
        }
        state
            .series
            .entry((family, outcome))
            .or_default()
            .observe(waited);
    }

    pub fn diagnostics(&self) -> GateWaitDiagnostics {
        let Ok(state) = self.state.lock() else {
            return GateWaitDiagnostics {
                samples: 0,
                dropped: 0,
                outcomes: Vec::new(),
                note: NOTE,
            };
        };
        let outcomes = state
            .series
            .iter()
            .map(|(&(channel_family, outcome), stats)| GateWaitSeries {
                channel_family: channel_family.to_owned(),
                outcome: outcome.as_str(),
                samples: stats.samples,
                mean_ms: stats.total_ms as f64 / stats.samples.max(1) as f64,
                max_ms: stats.max_ms,
                ema_ms: stats.ema_ms,
            })
            .collect();
        GateWaitDiagnostics {
            samples: state.series.values().map(|stats| stats.samples).sum(),
            dropped: state.dropped,
            outcomes,
            note: NOTE,
        }
    }
}

const NOTE: &str = "Observed gate wait only; not a promise about when a human will \
approve, and never used for expiry, scheduling or cost.";

/// Owner-only aggregate. Contains no parameters, user or request identifiers.
#[derive(Debug, Clone, Serialize)]
pub struct GateWaitDiagnostics {
    pub samples: u64,
    pub dropped: u64,
    pub outcomes: Vec<GateWaitSeries>,
    pub note: &'static str,
}

/// One `(channel family, outcome)` series.
#[derive(Debug, Clone, Serialize)]
pub struct GateWaitSeries {
    pub channel_family: String,
    pub outcome: &'static str,
    pub samples: u64,
    pub mean_ms: f64,
    pub max_ms: u64,
    pub ema_ms: f64,
}

/// Wait for an approved or denied gate, clamped at zero for clock skew.
pub fn observed_wait(created_at: DateTime<Utc>, resolved_at: DateTime<Utc>) -> Duration {
    resolved_at
        .signed_duration_since(created_at)
        .to_std()
        .unwrap_or(Duration::ZERO)
}

/// Wait for an expired gate: the fail-closed deadline, not the lazy cleanup
/// time. Expiry is noticed on the next ingress, so using "now" would report
/// the polling delay rather than the timer the user actually hit.
pub fn expired_wait(gate: &super::pending::PendingGate) -> Duration {
    observed_wait(gate.created_at, gate.expires_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_are_bounded_and_extension_names_collapse() {
        assert_eq!(channel_family("web"), "web");
        assert_eq!(channel_family("gateway"), "gateway");
        assert_eq!(channel_family("xmpp"), "xmpp");
        assert_eq!(channel_family("signal"), "signal");
        assert_eq!(channel_family("repl"), "repl");
        assert_eq!(channel_family("cli"), "repl");
        assert_eq!(channel_family("http"), "http");
        assert_eq!(channel_family("__bootstrap__"), "bootstrap");
        assert_eq!(channel_family("some-wasm-channel"), "extension");
        assert_eq!(channel_family(""), "extension");
    }

    #[test]
    fn outcome_labels_are_stable() {
        assert_eq!(GateWaitOutcome::Approved.as_str(), "approved");
        assert_eq!(GateWaitOutcome::Denied.as_str(), "denied");
        assert_eq!(GateWaitOutcome::Expired.as_str(), "expired");
    }

    #[test]
    fn records_per_family_and_outcome_with_bounded_series() {
        let analytics = GateWaitAnalytics::new();
        analytics.record("web", GateWaitOutcome::Approved, Duration::from_millis(100));
        analytics.record("web", GateWaitOutcome::Approved, Duration::from_millis(300));
        analytics.record("xmpp", GateWaitOutcome::Denied, Duration::from_millis(50));
        analytics.record(
            "bad-channel",
            GateWaitOutcome::Expired,
            Duration::from_secs(2),
        );

        let diagnostics = analytics.diagnostics();
        assert_eq!(diagnostics.samples, 4);
        assert_eq!(diagnostics.dropped, 0);
        let web = diagnostics
            .outcomes
            .iter()
            .find(|series| series.channel_family == "web" && series.outcome == "approved")
            .unwrap();
        assert_eq!(web.samples, 2);
        assert_eq!(web.mean_ms, 200.0);
        assert_eq!(web.max_ms, 300);
        let expired = diagnostics
            .outcomes
            .iter()
            .find(|series| series.outcome == "expired")
            .unwrap();
        assert_eq!(expired.channel_family, "extension");
        let rendered = serde_json::to_string(&diagnostics).unwrap();
        assert!(!rendered.contains("bad-channel"));
    }

    #[test]
    fn waits_are_clamped_and_series_set_stays_bounded() {
        let analytics = GateWaitAnalytics::new();
        analytics.record(
            "web",
            GateWaitOutcome::Approved,
            Duration::from_secs(86_400 * 30),
        );
        assert_eq!(analytics.diagnostics().outcomes[0].max_ms, 86_400_000);
        let now = Utc::now();
        assert_eq!(
            observed_wait(now, now - chrono::Duration::seconds(5)),
            Duration::ZERO
        );
        assert_eq!(
            observed_wait(now, now + chrono::Duration::milliseconds(1500)),
            Duration::from_millis(1500)
        );
    }
}
