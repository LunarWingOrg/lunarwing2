//! Daemon-wide LLM logical-call duration calibration (WI#318 Phase B1/B2).
//!
//! `LlmTimings` is owned by the provider chain built once per daemon. Keys are
//! bounded provider/model pairs, carry no user identity, prompt, result or
//! parameter, and are bounded by the learner's model cap.
//! Only durations are learned: a real per-call charge is still recorded by the
//! existing cost guard, so a v2 `cost_usd: 0.0` never becomes a free sample.
//!
//! Nothing here is persisted. A restart starts cold, exactly like the tool
//! cache, and an estimator failure can never change a call's result.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;

use crate::llm::{
    LlmTimingObservation, LlmTimingObserver, LlmTimingOutcome, LlmTimingTarget, valid_timing_label,
};

use super::learner::LearningModel;
use super::{Actuals, Estimator, RuntimeEstimate};

/// Read once when the provider chain is built; `0` disables recording.
const ENV_MODE: &str = "LUNARWING_LLM_TIMINGS";
/// Recording is on by default; it only feeds diagnostics in this slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmTimingMode {
    Disabled,
    Observe,
}

impl LlmTimingMode {
    fn from_setting(setting: Option<&str>) -> Self {
        match setting {
            Some("0") => Self::Disabled,
            _ => Self::Observe,
        }
    }

    /// Stable, non-sensitive label for diagnostics responses and logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Observe => "observe",
        }
    }
}

/// Duration models plus attempt/retry/first-chunk counts for one daemon.
pub struct LlmTimings {
    state: Mutex<LlmTimingState>,
    counters: LlmTimingCounters,
    mode: LlmTimingMode,
}

#[derive(Default)]
struct LlmTimingState {
    estimator: Estimator,
}

/// Non-identifying counters, read without taking the estimator lock.
#[derive(Debug, Default)]
struct LlmTimingCounters {
    calls_started: AtomicU64,
    provider_attempts: AtomicU64,
    completions: AtomicU64,
    failures: AtomicU64,
    cancellations: AtomicU64,
    retried_calls: AtomicU64,
    retry_attempts: AtomicU64,
    provider_switches: AtomicU64,
    first_chunk_samples: AtomicU64,
    first_chunk_sum_ms: AtomicU64,
}

impl LlmTimingCounters {
    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn add(counter: &AtomicU64, value: u64) {
        counter.fetch_add(value, Ordering::Relaxed);
    }

    fn snapshot(&self) -> LlmTimingCounterSnapshot {
        LlmTimingCounterSnapshot {
            calls_started: self.calls_started.load(Ordering::Relaxed),
            provider_attempts: self.provider_attempts.load(Ordering::Relaxed),
            completions: self.completions.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            cancellations: self.cancellations.load(Ordering::Relaxed),
            retried_calls: self.retried_calls.load(Ordering::Relaxed),
            retry_attempts: self.retry_attempts.load(Ordering::Relaxed),
            provider_switches: self.provider_switches.load(Ordering::Relaxed),
            first_chunk_samples: self.first_chunk_samples.load(Ordering::Relaxed),
            first_chunk_sum_ms: self.first_chunk_sum_ms.load(Ordering::Relaxed),
        }
    }
}

/// Serialisable counters. Contains no model names or identities.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct LlmTimingCounterSnapshot {
    /// Logical calls entering the timing boundary (one per caller attempt).
    pub calls_started: u64,
    /// Dispatches to a concrete provider, including retry and fallback legs.
    pub provider_attempts: u64,
    /// Terminal successes, including those excluded from model training.
    pub completions: u64,
    /// Calls that returned an error; they train nothing.
    pub failures: u64,
    /// Dropped call futures and dropped or truncated streams.
    pub cancellations: u64,
    /// Calls that began at least one same-provider retry.
    pub retried_calls: u64,
    /// Same-provider retries started, blocking and streaming.
    pub retry_attempts: u64,
    /// Calls that changed provider or model after dispatch; each counted once.
    pub provider_switches: u64,
    /// Streams that delivered at least one content chunk.
    pub first_chunk_samples: u64,
    /// Sum of first-chunk latencies, in milliseconds.
    pub first_chunk_sum_ms: u64,
}

/// One canonical model as observed by this daemon.
#[derive(Debug, Clone, Serialize)]
pub struct LlmModelDiagnostics {
    pub provider: String,
    pub model: String,
    pub samples: u64,
    pub duration_samples: u64,
    pub duration_ratio_samples: u64,
    pub duration_ema_ms: f64,
    pub time_error_ema: f64,
    pub confidence: f64,
    pub confidence_label: &'static str,
    /// Warm estimate, absent until the learner has enough samples.
    pub estimate_ms: Option<f64>,
}

impl LlmModelDiagnostics {
    fn from_model(
        labels: (&str, &str),
        model: &LearningModel,
        confidence: f64,
        estimate: Option<&RuntimeEstimate>,
        min_samples: u64,
    ) -> Self {
        let confidence_label = if model.time_sample_count < min_samples {
            "none"
        } else if confidence < 0.5 {
            "low"
        } else {
            "moderate"
        };
        Self {
            provider: labels.0.to_owned(),
            model: labels.1.to_owned(),
            samples: model.sample_count,
            duration_samples: model.time_sample_count,
            duration_ratio_samples: model.time_ratio_sample_count,
            duration_ema_ms: model.duration_secs * 1_000.0,
            time_error_ema: model.time_error_rate,
            confidence,
            confidence_label,
            estimate_ms: estimate.map(|estimate| estimate.duration.as_secs_f64() * 1_000.0),
        }
    }
}

/// Read-only view of the daemon's LLM duration models.
#[derive(Debug, Clone, Serialize)]
pub struct LlmTimingDiagnostics {
    pub mode: &'static str,
    pub min_samples: u64,
    pub counters: LlmTimingCounterSnapshot,
    pub models: Vec<LlmModelDiagnostics>,
}

impl LlmTimings {
    pub fn new(mode: LlmTimingMode) -> Self {
        Self {
            state: Mutex::new(LlmTimingState::default()),
            counters: LlmTimingCounters::default(),
            mode,
        }
    }

    /// A cache that records nothing; used when timing is switched off.
    pub fn disabled() -> Self {
        Self::new(LlmTimingMode::Disabled)
    }

    /// Read the process setting once. Unknown values keep observing.
    pub fn from_env() -> Self {
        let mode = match std::env::var(ENV_MODE) {
            Ok(value) => LlmTimingMode::from_setting(Some(&value)),
            Err(std::env::VarError::NotPresent) => LlmTimingMode::Observe,
            Err(std::env::VarError::NotUnicode(_)) => LlmTimingMode::Observe,
        };
        Self::new(mode)
    }

    pub fn enabled(&self) -> bool {
        self.mode == LlmTimingMode::Observe
    }

    pub fn mode_str(&self) -> &'static str {
        self.mode.as_str()
    }

    fn train(&self, target: LlmTimingTarget, elapsed: Duration) {
        if let Some(key) = model_key(&target.provider, &target.model)
            && let Ok(mut state) = self.state.lock()
        {
            state.estimator.record_operation_actuals(
                &key,
                Actuals {
                    baseline_time: None,
                    baseline_cost: None,
                    predicted_time: target.predicted,
                    actual_time: elapsed,
                    actual_cost: None,
                },
            );
        }
    }

    fn record_first_chunk(&self, first_chunk: Duration) {
        LlmTimingCounters::bump(&self.counters.first_chunk_samples);
        LlmTimingCounters::add(
            &self.counters.first_chunk_sum_ms,
            u64::try_from(first_chunk.as_millis()).unwrap_or(u64::MAX),
        );
    }

    /// Daemon-wide snapshot. Model names are provider configuration, not user
    /// data, and no call content or identity is included.
    pub fn diagnostics(&self) -> LlmTimingDiagnostics {
        let mut models = Vec::new();
        let mut min_samples = 0;
        if let Ok(state) = self.state.lock() {
            let learner = state.estimator.learner();
            min_samples = learner.min_samples();
            for (key, model) in learner.all_models() {
                let Some(labels) = labels_from_key(key) else {
                    continue;
                };
                let estimate = state.estimator.estimate_operation(key, None, None);
                models.push(LlmModelDiagnostics::from_model(
                    labels,
                    model,
                    learner.confidence(key),
                    estimate.as_ref(),
                    min_samples,
                ));
            }
        }
        models.sort_by(|a, b| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)));
        LlmTimingDiagnostics {
            mode: self.mode.as_str(),
            min_samples,
            counters: self.counters.snapshot(),
            models,
        }
    }
}

impl LlmTimingObserver for LlmTimings {
    fn call_started(&self) {
        if self.enabled() {
            LlmTimingCounters::bump(&self.counters.calls_started);
        }
    }

    fn predict(&self, provider: &str, model: &str) -> Option<Duration> {
        if !self.enabled() {
            return None;
        }
        let key = model_key(provider, model)?;
        self.state
            .lock()
            .ok()?
            .estimator
            .estimate_operation(&key, None, None)
            .map(|estimate| estimate.duration)
    }

    fn call_finished(&self, observation: LlmTimingObservation) {
        if !self.enabled() {
            return;
        }
        let counters = &self.counters;
        LlmTimingCounters::add(&counters.provider_attempts, observation.attempts);
        if observation.retries > 0 {
            LlmTimingCounters::bump(&counters.retried_calls);
            LlmTimingCounters::add(&counters.retry_attempts, observation.retries);
        }
        if observation.switched {
            LlmTimingCounters::bump(&counters.provider_switches);
        }
        if let Some(first_chunk) = observation.first_chunk {
            self.record_first_chunk(first_chunk);
        }
        match observation.outcome {
            LlmTimingOutcome::Completed { elapsed, target } => {
                LlmTimingCounters::bump(&counters.completions);
                if !observation.switched
                    && let Some(target) = target
                {
                    self.train(target, elapsed);
                }
            }
            LlmTimingOutcome::Failed => LlmTimingCounters::bump(&counters.failures),
            LlmTimingOutcome::Cancelled => LlmTimingCounters::bump(&counters.cancellations),
        }
    }
}

fn model_key(provider: &str, model: &str) -> Option<String> {
    // Length framing keeps provider/model pairs distinct even when names
    // contain separators. Neither component comes from request text.
    (valid_timing_label(provider) && valid_timing_label(model))
        .then(|| format!("Llm({}:{provider}{model})", provider.len()))
}

fn labels_from_key(key: &str) -> Option<(&str, &str)> {
    let (provider_len, labels) = key
        .strip_prefix("Llm(")?
        .strip_suffix(')')?
        .split_once(':')?;
    labels.split_at_checked(provider_len.parse().ok()?)
}

impl Default for LlmTimings {
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warmed(mode: LlmTimingMode, model: &str, calls: u64) -> LlmTimings {
        let timings = LlmTimings::new(mode);
        for _ in 0..calls {
            record(&timings, "provider-a", model, 40);
        }
        timings
    }

    fn record(timings: &LlmTimings, provider: &str, model: &str, elapsed_ms: u64) {
        timings.call_started();
        timings.call_finished(LlmTimingObservation {
            outcome: LlmTimingOutcome::Completed {
                elapsed: Duration::from_millis(elapsed_ms),
                target: Some(LlmTimingTarget {
                    provider: provider.into(),
                    model: model.into(),
                    predicted: timings.predict(provider, model),
                }),
            },
            attempts: 1,
            retries: 0,
            switched: false,
            first_chunk: None,
        });
    }

    #[test]
    fn setting_and_disabled_cache() {
        assert_eq!(LlmTimingMode::from_setting(None), LlmTimingMode::Observe);
        assert_eq!(
            LlmTimingMode::from_setting(Some("1")),
            LlmTimingMode::Observe
        );
        assert_eq!(
            LlmTimingMode::from_setting(Some("0")),
            LlmTimingMode::Disabled
        );
        let disabled = LlmTimings::new(LlmTimingMode::Disabled);
        assert!(!disabled.enabled());
        record(&disabled, "provider", "model", 10);
        assert_eq!(disabled.diagnostics().counters.calls_started, 0);
        assert!(disabled.diagnostics().models.is_empty());
    }

    #[test]
    fn hides_estimate_until_warm_and_reports_counters() {
        let timings = LlmTimings::new(LlmTimingMode::Observe);
        for _ in 0..4 {
            record(&timings, "provider-a", "model-a", 50);
        }
        let cold = timings.diagnostics();
        assert_eq!(cold.models.len(), 1);
        assert_eq!(cold.models[0].duration_samples, 4);
        assert_eq!(cold.models[0].confidence_label, "none");
        assert!(cold.models[0].estimate_ms.is_none());

        record(&timings, "provider-a", "model-a", 50);
        let warm = timings.diagnostics();
        assert_eq!(warm.models[0].confidence_label, "low");
        assert!(warm.models[0].estimate_ms.is_some());
        assert_eq!(warm.counters.calls_started, 5);
        assert_eq!(warm.counters.completions, 5);
    }

    #[test]
    fn switches_count_even_when_no_model_is_trained() {
        let timings = LlmTimings::new(LlmTimingMode::Observe);
        timings.call_started();
        timings.call_finished(LlmTimingObservation {
            outcome: LlmTimingOutcome::Completed {
                elapsed: Duration::from_millis(100),
                target: None,
            },
            attempts: 3,
            retries: 1,
            switched: true,
            first_chunk: Some(Duration::from_millis(50)),
        });
        let diagnostics = timings.diagnostics();
        assert!(diagnostics.models.is_empty());
        assert_eq!(diagnostics.counters.completions, 1);
        assert_eq!(diagnostics.counters.provider_switches, 1);
        assert_eq!(diagnostics.counters.provider_attempts, 3);
        assert_eq!(diagnostics.counters.retried_calls, 1);
        assert_eq!(diagnostics.counters.retry_attempts, 1);
        assert_eq!(diagnostics.counters.first_chunk_sum_ms, 50);
    }

    #[test]
    fn providers_serving_the_same_model_are_isolated() {
        let timings = warmed(LlmTimingMode::Observe, "shared-model", 5);
        for _ in 0..5 {
            record(&timings, "provider-b", "shared-model", 400);
        }
        let diagnostics = timings.diagnostics();
        assert_eq!(diagnostics.models.len(), 2);
        let a = &diagnostics.models[0];
        let b = &diagnostics.models[1];
        assert_eq!((&*a.provider, &*a.model), ("provider-a", "shared-model"));
        assert_eq!((&*b.provider, &*b.model), ("provider-b", "shared-model"));
        assert!((a.duration_ema_ms - 40.0).abs() < 1.0);
        assert!((b.duration_ema_ms - 400.0).abs() < 1.0);
    }

    #[test]
    fn labels_are_bounded_and_never_echoed_as_keys() {
        let timings = LlmTimings::new(LlmTimingMode::Observe);
        record(&timings, "provider", &"x".repeat(257), 10);
        record(&timings, "", "model-a", 10);
        record(&timings, "provider", "https://not-a-model.invalid", 10);
        assert!(timings.diagnostics().models.is_empty());
        record(&timings, "provider", ":model-a)", 10);
        record(&timings, "provider:", "model-a)", 30);
        let diagnostics = timings.diagnostics();
        assert_eq!(diagnostics.models.len(), 2);
        assert_eq!(diagnostics.models[0].model, ":model-a)");
        let rendered = serde_json::to_string(&diagnostics).unwrap();
        assert!(!rendered.contains("Llm("));
    }
}
