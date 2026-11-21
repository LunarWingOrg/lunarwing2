//! Registry-owned, user-isolated tool calibration. No I/O or process globals.
//!
//! The learner bounds its model count; canceled/failed/timed-out calls do not
//! train successful runtime. Only short statistical updates hold the mutex.
//! Diagnostics reads and the optional evaluation trace never touch a tool's
//! hot path with I/O.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rust_decimal::Decimal;
use serde::Serialize;

use crate::tools::Tool;

use super::diagnostics::{
    CategoryPolicy, CategoryPolicySummary, EvaluationSummary, EvaluationTrace, TraceRecord,
};
use super::learner::{LearnerCounterSnapshot, LearningModel};
use super::{Actuals, Estimator, RuntimeEstimate};

/// Recording is enabled by default; public hints require explicit opt-in
/// until representative workloads have passed the rollout accuracy gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEstimationMode {
    Disabled,
    Observe,
    Display,
}

impl ToolEstimationMode {
    fn from_setting(setting: Option<&str>) -> Self {
        match setting {
            Some("0") => Self::Disabled,
            Some("1") => Self::Display,
            _ => Self::Observe,
        }
    }

    /// Stable, non-sensitive label for diagnostics responses and logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Observe => "observe",
            Self::Display => "display",
        }
    }
}

/// Comma-separated display allowlist/suppression list (WI#318 A3).
const ENV_CATEGORIES: &str = "LUNARWING_TOOL_ESTIMATES_CATEGORIES";
/// Opt-in bounded held-out evaluation trace (WI#318 A4).
const ENV_TRACE: &str = "LUNARWING_TOOL_ESTIMATES_TRACE";

/// Process-lifetime cache owned by one tool registry. Restart starts cold.
pub struct ToolEstimates {
    estimator: Mutex<Estimator>,
    mode: ToolEstimationMode,
    categories: CategoryPolicy,
    requests: EstimateRequests,
    trace: Option<Mutex<EvaluationTrace>>,
}

/// Display-lookup counters: an estimate was asked for and either produced
/// (eligible) or withheld for cold start / insufficient samples.
#[derive(Debug, Default)]
struct EstimateRequests {
    requested: AtomicU64,
    returned: AtomicU64,
    withheld: AtomicU64,
}

impl EstimateRequests {
    fn snapshot(&self) -> EstimateRequestSnapshot {
        EstimateRequestSnapshot {
            estimate_requests: self.requested.load(Ordering::Relaxed),
            estimates_returned: self.returned.load(Ordering::Relaxed),
            estimates_withheld: self.withheld.load(Ordering::Relaxed),
        }
    }
}

/// Serialisable estimate-lookup counters.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct EstimateRequestSnapshot {
    pub estimate_requests: u64,
    pub estimates_returned: u64,
    pub estimates_withheld: u64,
}

/// User-scoped diagnostics. Never contains the private learner key, tool
/// parameters, results or another user's models.
#[derive(Debug, Clone, Serialize)]
pub struct ToolEstimateDiagnostics {
    pub mode: &'static str,
    pub category_policy: CategoryPolicySummary,
    pub estimates: EstimateRequestSnapshot,
    pub observations: LearnerCounterSnapshot,
    pub categories: Vec<CategoryDiagnostics>,
}

/// One canonical tool category as seen by the requesting user.
#[derive(Debug, Clone, Serialize)]
pub struct CategoryDiagnostics {
    pub tool: String,
    pub samples: u64,
    pub duration_samples: u64,
    pub duration_ratio_samples: u64,
    pub cost_samples: u64,
    pub duration_ema_ms: f64,
    pub time_error_ema: f64,
    pub confidence: f64,
    pub confidence_label: &'static str,
    /// Whether the active category policy would display this category's hint.
    pub hint_displayed: bool,
}

impl CategoryDiagnostics {
    fn from_model(
        tool: &str,
        model: &LearningModel,
        confidence: f64,
        min_samples: u64,
        hint_displayed: bool,
    ) -> Self {
        let confidence_label = if model.time_sample_count < min_samples {
            "none"
        } else if confidence < 0.5 {
            "low"
        } else {
            "moderate"
        };
        Self {
            tool: tool.to_owned(),
            samples: model.sample_count,
            duration_samples: model.time_sample_count,
            duration_ratio_samples: model.time_ratio_sample_count,
            cost_samples: model.cost_sample_count,
            duration_ema_ms: model.duration_secs * 1_000.0,
            time_error_ema: model.time_error_rate,
            confidence,
            confidence_label,
            hint_displayed,
        }
    }
}

/// Per-invocation data frozen before execution; contains no arguments/results.
pub(crate) struct ToolObservation {
    tool: String,
    key: String,
    /// EMA prediction frozen before execution; `None` when withheld (cold start).
    ema_prediction: Option<Duration>,
    actuals: Actuals,
}

impl ToolEstimates {
    pub fn new(mode: ToolEstimationMode) -> Self {
        Self::configured(mode, CategoryPolicy::default(), None)
    }

    /// Compose a registry-owned cache. Category gating and the evaluation
    /// trace are independent of the mode and of approval/cost policy.
    pub(crate) fn configured(
        mode: ToolEstimationMode,
        categories: CategoryPolicy,
        trace: Option<EvaluationTrace>,
    ) -> Self {
        Self {
            estimator: Mutex::new(Estimator::new()),
            mode,
            categories,
            requests: EstimateRequests::default(),
            trace: trace.map(Mutex::new),
        }
    }

    /// Whether hints are enabled at all (ignores per-category gating).
    pub fn display_enabled(&self) -> bool {
        self.mode == ToolEstimationMode::Display
    }

    /// Whether this canonical tool category may display a hint.
    pub fn display_enabled_for(&self, tool_name: &str) -> bool {
        self.display_enabled() && self.categories.allows(tool_name)
    }

    pub fn category_policy(&self) -> CategoryPolicySummary {
        self.categories.summary()
    }

    /// Only return an estimate after five completed, usable runtime samples.
    pub fn estimate(
        &self,
        user_id: &str,
        tool: &dyn Tool,
        params: &serde_json::Value,
    ) -> Option<RuntimeEstimate> {
        let key = self.key(user_id, tool.name())?;
        let baseline_time = tool.estimated_duration(params);
        let baseline_cost = tool.estimated_cost(params);
        self.requests.requested.fetch_add(1, Ordering::Relaxed);
        let estimate = self.estimator.lock().ok()?.estimate_operation(
            &key,
            baseline_time,
            baseline_cost,
        );
        if estimate.is_some() {
            self.requests.returned.fetch_add(1, Ordering::Relaxed);
        } else {
            self.requests.withheld.fetch_add(1, Ordering::Relaxed);
        }
        estimate
    }

    pub(crate) fn begin(
        &self,
        user_id: &str,
        tool: &dyn Tool,
        params: &serde_json::Value,
    ) -> Option<ToolObservation> {
        let key = self.key(user_id, tool.name())?;
        // Tool hooks run outside the lock and may use the normalized arguments;
        // arguments themselves are never stored in the learner.
        let baseline_time = tool.estimated_duration(params);
        let baseline_cost = tool.estimated_cost(params);
        let ema_prediction = self
            .estimator
            .lock()
            .ok()?
            .estimate_operation(&key, baseline_time, baseline_cost)
            .map(|estimate| estimate.duration);
        Some(ToolObservation {
            tool: tool.name().to_owned(),
            key,
            ema_prediction,
            actuals: Actuals {
                baseline_time,
                baseline_cost,
                predicted_time: ema_prediction.or(baseline_time),
                actual_time: Duration::ZERO,
                actual_cost: None,
            },
        })
    }

    pub(crate) fn complete(
        &self,
        observation: ToolObservation,
        elapsed: Duration,
        cost: Option<Decimal>,
    ) {
        if let Ok(mut estimator) = self.estimator.lock() {
            estimator.record_operation_actuals(
                &observation.key,
                Actuals {
                    actual_time: elapsed,
                    actual_cost: cost,
                    ..observation.actuals
                },
            );
        }
        self.push_evaluation(&observation, elapsed);
    }

    /// Bounded, parameter-free held-out record. No I/O on this path.
    fn push_evaluation(&self, observation: &ToolObservation, elapsed: Duration) {
        let Some(trace) = &self.trace else {
            return;
        };
        if let Ok(mut trace) = trace.lock() {
            trace.push(TraceRecord {
                tool: observation.tool.clone(),
                baseline: observation.actuals.baseline_time,
                prediction: observation.ema_prediction,
                actual: elapsed,
            });
        }
    }

    /// User-scoped snapshot of this registry's models. A caller can only see
    /// categories recorded under its own length-framed key prefix.
    pub fn diagnostics(&self, user_id: &str) -> ToolEstimateDiagnostics {
        let prefix = format!("tool:{}:{user_id}", user_id.len());
        let mut categories = Vec::new();
        let mut observations = LearnerCounterSnapshot::default();
        if let Ok(estimator) = self.estimator.lock() {
            let learner = estimator.learner();
            observations = learner.counters().snapshot();
            let min_samples = learner.min_samples();
            for (key, model) in learner.all_models() {
                let Some(tool) = key.strip_prefix(&prefix) else {
                    continue;
                };
                if tool.is_empty() {
                    continue;
                }
                categories.push(CategoryDiagnostics::from_model(
                    tool,
                    model,
                    learner.confidence(key),
                    min_samples,
                    self.display_enabled_for(tool),
                ));
            }
        }
        categories.sort_by(|a, b| a.tool.cmp(&b.tool));
        ToolEstimateDiagnostics {
            mode: self.mode.as_str(),
            category_policy: self.categories.summary(),
            estimates: self.requests.snapshot(),
            observations,
            categories,
        }
    }

    /// Operator-only aggregate of the held-out evaluation trace. Returns
    /// `None` when the trace is disabled; records carry no user identity.
    pub fn evaluation(&self) -> Option<EvaluationSummary> {
        let trace = self.trace.as_ref()?;
        trace.lock().ok().map(|trace| trace.summary())
    }

    fn key(&self, user_id: &str, tool_name: &str) -> Option<String> {
        if self.mode == ToolEstimationMode::Disabled || user_id.len() > 512 || tool_name.len() > 256
        {
            return None;
        }
        // Length framing avoids collisions even when identifiers contain ':'.
        // Private key only: never emit it to an API, metrics label or log.
        Some(format!("tool:{}:{user_id}{tool_name}", user_id.len()))
    }
}

impl Default for ToolEstimates {
    fn default() -> Self {
        // Read once at registry construction, independent of approval/cost policy.
        let mode = std::env::var("LUNARWING_TOOL_ESTIMATES").ok();
        let categories = std::env::var(ENV_CATEGORIES).ok();
        let trace = std::env::var(ENV_TRACE).ok();
        Self::configured(
            ToolEstimationMode::from_setting(mode.as_deref()),
            CategoryPolicy::from_setting(categories.as_deref()),
            EvaluationTrace::from_setting(trace.as_deref()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::EchoTool;

    #[test]
    fn rollout_modes_require_explicit_display_opt_in() {
        assert_eq!(
            ToolEstimationMode::from_setting(None),
            ToolEstimationMode::Observe
        );
        assert_eq!(
            ToolEstimationMode::from_setting(Some("observe")),
            ToolEstimationMode::Observe
        );
        assert_eq!(
            ToolEstimationMode::from_setting(Some("invalid")),
            ToolEstimationMode::Observe
        );
        assert_eq!(
            ToolEstimationMode::from_setting(Some("1")),
            ToolEstimationMode::Display
        );
        assert_eq!(
            ToolEstimationMode::from_setting(Some("0")),
            ToolEstimationMode::Disabled
        );
        assert_eq!(ToolEstimationMode::Observe.as_str(), "observe");
    }

    #[test]
    fn unknown_baseline_learns_elapsed_time_and_disabled_cache_stays_cold() {
        let params = serde_json::json!({});
        let enabled = ToolEstimates::new(ToolEstimationMode::Observe);
        let disabled = ToolEstimates::new(ToolEstimationMode::Disabled);
        for _ in 0..5 {
            let observation = enabled.begin("alice", &EchoTool, &params).unwrap();
            enabled.complete(observation, Duration::from_millis(5), None);
            assert!(disabled.begin("alice", &EchoTool, &params).is_none());
        }
        let estimate = enabled.estimate("alice", &EchoTool, &params).unwrap();
        assert_eq!(estimate.duration, Duration::from_millis(5));
        assert_eq!(estimate.cost, None);
        assert!(estimate.approval_hint().contains("<0.01s"));
        assert!(disabled.estimate("alice", &EchoTool, &params).is_none());
        assert!(enabled.estimate("bob", &EchoTool, &params).is_none());
        assert!(
            enabled
                .begin(&"x".repeat(513), &EchoTool, &params)
                .is_none()
        );
    }

    #[test]
    fn category_policy_gates_display_but_keeps_recording() {
        let params = serde_json::json!({});
        let estimates = ToolEstimates::configured(
            ToolEstimationMode::Display,
            CategoryPolicy::from_setting(Some("other-tool")),
            None,
        );
        for _ in 0..5 {
            let observation = estimates.begin("alice", &EchoTool, &params).unwrap();
            estimates.complete(observation, Duration::from_millis(5), None);
        }
        assert!(!estimates.display_enabled_for(EchoTool.name()));
        assert!(estimates.display_enabled());
        let diagnostics = estimates.diagnostics("alice");
        assert_eq!(diagnostics.categories.len(), 1);
        assert_eq!(diagnostics.categories[0].tool, EchoTool.name());
        assert_eq!(diagnostics.categories[0].duration_samples, 5);
        assert!(!diagnostics.categories[0].hint_displayed);
        assert_eq!(diagnostics.category_policy.mode, "allowlist");

        let suppressed = ToolEstimates::configured(
            ToolEstimationMode::Display,
            CategoryPolicy::from_setting(Some(&format!("-{}", EchoTool.name()))),
            None,
        );
        assert!(!suppressed.display_enabled_for(EchoTool.name()));
    }

    #[test]
    fn diagnostics_are_user_scoped_and_never_echo_keys_or_parameters() {
        let secret = "sk-live-very-secret-parameter";
        let params = serde_json::json!({ "api_key": secret });
        let estimates = ToolEstimates::new(ToolEstimationMode::Observe);
        for _ in 0..5 {
            let observation = estimates.begin("alice", &EchoTool, &params).unwrap();
            estimates.complete(observation, Duration::from_millis(7), None);
        }
        estimates.estimate("alice", &EchoTool, &params);

        let alice = estimates.diagnostics("alice");
        assert_eq!(alice.categories.len(), 1);
        assert_eq!(alice.categories[0].tool, EchoTool.name());
        assert_eq!(alice.estimates.estimate_requests, 1);
        assert_eq!(alice.estimates.estimates_returned, 1);
        assert_eq!(alice.observations.observations_recorded, 5);
        let rendered = serde_json::to_string(&alice).unwrap();
        assert!(!rendered.contains(secret));
        assert!(!rendered.contains("alice"));
        assert!(!rendered.contains("tool:"));

        assert!(estimates.diagnostics("bob").categories.is_empty());
        assert!(estimates.diagnostics("x").categories.is_empty());
    }

    #[test]
    fn cold_start_lookups_are_counted_as_withheld() {
        let params = serde_json::json!({});
        let estimates = ToolEstimates::new(ToolEstimationMode::Observe);
        assert!(estimates.estimate("alice", &EchoTool, &params).is_none());
        let diagnostics = estimates.diagnostics("alice");
        assert_eq!(diagnostics.estimates.estimate_requests, 1);
        assert_eq!(diagnostics.estimates.estimates_returned, 0);
        assert_eq!(diagnostics.estimates.estimates_withheld, 1);
    }

    #[test]
    fn evaluation_trace_is_opt_in_and_parameter_free() {
        let params = serde_json::json!({ "api_key": "sk-trace-secret" });
        let disabled = ToolEstimates::new(ToolEstimationMode::Observe);
        for _ in 0..5 {
            let observation = disabled.begin("alice", &EchoTool, &params).unwrap();
            disabled.complete(observation, Duration::from_millis(4), None);
        }
        assert!(disabled.evaluation().is_none());

        let traced = ToolEstimates::configured(
            ToolEstimationMode::Observe,
            CategoryPolicy::default(),
            EvaluationTrace::from_setting(Some("1")),
        );
        for _ in 0..5 {
            let observation = traced.begin("alice", &EchoTool, &params).unwrap();
            traced.complete(observation, Duration::from_millis(4), None);
        }
        let summary = traced.evaluation().expect("trace enabled");
        assert_eq!(summary.recorded, 5);
        assert_eq!(summary.categories.len(), 1);
        let rendered = serde_json::to_string(&summary).unwrap();
        assert!(!rendered.contains("sk-trace-secret"));
        assert!(!rendered.contains("alice"));
    }

    #[test]
    #[ignore = "manual hot-path overhead measurement; does not time tool execution"]
    fn estimation_update_overhead() {
        for threads in [1, 12] {
            for mode in [ToolEstimationMode::Disabled, ToolEstimationMode::Observe] {
                let estimator = ToolEstimates::new(mode);
                let barrier = std::sync::Barrier::new(threads);
                let mut timings = std::thread::scope(|scope| {
                    let handles: Vec<_> = (0..threads)
                        .map(|_| {
                            scope.spawn(|| {
                                let params = serde_json::json!({});
                                barrier.wait();
                                (0..10_000)
                                    .map(|_| {
                                        let start = std::time::Instant::now();
                                        if let Some(observation) =
                                            estimator.begin("alice", &EchoTool, &params)
                                        {
                                            estimator.complete(
                                                observation,
                                                Duration::from_millis(20),
                                                None,
                                            );
                                        }
                                        start.elapsed().as_nanos()
                                    })
                                    .collect::<Vec<_>>()
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .flat_map(|h| h.join().unwrap())
                        .collect::<Vec<_>>()
                });
                timings.sort_unstable();
                eprintln!(
                    "EMA begin+complete: mode={mode:?} threads={threads} median={}ns p95={}ns (debug=0 test profile, {} samples)",
                    timings[timings.len() / 2],
                    timings[timings.len() * 95 / 100],
                    timings.len()
                );
            }
        }
    }
}
