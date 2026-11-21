//! Read-only estimation diagnostics (WI#318 A2/A3/A4).
//!
//! Everything here is aggregate and parameter-free: categories are canonical
//! tool names, never arguments, prompts, results or user identities. Reads sort
//! a bounded in-memory snapshot outside the trace lock; trace recording never
//! performs I/O.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use super::MAX_TOOL_NAME_BYTES;
use super::learner::{DEFAULT_MIN_SAMPLES, valid_duration};

const MAX_POLICY_ENTRIES: usize = 256;
const DEFAULT_TRACE_CAPACITY: usize = 2_048;
const MIN_TRACE_CAPACITY: usize = 16;
const MAX_TRACE_CAPACITY: usize = 8_192;
/// Include the withheld period plus early forecasts after the five-sample warmup.
const COLD_START_SAMPLE_LIMIT: u64 = 20;

/// Per-category display gating (A3).
///
/// Positive entries form an allowlist; `-name` / `!name` entries suppress a
/// category. Suppression always wins, and an empty allowlist means "every
/// category". Suppressed categories keep recording; they only lose their hint.
#[derive(Debug, Clone, Default)]
pub struct CategoryPolicy {
    allow: BTreeSet<String>,
    suppress: BTreeSet<String>,
    invalid: bool,
}

impl CategoryPolicy {
    /// Parse a comma-separated `LUNARWING_TOOL_ESTIMATES_CATEGORIES` value.
    ///
    /// An invalid nonblank policy disables all hints rather than falling back
    /// to a broader policy (e.g. if an intended suppression rule is malformed).
    pub fn from_setting(setting: Option<&str>) -> Self {
        let mut policy = Self::default();
        let Some(setting) = setting.filter(|setting| !setting.trim().is_empty()) else {
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
            if name.is_empty() || name.len() > MAX_TOOL_NAME_BYTES {
                return Self::invalid("missing or oversized tool name");
            }
            entries += usize::from(target.insert(name.to_owned()));
            if entries > MAX_POLICY_ENTRIES {
                return Self::invalid("too many distinct rules");
            }
        }
        if entries == 0 {
            return Self::invalid("no tool names");
        }
        policy
    }

    pub(super) fn invalid(reason: &'static str) -> Self {
        tracing::warn!(
            reason,
            "invalid tool estimate category policy; hints disabled"
        );
        Self {
            invalid: true,
            ..Self::default()
        }
    }

    /// Whether `category` (a canonical tool name) may display a hint.
    pub fn allows(&self, category: &str) -> bool {
        !self.invalid
            && !self.suppress.contains(category)
            && (self.allow.is_empty() || self.allow.contains(category))
    }

    /// Bounded, non-sensitive description for diagnostics responses.
    pub fn summary(&self) -> CategoryPolicySummary {
        CategoryPolicySummary {
            mode: if self.invalid {
                "invalid"
            } else if self.allow.is_empty() {
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
#[derive(Debug, Clone)]
pub(crate) struct TraceRecord {
    pub tool: Arc<str>,
    pub baseline: Option<Duration>,
    pub prediction: Option<Duration>,
    pub actual: Duration,
    /// Valid duration samples already known to this user/tool model when the
    /// forecast was made. No identity or ordering across users is retained.
    pub prior_duration_samples: u64,
}

/// Opt-in bounded ring of parameter-free evaluation records.
#[derive(Debug, Clone)]
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
        }
        self.records.push_back(record);
    }

    /// Aggregate view: paired errors, warmup, and an early cold-start window
    /// per canonical tool name. Call on a snapshot, not under the trace lock.
    pub fn summary(&self) -> EvaluationSummary {
        let mut categories: BTreeMap<&str, CategoryAccumulator> = BTreeMap::new();
        for record in &self.records {
            categories.entry(&record.tool).or_default().push(record);
        }
        let categories = categories
            .into_iter()
            .map(|(tool, accumulator)| accumulator.finish(tool))
            .collect();
        EvaluationSummary {
            capacity: self.capacity,
            recorded: self.records.len(),
            evicted: self.evicted,
            cold_start_sample_limit: COLD_START_SAMPLE_LIMIT,
            categories,
        }
    }
}

#[derive(Default)]
struct CategoryAccumulator {
    overall: ErrorAccumulator,
    cold_start: ErrorAccumulator,
    warmup_samples: u64,
    warmup_baseline_errors: Vec<f64>,
}

impl CategoryAccumulator {
    fn push(&mut self, record: &TraceRecord) {
        let error_ms = |forecast| record.actual.abs_diff(forecast).as_secs_f64() * 1_000.0;
        let baseline = record
            .baseline
            .filter(|&duration| valid_duration(duration).is_some())
            .map(error_ms);
        let prediction = record.prediction.map(error_ms);
        self.overall.push(baseline, prediction);
        if record.prior_duration_samples < COLD_START_SAMPLE_LIMIT {
            self.cold_start.push(baseline, prediction);
        }
        if record.prior_duration_samples < DEFAULT_MIN_SAMPLES {
            self.warmup_samples += 1;
            self.warmup_baseline_errors.extend(baseline);
        }
    }

    fn finish(mut self, tool: &str) -> EvaluationCategory {
        EvaluationCategory {
            tool: tool.to_owned(),
            overall: self.overall.finish(),
            cold_start: self.cold_start.finish(),
            warmup: WarmupSummary {
                samples: self.warmup_samples,
                baseline_samples: self.warmup_baseline_errors.len() as u64,
                median_abs_error_baseline_ms: median(&mut self.warmup_baseline_errors),
            },
        }
    }
}

#[derive(Default)]
struct ErrorAccumulator {
    samples: u64,
    prediction_samples: u64,
    baseline_errors: Vec<f64>,
    prediction_errors: Vec<f64>,
}

impl ErrorAccumulator {
    fn push(&mut self, baseline: Option<f64>, prediction: Option<f64>) {
        self.samples += 1;
        self.prediction_samples += u64::from(prediction.is_some());
        if let (Some(baseline), Some(prediction)) = (baseline, prediction) {
            self.baseline_errors.push(baseline);
            self.prediction_errors.push(prediction);
        }
    }

    fn finish(mut self) -> EvaluationMetrics {
        let paired_samples = self.baseline_errors.len() as u64;
        let baseline = median(&mut self.baseline_errors);
        let prediction = median(&mut self.prediction_errors);
        EvaluationMetrics {
            samples: self.samples,
            prediction_samples: self.prediction_samples,
            paired_samples,
            coverage: self.prediction_samples as f64 / self.samples.max(1) as f64,
            median_abs_error_baseline_ms: baseline,
            median_abs_error_prediction_ms: prediction,
            improvement_percent: improvement(baseline, prediction),
        }
    }
}

/// Bounded evaluation summary. Contains no user or request identifiers.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationSummary {
    pub capacity: usize,
    pub recorded: usize,
    pub evicted: u64,
    /// Exclusive upper bound on prior valid duration samples for `cold_start`.
    pub cold_start_sample_limit: u64,
    pub categories: Vec<EvaluationCategory>,
}

/// Per-category held-out comparison of baseline versus frozen prediction.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationCategory {
    pub tool: String,
    #[serde(flatten)]
    pub overall: EvaluationMetrics,
    pub cold_start: EvaluationMetrics,
    pub warmup: WarmupSummary,
}

/// Prediction coverage on all calls in a cohort, errors on the paired subset.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationMetrics {
    pub samples: u64,
    pub prediction_samples: u64,
    pub paired_samples: u64,
    pub coverage: f64,
    pub median_abs_error_baseline_ms: Option<f64>,
    pub median_abs_error_prediction_ms: Option<f64>,
    /// Positive means the EMA beat the unadjusted baseline.
    pub improvement_percent: Option<f64>,
}

/// Baseline-only error while a model is withholding its learned estimate.
#[derive(Debug, Clone, Serialize)]
pub struct WarmupSummary {
    pub samples: u64,
    pub baseline_samples: u64,
    pub median_abs_error_baseline_ms: Option<f64>,
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
    fn malformed_policy_does_not_fall_back_to_allow_all() {
        let oversized = "x".repeat(MAX_TOOL_NAME_BYTES + 1);
        for setting in [", ,", "-", &oversized, &format!("http, -{oversized}")] {
            let policy = CategoryPolicy::from_setting(Some(setting));
            assert!(!policy.allows("http"));
            assert_eq!(policy.summary().mode, "invalid");
        }
        assert!(CategoryPolicy::from_setting(Some("  ")).allows("http"));
    }

    #[test]
    fn policy_accepts_runtime_name_limit_and_rejects_excess_rules() {
        let longest = "x".repeat(MAX_TOOL_NAME_BYTES);
        assert!(CategoryPolicy::from_setting(Some(&longest)).allows(&longest));
        assert!(!CategoryPolicy::from_setting(Some(&format!("-{longest}"))).allows(&longest));

        let rules = (0..MAX_POLICY_ENTRIES)
            .map(|i| format!("tool_{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let full = CategoryPolicy::from_setting(Some(&format!("{rules},tool_0")));
        assert_eq!(full.summary().allowed, MAX_POLICY_ENTRIES);
        assert!(full.allows("tool_0"));
        let overflow = CategoryPolicy::from_setting(Some(&format!("{rules},-tool_0")));
        assert_eq!(overflow.summary().mode, "invalid");
        assert!(!overflow.allows("tool_0"));
        assert!(!overflow.allows("tool_1"));
    }

    fn record(
        tool: &str,
        prior_duration_samples: u64,
        baseline: Option<u64>,
        prediction: Option<u64>,
        actual: u64,
    ) -> TraceRecord {
        TraceRecord {
            tool: Arc::from(tool),
            baseline: baseline.map(Duration::from_secs),
            prediction: prediction.map(Duration::from_secs),
            actual: Duration::from_secs(actual),
            prior_duration_samples,
        }
    }

    #[test]
    fn trace_is_opt_in_and_bounded() {
        assert!(EvaluationTrace::from_setting(None).is_none());
        assert!(EvaluationTrace::from_setting(Some("0")).is_none());
        assert!(EvaluationTrace::from_setting(Some("nonsense")).is_none());
        let mut trace = EvaluationTrace::new(MIN_TRACE_CAPACITY);
        for n in 0..MIN_TRACE_CAPACITY + 3 {
            let prediction = (n as u64 >= DEFAULT_MIN_SAMPLES).then_some(1);
            trace.push(record("http", n as u64, Some(1), prediction, 1));
        }
        let summary = trace.summary();
        assert_eq!(summary.recorded, MIN_TRACE_CAPACITY);
        assert_eq!(summary.evicted, 3);
        assert_eq!(
            summary.categories[0].warmup.samples,
            DEFAULT_MIN_SAMPLES - 3
        );
    }

    #[test]
    fn trace_summary_pairs_errors_and_reports_warmup_and_early_forecasts_separately() {
        let mut trace = EvaluationTrace::new(MIN_TRACE_CAPACITY);
        trace.push(record("http", 0, Some(1), None, 10)); // warmup, 9 s error
        trace.push(record("http", 1, Some(1), None, 21)); // warmup, 20 s error
        trace.push(record("http", DEFAULT_MIN_SAMPLES, Some(2), Some(3), 4));
        trace.push(record(
            "http",
            COLD_START_SAMPLE_LIMIT - 1,
            None,
            Some(100),
            7,
        ));
        trace.push(record(
            "http",
            COLD_START_SAMPLE_LIMIT,
            Some(5),
            Some(7),
            11,
        ));
        trace.push(record(
            "http",
            COLD_START_SAMPLE_LIMIT + 1,
            Some(0),
            Some(12),
            4,
        ));
        trace.push(record("bash", DEFAULT_MIN_SAMPLES, None, Some(3), 3));
        let summary = trace.summary();
        let http = &summary.categories[1];
        assert_eq!(http.tool, "http");
        assert_eq!(http.overall.samples, 6);
        assert_eq!(http.overall.prediction_samples, 4);
        assert_eq!(http.overall.paired_samples, 2);
        assert!((http.overall.coverage - 2.0 / 3.0).abs() < f64::EPSILON);
        assert_eq!(http.overall.median_abs_error_baseline_ms, Some(4_000.0));
        assert_eq!(http.overall.median_abs_error_prediction_ms, Some(2_500.0));
        assert_eq!(http.overall.improvement_percent, Some(37.5));

        assert_eq!(http.warmup.samples, 2);
        assert_eq!(http.warmup.baseline_samples, 2);
        assert_eq!(http.warmup.median_abs_error_baseline_ms, Some(14_500.0));
        assert_eq!(http.cold_start.samples, 4);
        assert_eq!(http.cold_start.prediction_samples, 2);
        assert_eq!(http.cold_start.paired_samples, 1);
        assert!((http.cold_start.coverage - 0.5).abs() < f64::EPSILON);
        assert_eq!(http.cold_start.improvement_percent, Some(50.0));

        let bash = &summary.categories[0].overall;
        assert_eq!(bash.prediction_samples, 1);
        assert_eq!(bash.paired_samples, 0);
        assert_eq!(bash.median_abs_error_baseline_ms, None);
        assert_eq!(bash.median_abs_error_prediction_ms, None);
        assert_eq!(bash.improvement_percent, None);
    }

    #[test]
    fn trace_summary_does_not_report_improvement_against_a_perfect_baseline() {
        let mut trace = EvaluationTrace::new(MIN_TRACE_CAPACITY);
        trace.push(record("http", DEFAULT_MIN_SAMPLES, Some(2), Some(2), 2));
        let metrics = &trace.summary().categories[0].overall;
        assert_eq!(metrics.paired_samples, 1);
        assert_eq!(metrics.median_abs_error_baseline_ms, Some(0.0));
        assert_eq!(metrics.improvement_percent, None);
    }
}
