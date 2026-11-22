//! Registry-owned, user-isolated tool calibration. No persistence or process globals.
//!
//! The learner bounds its model count; canceled/failed/timed-out calls do not
//! train successful runtime. Only short statistical updates hold the mutex.
//! Diagnostics reads and the optional evaluation trace never touch a tool's
//! hot path with I/O.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rust_decimal::Decimal;
use serde::Serialize;

use crate::tools::Tool;

use super::diagnostics::{
    CategoryPolicy, CategoryPolicySummary, EvaluationSummary, EvaluationTrace, TraceRecord,
};
use super::jobs::{JobEstimateCounters, job_key};
use super::learner::{LearnerCounterSnapshot, LearningModel};
use super::{Actuals, Estimator, MAX_TOOL_NAME_BYTES, RuntimeEstimate};

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
    state: Mutex<EstimationState>,
    mode: ToolEstimationMode,
    categories: CategoryPolicy,
    trace: Option<Mutex<EvaluationTrace>>,
}

#[derive(Default)]
struct EstimationState {
    estimator: Estimator,
    requests: EstimateRequestSnapshot,
    jobs: JobEstimateCounters,
}

/// One lookup per tracked execution attempt, independent of hint display.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct EstimateRequestSnapshot {
    pub estimate_requests: u64,
    pub estimates_returned: u64,
    pub estimates_withheld: u64,
}

impl EstimateRequestSnapshot {
    fn record(&mut self, eligible: bool) {
        self.estimate_requests = self.estimate_requests.saturating_add(1);
        let outcome = if eligible {
            &mut self.estimates_returned
        } else {
            &mut self.estimates_withheld
        };
        *outcome = outcome.saturating_add(1);
    }
}

/// Registry-wide counts. Expose only as owner-only, unlabeled aggregates.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ToolEstimateCounters {
    pub estimates: EstimateRequestSnapshot,
    pub observations: LearnerCounterSnapshot,
    /// Bounded local-job estimates (WI#318 C1). Same owner-only aggregate.
    pub jobs: JobEstimateCounters,
}

/// User-scoped diagnostics. Never contains the private learner key, tool
/// parameters, results or another user's activity.
#[derive(Debug, Clone, Serialize)]
pub struct ToolEstimateDiagnostics {
    pub mode: &'static str,
    pub category_policy: CategoryPolicySummary,
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
    key: String,
    actuals: Actuals,
    /// Pending trace row; actual duration is only filled for an `Ok` call.
    trace: Option<TraceRecord>,
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
            state: Mutex::new(EstimationState::default()),
            mode,
            categories,
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

    /// Read-only lookup, used for approval hints. It does not count as an
    /// execution attempt and only returns a warmed estimate.
    pub fn estimate(
        &self,
        user_id: &str,
        tool: &dyn Tool,
        params: &serde_json::Value,
    ) -> Option<RuntimeEstimate> {
        let key = self.key(user_id, tool.name())?;
        let baseline_time = tool.estimated_duration(params);
        let baseline_cost = tool.estimated_cost(params);
        self.state
            .lock()
            .ok()?
            .estimator
            .estimate_operation(&key, baseline_time, baseline_cost)
    }

    pub(crate) fn begin(
        &self,
        user_id: &str,
        tool: &dyn Tool,
        params: &serde_json::Value,
    ) -> Option<ToolObservation> {
        let tool_name = tool.name();
        let key = self.key(user_id, tool_name)?;
        // Tool hooks run outside the lock and may use the normalized arguments;
        // arguments themselves are never stored in the learner.
        let baseline_time = tool.estimated_duration(params);
        let baseline_cost = tool.estimated_cost(params);
        let (estimate, trace_capture) = {
            let mut state = self.state.lock().ok()?;
            let estimate = state
                .estimator
                .estimate_operation(&key, baseline_time, baseline_cost);
            state.requests.record(estimate.is_some());
            // Capture the model's age and early-run anchor with the prediction,
            // never after the call or a concurrent completion has trained the
            // learner.
            let trace_capture = self.trace.as_ref().map(|_| {
                let model = state.estimator.learner().get_model(&key);
                let prior_duration_samples = model.map_or(0, |model| model.time_sample_count);
                let comparison_baseline = model.and_then(LearningModel::early_duration_baseline);
                (prior_duration_samples, comparison_baseline)
            });
            (estimate, trace_capture)
        };
        let prediction = estimate.map(|estimate| estimate.duration);
        Some(ToolObservation {
            key,
            actuals: Actuals {
                baseline_time,
                baseline_cost,
                predicted_time: prediction.or(baseline_time),
                actual_time: Duration::ZERO,
                actual_cost: None,
            },
            trace: trace_capture.map(
                |(prior_duration_samples, comparison_baseline)| TraceRecord {
                    tool: Arc::from(tool_name),
                    declared_baseline: baseline_time,
                    comparison_baseline,
                    prediction,
                    actual: Duration::ZERO,
                    prior_duration_samples,
                },
            ),
        })
    }

    pub(crate) fn complete(
        &self,
        observation: ToolObservation,
        elapsed: Duration,
        cost: Option<Decimal>,
    ) {
        if let Ok(mut state) = self.state.lock() {
            state.estimator.record_operation_actuals(
                &observation.key,
                Actuals {
                    actual_time: elapsed,
                    actual_cost: cost,
                    ..observation.actuals
                },
            );
        }
        // No grouping, sorting or logging while holding the trace lock.
        if let (Some(trace), Some(record)) = (&self.trace, observation.trace)
            && let Ok(mut trace) = trace.lock()
        {
            trace.push(TraceRecord {
                actual: elapsed,
                ..record
            });
        }
    }

    /// User-scoped snapshot of this registry's models. A caller can only see
    /// categories recorded under its own length-framed key prefix.
    pub fn diagnostics(&self, user_id: &str) -> ToolEstimateDiagnostics {
        let prefix = format!("tool:{}:{user_id}", user_id.len());
        let mut categories = Vec::new();
        if let Ok(state) = self.state.lock() {
            let learner = state.estimator.learner();
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
            categories,
        }
    }

    /// Counts for the entire registry. Call only from owner-only diagnostics.
    pub(crate) fn aggregate_counters(&self) -> ToolEstimateCounters {
        self.state.lock().map_or_else(
            |_| ToolEstimateCounters::default(),
            |state| ToolEstimateCounters {
                estimates: state.requests,
                observations: state.estimator.learner().counters().snapshot(),
                jobs: state.jobs,
            },
        )
    }

    /// Whether job timing views may display a historical duration. This is
    /// governed by the same opt-in display mode as tool hints; recording and
    /// the per-tool category policy stay independent of it.
    pub fn job_display_enabled(&self) -> bool {
        self.display_enabled()
    }

    /// Frozen historical duration for a local job about to be dispatched.
    /// Read-only: it neither trains the model nor claims to be a deadline.
    pub fn estimate_job(&self, user_id: &str) -> Option<RuntimeEstimate> {
        if self.mode == ToolEstimationMode::Disabled {
            return None;
        }
        let key = job_key(user_id)?;
        let mut state = self.state.lock().ok()?;
        let estimate = state.estimator.estimate_operation(&key, None, None);
        state.jobs.record_estimate(estimate.is_some());
        estimate
    }

    /// One terminal observation for a local job run (WI#318 C1). The caller
    /// passes the frozen prediction, if any, so forecast error reflects the
    /// estimate that was actually shown. Only a successful completion trains
    /// the duration model; failures and cancellations are counted separately
    /// because their elapsed time is not a comparable run.
    pub fn record_job_terminal(
        &self,
        user_id: &str,
        elapsed: Duration,
        completed: bool,
        predicted: Option<Duration>,
    ) {
        if self.mode == ToolEstimationMode::Disabled {
            return;
        }
        let Some(key) = job_key(user_id) else {
            return;
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.jobs.record_terminal(completed);
        if !completed {
            return;
        }
        state.estimator.record_operation_actuals(
            &key,
            Actuals {
                baseline_time: None,
                baseline_cost: None,
                predicted_time: predicted,
                actual_time: elapsed,
                actual_cost: None,
            },
        );
    }

    /// Operator-only aggregate of the held-out evaluation trace. Returns
    /// `None` when the trace is disabled; records carry no user identity.
    pub fn evaluation(&self) -> Option<EvaluationSummary> {
        // Clone the bounded records (tool names are shared), then release the
        // lock before grouping and sorting the snapshot.
        let snapshot = self.trace.as_ref()?.lock().ok()?.clone();
        Some(snapshot.summary())
    }

    fn key(&self, user_id: &str, tool_name: &str) -> Option<String> {
        if self.mode == ToolEstimationMode::Disabled
            || user_id.len() > 512
            || tool_name.len() > MAX_TOOL_NAME_BYTES
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
        let categories = match std::env::var(ENV_CATEGORIES) {
            Ok(value) => CategoryPolicy::from_setting(Some(&value)),
            Err(std::env::VarError::NotPresent) => CategoryPolicy::default(),
            Err(std::env::VarError::NotUnicode(_)) => CategoryPolicy::invalid("invalid UTF-8"),
        };
        let trace = std::env::var(ENV_TRACE).ok();
        Self::configured(
            ToolEstimationMode::from_setting(mode.as_deref()),
            categories,
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
    fn job_estimates_stay_withheld_until_warm_and_never_train_failed_runs() {
        let estimates = ToolEstimates::new(ToolEstimationMode::Observe);
        for _ in 0..4 {
            estimates.record_job_terminal("alice", Duration::from_secs(90), true, None);
        }
        assert!(estimates.estimate_job("alice").is_none());
        // A failed run may be counted, but its elapsed time is not a
        // comparable completed run and must not train the model.
        estimates.record_job_terminal("alice", Duration::from_secs(5), false, None);
        assert!(estimates.estimate_job("alice").is_none());
        estimates.record_job_terminal("alice", Duration::from_secs(90), true, None);

        let estimate = estimates.estimate_job("alice").unwrap(); // safety: test
        assert_eq!(estimate.duration, Duration::from_secs(90));
        assert_eq!(estimate.cost, None);
        assert_eq!(estimate.samples, 5);
        // A different user stays cold.
        assert!(estimates.estimate_job("bob").is_none());

        let counters = estimates.aggregate_counters().jobs;
        assert_eq!(counters.completed_runs, 5);
        assert_eq!(counters.non_completed_runs, 1);
        // Two cold lookups plus Bob's.
        assert_eq!(counters.estimates_withheld, 3);
        assert_eq!(counters.estimates_offered, 1);
    }

    #[test]
    fn disabled_mode_records_no_job_observations_and_shows_no_timing() {
        let estimates = ToolEstimates::new(ToolEstimationMode::Disabled);
        for _ in 0..6 {
            estimates.record_job_terminal("alice", Duration::from_secs(10), true, None);
        }
        assert!(estimates.estimate_job("alice").is_none());
        assert!(!estimates.job_display_enabled());
        assert_eq!(
            estimates.aggregate_counters().jobs,
            crate::estimation::JobEstimateCounters::default()
        );
        assert!(!ToolEstimates::new(ToolEstimationMode::Observe).job_display_enabled());
        assert!(ToolEstimates::new(ToolEstimationMode::Display).job_display_enabled());
    }

    #[test]
    fn category_policy_gates_display_but_keeps_recording() {        let params = serde_json::json!({});
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
        let alice = estimates.diagnostics("alice");
        assert_eq!(alice.categories.len(), 1);
        assert_eq!(alice.categories[0].tool, EchoTool.name());
        assert_eq!(alice.categories[0].samples, 5);
        let rendered = serde_json::to_string(&alice).unwrap();
        assert!(!rendered.contains(secret));
        assert!(!rendered.contains("alice"));
        assert!(!rendered.contains("tool:"));

        assert!(estimates.diagnostics("bob").categories.is_empty());
        assert!(estimates.diagnostics("x").categories.is_empty());
    }

    #[test]
    fn evaluation_freezes_model_age_and_prediction_before_execution() {
        let params = serde_json::json!({});
        let traced = ToolEstimates::configured(
            ToolEstimationMode::Observe,
            CategoryPolicy::default(),
            Some(EvaluationTrace::new(64)),
        );
        let early = traced.begin("alice", &EchoTool, &params).unwrap();
        let window = traced.evaluation().unwrap().cold_start_sample_limit;
        for _ in 0..window {
            let observation = traced.begin("alice", &EchoTool, &params).unwrap();
            traced.complete(observation, Duration::from_millis(4), None);
        }
        // This call finished after other calls warmed the model, but must
        // still be part of warmup. Bob starts his own warmup independently.
        traced.complete(early, Duration::from_millis(4), None);
        for user in ["alice", "bob"] {
            let observation = traced.begin(user, &EchoTool, &params).unwrap();
            traced.complete(observation, Duration::from_millis(4), None);
        }

        let summary = traced.evaluation().unwrap();
        let echo = &summary.categories[0];
        assert_eq!(echo.overall.samples, window + 3);
        assert_eq!(echo.overall.prediction_samples, window - 4);
        assert_eq!(echo.warmup.samples, 7);
        assert_eq!(echo.cold_start.samples, window + 2);
        assert_eq!(echo.cold_start.prediction_samples, window - 5);
    }

    #[test]
    fn evaluation_comparison_baseline_uses_only_prior_runs_and_is_frozen() {
        let params = serde_json::json!({});
        let traced = ToolEstimates::configured(
            ToolEstimationMode::Observe,
            CategoryPolicy::default(),
            Some(EvaluationTrace::new(64)),
        );
        // The first call has no earlier run to average.
        let first = traced.begin("alice", &EchoTool, &params).unwrap();
        assert!(first.trace.as_ref().unwrap().comparison_baseline.is_none());
        traced.complete(first, Duration::from_secs(4), None);

        // The second call sees the first run's 4 s only, never its own result.
        let second = traced.begin("alice", &EchoTool, &params).unwrap();
        assert_eq!(
            second.trace.as_ref().unwrap().comparison_baseline,
            Some(Duration::from_secs(4))
        );
        traced.complete(second, Duration::from_secs(10), None);

        // The anchor is reported for the withheld warmup period too.
        let summary = traced.evaluation().unwrap();
        let echo = &summary.categories[0];
        assert_eq!(echo.warmup.comparison_baseline.samples, 1);
        assert_eq!(
            echo.warmup.comparison_baseline.median_abs_error_ms,
            Some(6_000.0)
        );
        assert_eq!(echo.overall.comparison_baseline.samples, 0);

        // A third call averages the two completed runs for its own anchor.
        let third = traced.begin("alice", &EchoTool, &params).unwrap();
        assert_eq!(
            third.trace.as_ref().unwrap().comparison_baseline,
            Some(Duration::from_secs(7))
        );
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
