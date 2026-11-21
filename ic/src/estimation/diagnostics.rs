//! Read-only estimation diagnostics (WI#318 A2/A3/A4).
//!
//! Everything here is aggregate and parameter-free: categories are canonical
//! tool names, never arguments, prompts, results or user identities. Reads sort
//! a bounded in-memory buffer only when a request asks for a snapshot; the hot
//! path never performs I/O.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use serde::Serialize;

const MAX_POLICY_ENTRIES: usize = 256;
const MAX_CATEGORY_NAME_BYTES: usize = 128;
const DEFAULT_TRACE_CAPACITY: usize = 2_048;
const MIN_TRACE_CAPACITY: usize = 16;
const MAX_TRACE_CAPACITY: usize = 8_192;

/// Per-category display gating (A3).
///
/// Positive entries form an allowlist; `-name` / `!name` entries suppress a
/// category. Suppression always wins, and an empty allowlist means "every
/// category". Suppressed categories keep recording; they only lose their hint.
#[derive(Debug, Clone, Default)]
pub struct CategoryPolicy {
    allow: BTreeSet<String>,
    suppress: BTreeSet<String>,
}

impl CategoryPolicy {
    /// Parse a comma-separated `LUNARWING_TOOL_ESTIMATES_CATEGORIES` value.
    ///
    /// Unrecognised entries are ignored rather than treated as a wildcard, so
    /// a typo cannot silently widen display to categories that never passed the
    /// held-out gate.
    pub fn from_setting(setting: Option<&str>) -> Self {
        let mut policy = Self::default();
        let Some(setting) = setting else {
            return policy;
        };
        let mut entries = 0usize;
        for raw in setting.split(',') {
            let entry = raw.trim();
            if entry.is_empty() {
                continue;
            }
            let (target, name) = match entry.strip_prefix(['-', '!']) {
                Some(name) => (&mut policy.suppress, name.trim()),
                None => (&mut policy.allow, entry),
            };
            if name.is_empty() || name.len() > MAX_CATEGORY_NAME_BYTES {
                continue;
            }
            if entries >= MAX_POLICY_ENTRIES {
                break;
            }
            if target.insert(name.to_owned()) {
                entries += 1;
            }
        }
        policy
    }

    /// Whether `category` (a canonical tool name) may display a hint.
    pub fn allows(&self, category: &str) -> bool {
        !self.suppress.contains(category)
            && (self.allow.is_empty() || self.allow.contains(category))
    }

    /// Bounded, non-sensitive description for diagnostics responses.
    pub fn summary(&self) -> CategoryPolicySummary {
        CategoryPolicySummary {
            mode: if self.allow.is_empty() {
                if self.suppress.is_empty() {
                    "all"
                } else {
                    "all-except-suppressed"
                }
            } else {
                "allowlist"
            },
            allowed: self.allow.len(),
            suppressed: self.suppress.len(),
        }
    }
}

/// Shape of the active category policy, without the category names themselves.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CategoryPolicySummary {
    pub mode: &'static str,
    pub allowed: usize,
    pub suppressed: usize,
}

/// One completed call captured for held-out evaluation (A4).
#[derive(Debug)]
pub(crate) struct TraceRecord {
    pub tool: String,
    pub baseline: Option<Duration>,
    pub prediction: Option<Duration>,
    pub actual: Duration,
}

/// Opt-in bounded ring of parameter-free evaluation records.
#[derive(Debug)]
pub struct EvaluationTrace {
    capacity: usize,
    records: VecDeque<TraceRecord>,
    evicted: u64,
}

impl EvaluationTrace {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.clamp(MIN_TRACE_CAPACITY, MAX_TRACE_CAPACITY),
            records: VecDeque::new(),
            evicted: 0,
        }
    }

    /// `None` keeps the trace off; only an explicit opt-in enables it.
    pub fn from_setting(setting: Option<&str>) -> Option<Self> {
        let setting = setting?.trim();
        if setting.is_empty() || setting == "0" {
            return None;
        }
        if setting == "1" {
            return Some(Self::new(DEFAULT_TRACE_CAPACITY));
        }
        let capacity = setting.parse::<usize>().ok()?;
        (capacity > 0).then(|| Self::new(capacity))
    }

    pub(crate) fn push(&mut self, record: TraceRecord) {
        if self.records.len() >= self.capacity {
            self.records.pop_front();
            self.evicted = self.evicted.saturating_add(1);
            tracing::debug!(
                capacity = self.capacity,
                "estimation evaluation trace evicted the oldest record"
            );
        }
        self.records.push_back(record);
    }

    /// Aggregate view: per canonical tool name, median absolute error of the
    /// unadjusted baseline and of the frozen EMA prediction.
    pub fn summary(&self) -> EvaluationSummary {
        let mut categories: BTreeMap<&str, CategoryAccumulator> = BTreeMap::new();
        for record in &self.records {
            let accumulator = categories.entry(record.tool.as_str()).or_default();
            accumulator.samples = accumulator.samples.saturating_add(1);
            let actual_ms = record.actual.as_secs_f64() * 1_000.0;
            if let Some(baseline) = record.baseline.and_then(usable) {
                accumulator
                    .baseline_errors
                    .push((actual_ms - baseline).abs());
            }
            if let Some(prediction) = record.prediction.and_then(usable) {
                accumulator
                    .prediction_errors
                    .push((actual_ms - prediction).abs());
            }
        }
        let categories = categories
            .into_iter()
            .map(|(tool, mut accumulator)| {
                let prediction_samples = accumulator.prediction_errors.len() as u64;
                let baseline = median(&mut accumulator.baseline_errors);
                let prediction = median(&mut accumulator.prediction_errors);
                EvaluationCategory {
                    tool: tool.to_owned(),
                    samples: accumulator.samples,
                    prediction_samples,
                    coverage: if accumulator.samples == 0 {
                        0.0
                    } else {
                        prediction_samples as f64 / accumulator.samples as f64
                    },
                    median_abs_error_baseline_ms: baseline,
                    median_abs_error_prediction_ms: prediction,
                    improvement_percent: improvement(baseline, prediction),
                }
            })
            .collect();
        EvaluationSummary {
            capacity: self.capacity,
            recorded: self.records.len(),
            evicted: self.evicted,
            categories,
        }
    }
}

#[derive(Default)]
struct CategoryAccumulator {
    samples: u64,
    baseline_errors: Vec<f64>,
    prediction_errors: Vec<f64>,
}

/// Bounded evaluation summary. Contains no user or request identifiers.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationSummary {
    pub capacity: usize,
    pub recorded: usize,
    pub evicted: u64,
    pub categories: Vec<EvaluationCategory>,
}

/// Per-category held-out comparison of baseline versus frozen prediction.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationCategory {
    pub tool: String,
    pub samples: u64,
    pub prediction_samples: u64,
    pub coverage: f64,
    pub median_abs_error_baseline_ms: Option<f64>,
    pub median_abs_error_prediction_ms: Option<f64>,
    /// Positive means the EMA beat the unadjusted baseline.
    pub improvement_percent: Option<f64>,
}

fn usable(duration: Duration) -> Option<f64> {
    let ms = duration.as_secs_f64() * 1_000.0;
    (ms.is_finite() && ms >= 0.0).then_some(ms)
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    })
}

fn improvement(baseline: Option<f64>, prediction: Option<f64>) -> Option<f64> {
    let (baseline, prediction) = (baseline?, prediction?);
    (baseline > 0.0).then(|| (1.0 - prediction / baseline) * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_allowlist_and_suppression_are_independent() {
        let policy = CategoryPolicy::from_setting(Some("http, bash, -bash, !web_search"));
        assert!(policy.allows("http"));
        assert!(!policy.allows("bash"));
        assert!(!policy.allows("web_search"));
        assert!(!policy.allows("unlisted"));

        let suppression_only = CategoryPolicy::from_setting(Some("-web_search"));
        assert!(suppression_only.allows("http"));
        assert!(!suppression_only.allows("web_search"));

        assert!(CategoryPolicy::default().allows("anything"));
        assert_eq!(CategoryPolicy::default().summary().mode, "all");
        assert_eq!(suppression_only.summary().mode, "all-except-suppressed");
        assert_eq!(policy.summary().mode, "allowlist");
    }

    #[test]
    fn policy_ignores_empty_and_oversized_entries() {
        let long = "x".repeat(MAX_CATEGORY_NAME_BYTES + 1);
        let policy = CategoryPolicy::from_setting(Some(&format!(",, {long}, -")));
        assert!(policy.allows("http"));
        assert_eq!(policy.summary().allowed, 0);
        assert_eq!(policy.summary().suppressed, 0);
    }

    #[test]
    fn trace_is_opt_in_and_bounded() {
        assert!(EvaluationTrace::from_setting(None).is_none());
        assert!(EvaluationTrace::from_setting(Some("0")).is_none());
        assert!(EvaluationTrace::from_setting(Some("nonsense")).is_none());
        let mut trace = EvaluationTrace::new(MIN_TRACE_CAPACITY);
        for _ in 0..MIN_TRACE_CAPACITY + 3 {
            trace.push(TraceRecord {
                tool: "http".to_owned(),
                baseline: Some(Duration::from_secs(1)),
                prediction: Some(Duration::from_secs(1)),
                actual: Duration::from_secs(1),
            });
        }
        let summary = trace.summary();
        assert_eq!(summary.recorded, MIN_TRACE_CAPACITY);
        assert_eq!(summary.evicted, 3);
    }

    #[test]
    fn trace_summary_compares_baseline_with_frozen_prediction() {
        let mut trace = EvaluationTrace::new(MIN_TRACE_CAPACITY);
        for n in 1..=5 {
            trace.push(TraceRecord {
                tool: "http".to_owned(),
                baseline: Some(Duration::from_secs(10)),
                prediction: Some(Duration::from_secs(n)),
                actual: Duration::from_secs(n),
            });
        }
        trace.push(TraceRecord {
            tool: "bash".to_owned(),
            baseline: None,
            prediction: None,
            actual: Duration::from_secs(3),
        });
        let summary = trace.summary();
        let http = summary
            .categories
            .iter()
            .find(|category| category.tool == "http")
            .expect("http category");
        assert_eq!(http.samples, 5);
        assert_eq!(http.prediction_samples, 5);
        assert!((http.coverage - 1.0).abs() < f64::EPSILON);
        assert_eq!(http.median_abs_error_baseline_ms, Some(7_000.0));
        assert_eq!(http.median_abs_error_prediction_ms, Some(0.0));
        assert_eq!(http.improvement_percent, Some(100.0));
        let bash = summary
            .categories
            .iter()
            .find(|category| category.tool == "bash")
            .expect("bash category");
        assert_eq!(bash.prediction_samples, 0);
        assert_eq!(bash.median_abs_error_baseline_ms, None);
        assert_eq!(bash.improvement_percent, None);
    }
}
