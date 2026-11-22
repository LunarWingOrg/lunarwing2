//! Cost, time, and value estimation with continuous learning.
//!
//! Estimates are based on:
//! - Historical data from similar jobs
//! - Tool cost/time characteristics
//! - Statistical models that improve over time

mod cost;
mod diagnostics;
mod jobs;
mod learner;
mod llm;
mod runtime;
mod time;
mod value;

pub use cost::CostEstimator;
pub use diagnostics::{
    BaselineComparison, CategoryPolicy, CategoryPolicySummary, EvaluationCategory,
    EvaluationMetrics, EvaluationSummary, EvaluationTrace, WarmupBaseline, WarmupSummary,
};
pub use jobs::{JOB_ESTIMATE_KIND_KEY, JobEstimateCounters, JobTimingView, LocalJobKind, job_key};
pub use learner::{EstimationLearner, LearnerCounterSnapshot, LearnerCounters, LearningModel};
pub use llm::{
    LlmModelDiagnostics, LlmTimingCounterSnapshot, LlmTimingDiagnostics, LlmTimingMode, LlmTimings,
};
pub use runtime::{
    CategoryDiagnostics, EstimateRequestSnapshot, ToolEstimateCounters, ToolEstimateDiagnostics,
    ToolEstimates, ToolEstimationMode,
};
pub use time::TimeEstimator;
pub use value::ValueEstimator;

use rust_decimal::Decimal;
use std::time::Duration;

const MAX_TOOL_NAME_BYTES: usize = 256;

/// Valid completed samples a learned or historical timing needs before it may
/// be shown (WI#318 A6.2): below this, timing stays withheld.
pub const MIN_WARM_SAMPLES: u64 = learner::DEFAULT_MIN_SAMPLES;

/// A completed execution paired with its *unadjusted* baselines and the
/// duration prediction captured before execution. Unknown cost stays unknown.
#[derive(Debug, Clone, Copy)]
pub struct Actuals {
    pub baseline_cost: Option<Decimal>,
    pub actual_cost: Option<Decimal>,
    pub baseline_time: Option<Duration>,
    pub predicted_time: Option<Duration>,
    pub actual_time: Duration,
}

/// Advisory single-operation estimate. Confidence is a heuristic, not a
/// probability or a deadline. Cost has its own minimum valid sample count.
#[derive(Debug, Clone)]
pub struct RuntimeEstimate {
    pub duration: Duration,
    pub confidence: f64,
    pub samples: u64,
    pub cost: Option<Decimal>,
}

impl RuntimeEstimate {
    /// Safe to append to an existing redacted approval description. Contains
    /// only aggregate numbers, never tool arguments, names or user identity.
    pub fn approval_hint(&self) -> String {
        let confidence = if self.confidence < 0.5 {
            "low"
        } else {
            "moderate"
        };
        let runtime = if self.duration < Duration::from_millis(10) {
            "<0.01s".to_owned()
        } else {
            format!("~{:.2}s", self.duration.as_secs_f64())
        };
        format!(
            "Typical tool runtime: {runtime} (EMA, {} completed calls, {confidence} confidence). Excludes approval wait; not a deadline.",
            self.samples,
        )
    }
}

/// Combined estimator.
pub struct Estimator {
    learner: EstimationLearner,
}

impl Estimator {
    /// Create a new estimator.
    pub fn new() -> Self {
        Self {
            learner: EstimationLearner::new(),
        }
    }

    /// Estimate a single operation from real observations, without the legacy
    /// job facade's synthetic prices, tool defaults or value/earnings fields.
    pub fn estimate_operation(
        &self,
        category: &str,
        baseline_time: Option<Duration>,
        baseline_cost: Option<Decimal>,
    ) -> Option<RuntimeEstimate> {
        let duration = self.learner.estimate_duration(category, baseline_time)?;
        let model = self.learner.get_model(category)?;
        Some(RuntimeEstimate {
            duration,
            confidence: self.learner.confidence(category),
            samples: model.time_sample_count,
            cost: baseline_cost.and_then(|cost| self.learner.adjust_cost(category, cost)),
        })
    }

    /// Record actual results for learning. Call only for executed, completed
    /// operations, not gate denials, preflight failures or truncated timeouts.
    pub fn record_operation_actuals(&mut self, category: &str, actuals: Actuals) {
        self.learner.record_actuals(category, actuals);
    }

    /// Read-only learner access for the diagnostics surface.
    pub fn learner(&self) -> &EstimationLearner {
        &self.learner
    }
}

impl Default for Estimator {
    fn default() -> Self {
        Self::new()
    }
}
