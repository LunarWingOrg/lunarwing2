//! Statistical learning for estimation improvement.

use std::collections::HashMap;
use std::time::Duration;

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use super::Actuals;

const MAX_MODELS: usize = 1024;
const MAX_CATEGORY_BYTES: usize = 1024;
const MAX_DURATION_SECS: f64 = 86_400.0;
const MIN_FACTOR: f64 = 0.01;
const MAX_FACTOR: f64 = 100.0;

/// Learning model for estimation adjustments.
#[derive(Debug, Clone)]
pub struct LearningModel {
    /// Cost adjustment factor (multiplier).
    pub cost_factor: f64,
    /// Time adjustment factor (multiplier).
    pub time_factor: f64,
    /// Number of samples.
    pub sample_count: u64,
    /// Valid cost ratios; a missing charge is not a zero-cost observation.
    pub cost_sample_count: u64,
    /// Completed calls with a usable duration.
    pub time_sample_count: u64,
    /// Duration samples with a declared, positive baseline.
    pub time_ratio_sample_count: u64,
    /// Running error rate for cost.
    pub cost_error_rate: f64,
    /// Running error rate for time.
    pub time_error_rate: f64,
    /// Direct duration EMA, used when the tool has no declared baseline.
    pub duration_secs: f64,
    /// Ordering for bounded least-recently-observed eviction.
    last_observation: u64,
}

impl Default for LearningModel {
    fn default() -> Self {
        Self {
            cost_factor: 1.0,
            time_factor: 1.0,
            sample_count: 0,
            cost_sample_count: 0,
            time_sample_count: 0,
            time_ratio_sample_count: 0,
            cost_error_rate: 0.0,
            time_error_rate: 0.0,
            duration_secs: 0.0,
            last_observation: 0,
        }
    }
}

/// Learner that improves estimates over time.
pub struct EstimationLearner {
    /// Models per category.
    models: HashMap<String, LearningModel>,
    /// Exponential moving average alpha.
    alpha: f64,
    /// Minimum samples before adjusting.
    min_samples: u64,
    observation_sequence: u64,
}

impl EstimationLearner {
    /// Create a new estimation learner.
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
            alpha: 0.1, // EMA smoothing factor
            min_samples: 5,
            observation_sequence: 0,
        }
    }

    /// Record actual results and update the model.
    pub fn record(
        &mut self,
        category: &str,
        estimated_cost: Decimal,
        actual_cost: Decimal,
        estimated_time: Duration,
        actual_time: Duration,
    ) {
        let predicted_time = self.adjust(category, Decimal::ZERO, estimated_time).1;
        self.record_actuals(
            category,
            Actuals {
                baseline_cost: Some(estimated_cost),
                actual_cost: Some(actual_cost),
                baseline_time: Some(estimated_time),
                predicted_time: Some(predicted_time),
                actual_time,
            },
        );
    }

    /// Record one completed execution. Ratios use the unadjusted baseline,
    /// while forecast error uses the prediction frozen before execution.
    pub fn record_actuals(&mut self, category: &str, actuals: Actuals) {
        if category.len() > MAX_CATEGORY_BYTES {
            return;
        }
        let duration = valid_duration(actuals.actual_time);
        let cost_ratio =
            actuals
                .baseline_cost
                .zip(actuals.actual_cost)
                .and_then(|(base, actual)| {
                    if base <= Decimal::ZERO || actual < Decimal::ZERO {
                        return None;
                    }
                    actual
                        .checked_div(base)?
                        .to_f64()
                        .filter(|ratio| ratio.is_finite())
                });
        if duration.is_none() && cost_ratio.is_none() {
            return;
        }
        self.make_room(category);
        self.observation_sequence = self.observation_sequence.saturating_add(1);
        let model = self.models.entry(category.to_owned()).or_default();
        model.last_observation = self.observation_sequence;
        model.sample_count = model.sample_count.saturating_add(1);
        if let Some(ratio) = cost_ratio {
            let ratio = ratio.clamp(0.0, MAX_FACTOR);
            model.cost_factor = smooth(
                model.cost_factor,
                ratio,
                self.alpha,
                model.cost_sample_count,
            );
            model.cost_error_rate = smooth(
                model.cost_error_rate,
                (ratio - 1.0).abs().min(1.0),
                self.alpha,
                model.cost_sample_count,
            );
            model.cost_sample_count = model.cost_sample_count.saturating_add(1);
        }
        if let Some(seconds) = duration {
            record_duration(model, &actuals, seconds, self.alpha);
        }
    }

    fn make_room(&mut self, category: &str) {
        if self.models.len() < MAX_MODELS || self.models.contains_key(category) {
            return;
        }
        if let Some(oldest) = self
            .models
            .iter()
            .min_by_key(|(key, model)| (model.last_observation, *key))
            .map(|(key, _)| key.clone())
        {
            self.models.remove(&oldest);
        }
    }

    /// Adjust estimates based on learned factors.
    pub fn adjust(&self, category: &str, cost: Decimal, time: Duration) -> (Decimal, Duration) {
        (
            self.adjust_cost(category, cost).unwrap_or(cost),
            self.estimate_duration(category, Some(time)).unwrap_or(time),
        )
    }

    /// Only a measured cost ratio can calibrate a declared cost.
    pub fn adjust_cost(&self, category: &str, cost: Decimal) -> Option<Decimal> {
        let model = self.models.get(category)?;
        if model.cost_sample_count < self.min_samples || cost < Decimal::ZERO {
            return None;
        }
        cost.checked_mul(Decimal::try_from(model.cost_factor).ok()?)
    }

    /// Withhold cold-start durations; no arbitrary five-second tool default.
    pub fn estimate_duration(
        &self,
        category: &str,
        baseline: Option<Duration>,
    ) -> Option<Duration> {
        let model = self.models.get(category)?;
        if model.time_sample_count < self.min_samples {
            return None;
        }
        let seconds = match baseline.and_then(valid_duration) {
            Some(base) if model.time_ratio_sample_count >= self.min_samples => {
                base * model.time_factor
            }
            _ => model.duration_secs,
        };
        Duration::try_from_secs_f64(seconds.clamp(0.0, MAX_DURATION_SECS)).ok()
    }

    /// Get confidence for a category (based on sample count and error rate).
    pub fn confidence(&self, category: &str) -> f64 {
        self.models.get(category).map_or(0.0, |model| {
            let samples = model.time_sample_count as f64;
            (samples / (samples + 20.0)) * (1.0 - model.time_error_rate)
        })
    }

    /// Get the model for a category.
    pub fn get_model(&self, category: &str) -> Option<&LearningModel> {
        self.models.get(category)
    }

    /// Get all models.
    pub fn all_models(&self) -> &HashMap<String, LearningModel> {
        &self.models
    }

    /// Set the EMA alpha.
    pub fn set_alpha(&mut self, alpha: f64) {
        if alpha.is_finite() {
            self.alpha = alpha.clamp(0.01, 0.5);
        }
    }

    /// Set minimum samples.
    pub fn set_min_samples(&mut self, min: u64) {
        self.min_samples = min.max(1);
    }

    /// Clear all learned data.
    pub fn clear(&mut self) {
        self.models.clear();
        self.observation_sequence = 0;
    }
}

fn valid_duration(duration: Duration) -> Option<f64> {
    let seconds = duration.as_secs_f64();
    (seconds > 0.0 && seconds <= MAX_DURATION_SECS).then_some(seconds)
}

fn smooth(previous: f64, new: f64, alpha: f64, samples: u64) -> f64 {
    if samples == 0 {
        new
    } else {
        previous * (1.0 - alpha) + new * alpha
    }
}

fn record_duration(model: &mut LearningModel, actuals: &Actuals, seconds: f64, alpha: f64) {
    // Bound a single outlier's influence, without calling this a percentile.
    let bounded = if model.time_sample_count == 0 {
        seconds
    } else {
        seconds.clamp(model.duration_secs * 0.1, model.duration_secs * 10.0)
    };
    model.duration_secs = smooth(model.duration_secs, bounded, alpha, model.time_sample_count);
    if let Some(base) = actuals.baseline_time.and_then(valid_duration) {
        let ratio = (seconds / base).clamp(MIN_FACTOR, MAX_FACTOR);
        model.time_factor = smooth(
            model.time_factor,
            ratio,
            alpha,
            model.time_ratio_sample_count,
        );
        model.time_ratio_sample_count = model.time_ratio_sample_count.saturating_add(1);
    }
    // Missing predictions contribute uncertainty rather than perfect accuracy.
    let error = actuals
        .predicted_time
        .and_then(valid_duration)
        .map_or(1.0, |prediction| {
            (seconds - prediction).abs() / seconds.max(prediction)
        });
    model.time_error_rate = smooth(model.time_error_rate, error, alpha, model.time_sample_count);
    model.time_sample_count = model.time_sample_count.saturating_add(1);
}

impl Default for EstimationLearner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_learning_model_update() {
        let mut learner = EstimationLearner::new();
        learner.set_min_samples(2);

        // Record some results where actuals are 20% higher than estimates
        for _ in 0..5 {
            learner.record(
                "test",
                dec!(100.0),
                dec!(120.0),
                Duration::from_secs(60),
                Duration::from_secs(72),
            );
        }

        let model = learner.get_model("test").unwrap();
        assert!(model.cost_factor > 1.0);
        assert!(model.time_factor > 1.0);
    }

    #[test]
    fn test_adjustment() {
        let mut learner = EstimationLearner::new();
        learner.set_min_samples(2);

        // Train with consistent 50% underestimation
        for _ in 0..10 {
            learner.record(
                "test",
                dec!(100.0),
                dec!(150.0),
                Duration::from_secs(60),
                Duration::from_secs(90),
            );
        }

        let (adjusted_cost, adjusted_time) =
            learner.adjust("test", dec!(100.0), Duration::from_secs(60));

        // Should adjust upward
        assert!(adjusted_cost > dec!(100.0));
        assert!(adjusted_time > Duration::from_secs(60));
    }

    #[test]
    fn test_confidence() {
        let mut learner = EstimationLearner::new();

        // No data = low confidence
        assert!(learner.confidence("unknown") < 0.5);

        // Add data
        for _ in 0..20 {
            learner.record(
                "known",
                dec!(100.0),
                dec!(100.0), // Perfect estimates
                Duration::from_secs(60),
                Duration::from_secs(60),
            );
        }

        // More data with good accuracy = higher confidence
        assert!(learner.confidence("known") >= 0.5);
    }

    fn timing(seconds: u64) -> Actuals {
        Actuals {
            baseline_cost: None,
            actual_cost: None,
            baseline_time: None,
            predicted_time: None,
            actual_time: Duration::from_secs(seconds),
        }
    }

    #[test]
    fn missing_cost_and_zero_baselines_do_not_create_cost_confidence() {
        let mut learner = EstimationLearner::new();
        for _ in 0..20 {
            learner.record_actuals(
                "tool",
                Actuals {
                    baseline_cost: Some(dec!(0.01)),
                    baseline_time: Some(Duration::ZERO),
                    ..timing(2)
                },
            );
        }
        let model = learner.get_model("tool").unwrap();
        assert_eq!(model.cost_sample_count, 0);
        assert_eq!(model.time_ratio_sample_count, 0);
        assert_eq!(model.time_sample_count, 20);
        assert_eq!(learner.adjust_cost("tool", dec!(0.01)), None);
        assert_eq!(
            learner.estimate_duration("tool", None),
            Some(Duration::from_secs(2))
        );
        // Cost has its own warmup, even after time is mature.
        learner.record_actuals(
            "tool",
            Actuals {
                baseline_cost: Some(dec!(0.01)),
                actual_cost: Some(dec!(0.02)),
                ..timing(2)
            },
        );
        assert_eq!(learner.adjust_cost("tool", dec!(0.01)), None);
    }

    #[test]
    fn absolute_factors_use_raw_baselines_not_previous_predictions() {
        let mut estimator = super::super::Estimator::new();
        let base = Duration::from_secs(1);
        for _ in 0..50 {
            let predicted = estimator
                .estimate_operation("tool", Some(base), None)
                .map(|estimate| estimate.duration);
            estimator.record_operation_actuals(
                "tool",
                Actuals {
                    baseline_time: Some(base),
                    predicted_time: predicted,
                    ..timing(4)
                },
            );
        }
        let result = estimator
            .estimate_operation("tool", Some(base), None)
            .unwrap();
        assert!((result.duration.as_secs_f64() - 4.0).abs() < 0.001);
        assert!(result.confidence > 0.6);
        assert_eq!(result.cost, None);
    }

    #[test]
    fn explicit_zero_charge_is_known_and_not_missing() {
        let mut learner = EstimationLearner::new();
        for _ in 0..5 {
            learner.record_actuals(
                "free",
                Actuals {
                    baseline_cost: Some(dec!(1)),
                    actual_cost: Some(Decimal::ZERO),
                    ..timing(2)
                },
            );
            learner.record_actuals(
                "unknown",
                Actuals {
                    baseline_cost: Some(dec!(1)),
                    ..timing(2)
                },
            );
        }
        assert_eq!(learner.adjust_cost("free", dec!(1)), Some(Decimal::ZERO));
        assert_eq!(learner.adjust_cost("unknown", dec!(1)), None);
    }

    #[test]
    fn durations_are_withheld_until_five_calls_and_outliers_are_bounded() {
        let mut learner = EstimationLearner::new();
        for _ in 0..4 {
            learner.record_actuals("tool", timing(1));
        }
        assert_eq!(learner.estimate_duration("tool", None), None);
        learner.record_actuals("tool", timing(1));
        assert_eq!(
            learner.estimate_duration("tool", None),
            Some(Duration::from_secs(1))
        );
        learner.record_actuals("tool", timing(86_400));
        assert!(learner.estimate_duration("tool", None).unwrap() < Duration::from_secs(2));
        let count = learner.get_model("tool").unwrap().sample_count;
        learner.record_actuals("tool", timing(0));
        learner.record_actuals("tool", timing(u64::MAX));
        assert_eq!(learner.get_model("tool").unwrap().sample_count, count);
    }

    #[test]
    fn numeric_guards_and_fractional_cost_factors() {
        let mut learner = EstimationLearner::new();
        learner.set_alpha(f64::NAN);
        learner.set_alpha(f64::INFINITY);
        learner.set_min_samples(0);
        learner.record(
            "tool",
            dec!(1),
            dec!(1.13),
            Duration::from_secs(1),
            Duration::from_secs(2),
        );
        let cost = learner.adjust_cost("tool", dec!(1)).unwrap();
        assert!((cost - dec!(1.13)).abs() < dec!(0.000001));
        learner.record_actuals(
            "bad",
            Actuals {
                baseline_cost: Some(Decimal::ZERO),
                actual_cost: Some(dec!(1)),
                ..timing(0)
            },
        );
        learner.record_actuals(
            "bad",
            Actuals {
                baseline_cost: Some(dec!(1)),
                actual_cost: Some(dec!(-1)),
                ..timing(0)
            },
        );
        assert!(learner.get_model("bad").is_none());
        // Checked decimal arithmetic must not panic on an unrepresentable ratio.
        learner.record_actuals(
            "overflow",
            Actuals {
                baseline_cost: Some(dec!(0.0000000000000000000000000001)),
                actual_cost: Some(Decimal::MAX),
                ..timing(1)
            },
        );
        assert_eq!(learner.get_model("overflow").unwrap().cost_sample_count, 0);
        assert!(learner.confidence("tool").is_finite());
    }

    #[test]
    fn model_storage_is_bounded_and_least_recent_observation_is_evicted() {
        let mut learner = EstimationLearner::new();
        for n in 0..MAX_MODELS {
            learner.record_actuals(&format!("tool:{n}"), timing(1));
        }
        learner.record_actuals("tool:0", timing(1));
        learner.record_actuals("new", timing(1));
        assert_eq!(learner.all_models().len(), MAX_MODELS);
        assert!(learner.get_model("tool:0").is_some());
        assert!(learner.get_model("tool:1").is_none());
        learner.record_actuals(&"x".repeat(MAX_CATEGORY_BYTES + 1), timing(1));
        assert_eq!(learner.all_models().len(), MAX_MODELS);
    }
}
