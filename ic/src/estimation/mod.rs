//! Cost, time, and value estimation with continuous learning.
//!
//! Estimates are based on:
//! - Historical data from similar jobs
//! - Tool cost/time characteristics
//! - Statistical models that improve over time

mod cost;
mod learner;
mod runtime;
mod time;
mod value;

pub use cost::CostEstimator;
pub use learner::{EstimationLearner, LearningModel};
pub use runtime::{ToolEstimates, ToolEstimationMode};
pub use time::TimeEstimator;
pub use value::ValueEstimator;

use rust_decimal::Decimal;
use std::time::Duration;

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

/// Combined estimation for a job.
#[derive(Debug, Clone)]
pub struct JobEstimate {
    /// Estimated cost to complete the job.
    pub cost: Decimal,
    /// Estimated time to complete.
    pub duration: Duration,
    /// Estimated value/earnings.
    pub value: Decimal,
    /// Confidence in the estimate (0-1).
    pub confidence: f64,
    /// Breakdown by tool.
    pub tool_breakdown: Vec<ToolEstimate>,
}

/// Estimate for a single tool usage.
#[derive(Debug, Clone)]
pub struct ToolEstimate {
    pub tool_name: String,
    pub cost: Decimal,
    pub duration: Duration,
    pub confidence: f64,
}

/// Combined estimator.
pub struct Estimator {
    cost: CostEstimator,
    time: TimeEstimator,
    value: ValueEstimator,
    learner: EstimationLearner,
}

impl Estimator {
    /// Create a new estimator.
    pub fn new() -> Self {
        Self {
            cost: CostEstimator::new(),
            time: TimeEstimator::new(),
            value: ValueEstimator::new(),
            learner: EstimationLearner::new(),
        }
    }

    /// Estimate for a job.
    pub fn estimate_job(
        &self,
        description: &str,
        category: Option<&str>,
        tools: &[String],
    ) -> JobEstimate {
        let tool_estimates: Vec<ToolEstimate> = tools
            .iter()
            .map(|t| ToolEstimate {
                tool_name: t.clone(),
                cost: self.cost.estimate_tool(t),
                duration: self.time.estimate_tool(t),
                confidence: 0.7, // Default confidence
            })
            .collect();

        let total_cost: Decimal = tool_estimates.iter().map(|e| e.cost).sum();
        let total_duration: Duration = tool_estimates.iter().map(|e| e.duration).sum();

        // Apply learned adjustments
        let (adjusted_cost, adjusted_time) =
            self.learner
                .adjust(category.unwrap_or("general"), total_cost, total_duration);

        let value = self.value.estimate(description, adjusted_cost);
        let confidence = self.learner.confidence(category.unwrap_or("general"));

        JobEstimate {
            cost: adjusted_cost,
            duration: adjusted_time,
            value,
            confidence,
            tool_breakdown: tool_estimates,
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

    /// Legacy job calibration API. Estimates here must be the unadjusted
    /// baselines; use `record_operation_actuals` for optional cost and a frozen
    /// prediction captured before execution.
    pub fn record_actuals(
        &mut self,
        category: &str,
        estimated_cost: Decimal,
        actual_cost: Decimal,
        estimated_time: Duration,
        actual_time: Duration,
    ) {
        self.learner.record(
            category,
            estimated_cost,
            actual_cost,
            estimated_time,
            actual_time,
        );
    }

    /// Record actual results for learning. Call only for executed, completed
    /// operations, not gate denials, preflight failures or truncated timeouts.
    pub fn record_operation_actuals(&mut self, category: &str, actuals: Actuals) {
        self.learner.record_actuals(category, actuals);
    }

    /// Get the cost estimator.
    pub fn cost(&self) -> &CostEstimator {
        &self.cost
    }

    /// Get the time estimator.
    pub fn time(&self) -> &TimeEstimator {
        &self.time
    }

    /// Get the value estimator.
    pub fn value(&self) -> &ValueEstimator {
        &self.value
    }
}

impl Default for Estimator {
    fn default() -> Self {
        Self::new()
    }
}
