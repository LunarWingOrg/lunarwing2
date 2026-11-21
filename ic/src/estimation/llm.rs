//! Daemon-wide LLM logical-call duration calibration (WI#318 Phase B1/B2).
//!
//! `LlmTimings` is owned by the provider chain built once per daemon. Keys are
//! canonical provider model names (`Llm(model)`), carry no user identity,
//! prompt, result or parameter, and are bounded by the learner's model cap.
//! Only durations are learned: a real per-call charge is still recorded by the
//! existing cost guard, so a v2 `cost_usd: 0.0` never becomes a free sample.
//!
//! Nothing here is persisted. A restart starts cold, exactly like the tool
//! cache, and an estimator failure can never change a call's result.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;

use super::learner::LearningModel;
use super::{Actuals, Estimator, RuntimeEstimate};

/// Read once when the provider chain is built; `0` disables recording.
const ENV_MODE: &str = "LUNARWING_LLM_TIMINGS";
/// Upper bound for a canonical model label, mirroring the tool cache.
const MAX_LABEL_BYTES: usize = 256;

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

/// One logical call frozen at entry. The key never leaves the process.
#[derive(Debug, Clone)]
pub struct LlmCall {
    key: String,
    predicted: Option<Duration>,
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
    completions: AtomicU64,
    failures: AtomicU64,
    cancellations: AtomicU64,
    retried_calls: AtomicU64,
    retry_attempts: AtomicU64,
    first_chunk_samples: AtomicU64,
    first_chunk_sum_ms: AtomicU64,
}

impl LlmTimingCounters {
    fn bump(counter: &AtomicU64) -> u64 {
        counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn add(counter: &AtomicU64, value: u64) {
        counter.fetch_add(value, Ordering::Relaxed);
    }

    fn snapshot(&self) -> LlmTimingCounterSnapshot {
        LlmTimingCounterSnapshot {
            calls_started: self.calls_started.load(Ordering::Relaxed),
            completions: self.completions.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            cancellations: self.cancellations.load(Ordering::Relaxed),
            retried_calls: self.retried_calls.load(Ordering::Relaxed),
            retry_attempts: self.retry_attempts.load(Ordering::Relaxed),
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
    /// Calls that produced a terminal success and trained the duration EMA.
    pub completions: u64,
    /// Calls that returned an error; they train nothing.
    pub failures: u64,
    /// Streams dropped before a terminal chunk; they train nothing.
    pub cancellations: u64,
    /// Calls whose stream reported at least one recovered attempt.
    pub retried_calls: u64,
    /// Extra attempts reported beyond the first, summed over all streams.
    pub retry_attempts: u64,
    /// Streams that produced at least one chunk.
    pub first_chunk_samples: u64,
    /// Sum of first-chunk latencies, in milliseconds.
    pub first_chunk_sum_ms: u64,
}

/// One canonical model as observed by this daemon.
#[derive(Debug, Clone, Serialize)]
pub struct LlmModelDiagnostics {
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
        label: &str,
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
            model: label.to_owned(),
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

    /// Start one logical call. The prediction is frozen here so later
    /// completions cannot retroactively change the forecast error.
    pub fn begin(&self, label: &str) -> Option<LlmCall> {
        let key = self.key_for(label)?;
        let predicted = self
            .state
            .lock()
            .ok()?
            .estimator
            .estimate_operation(&key, None, None)
            .map(|estimate| estimate.duration);
        LlmTimingCounters::bump(&self.counters.calls_started);
        Some(LlmCall {
            key,
            predicted,
        })
    }

    /// Record a completed call. `completed_label` is the model that actually
    /// served it, so a failover never trains the primary's model.
    pub fn complete(
        &self,
        call: LlmCall,
        completed_label: &str,
        elapsed: Duration,
        first_chunk: Option<Duration>,
    ) {
        let key = self.key_for(completed_label).unwrap_or(call.key);
        if let Ok(mut state) = self.state.lock() {
            state.estimator.record_operation_actuals(
                &key,
                Actuals {
                    baseline_time: None,
                    baseline_cost: None,
                    predicted_time: call.predicted,
                    actual_time: elapsed,
                    actual_cost: None,
                },
            );
        }
        LlmTimingCounters::bump(&self.counters.completions);
        self.record_first_chunk(first_chunk);
    }

    /// Record a call that returned an error. Nothing is trained.
    pub fn fail(&self) {
        LlmTimingCounters::bump(&self.counters.failures);
    }

    /// Record a stream dropped before its terminal chunk. Nothing is trained.
    pub fn cancel(&self) {
        LlmTimingCounters::bump(&self.counters.cancellations);
    }

    /// Record the recovered-attempt count a retry/failover layer reported.
    pub fn record_retry(&self, attempts: u32) {
        LlmTimingCounters::bump(&self.counters.retried_calls);
        LlmTimingCounters::add(
            &self.counters.retry_attempts,
            u64::from(attempts.saturating_sub(1)),
        );
    }

    fn record_first_chunk(&self, first_chunk: Option<Duration>) {
        let Some(first_chunk) = first_chunk else {
            return;
        };
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
                let Some(label) = key.strip_prefix("Llm(").and_then(|k| k.strip_suffix(')'))
                else {
                    continue;
                };
                let estimate = state.estimator.estimate_operation(key, None, None);
                models.push(LlmModelDiagnostics::from_model(
                    label,
                    model,
                    learner.confidence(key),
                    estimate.as_ref(),
                    min_samples,
                ));
            }
        }
        models.sort_by(|a, b| a.model.cmp(&b.model));
        LlmTimingDiagnostics {
            mode: self.mode.as_str(),
            min_samples,
            counters: self.counters.snapshot(),
            models,
        }
    }

    fn key_for(&self, label: &str) -> Option<String> {
        if !self.enabled() || label.is_empty() || label.len() > MAX_LABEL_BYTES {
            return None;
        }
        Some(format!("Llm({label})"))
    }
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
            let call = timings.begin(model).unwrap();
            timings.complete(call, model, Duration::from_millis(40), None);
        }
        timings
    }

    #[test]
    fn setting_and_disabled_cache() {
        assert_eq!(
            LlmTimingMode::from_setting(None),
            LlmTimingMode::Observe
        );
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
        assert!(disabled.begin("model").is_none());
        assert!(disabled.diagnostics().models.is_empty());
    }

    #[test]
    fn hides_estimate_until_warm_and_reports_counters() {
        let timings = LlmTimings::new(LlmTimingMode::Observe);
        for _ in 0..4 {
            let call = timings.begin("model-a").unwrap();
            timings.complete(call, "model-a", Duration::from_millis(50), None);
        }
        let cold = timings.diagnostics();
        assert_eq!(cold.models.len(), 1);
        assert_eq!(cold.models[0].duration_samples, 4);
        assert_eq!(cold.models[0].confidence_label, "none");
        assert!(cold.models[0].estimate_ms.is_none());

        let call = timings.begin("model-a").unwrap();
        timings.complete(call, "model-a", Duration::from_millis(50), None);
        let warm = timings.diagnostics();
        assert_eq!(warm.models[0].confidence_label, "low");
        assert!(warm.models[0].estimate_ms.is_some());
        assert_eq!(warm.counters.calls_started, 5);
        assert_eq!(warm.counters.completions, 5);
    }

    #[test]
    fn failures_cancellations_and_retries_never_train() {
        let timings = LlmTimings::new(LlmTimingMode::Observe);
        let failed = timings.begin("model-a").unwrap();
        timings.fail();
        drop(failed);
        timings.cancel();
        timings.record_retry(3);
        timings.record_retry(1);

        let diagnostics = timings.diagnostics();
        assert!(diagnostics.models.is_empty());
        assert_eq!(diagnostics.counters.calls_started, 1);
        assert_eq!(diagnostics.counters.completions, 0);
        assert_eq!(diagnostics.counters.failures, 1);
        assert_eq!(diagnostics.counters.cancellations, 1);
        assert_eq!(diagnostics.counters.retried_calls, 2);
        assert_eq!(diagnostics.counters.retry_attempts, 2);
    }

    #[test]
    fn completion_label_attributes_failover_to_the_serving_model() {
        let timings = LlmTimings::new(LlmTimingMode::Observe);
        let call = timings.begin("primary").unwrap();
        timings.complete(call, "fallback", Duration::from_millis(30), None);
        let diagnostics = timings.diagnostics();
        assert_eq!(diagnostics.models.len(), 1);
        assert_eq!(diagnostics.models[0].model, "fallback");
    }

    #[test]
    fn models_are_isolated_and_first_chunk_is_separate() {
        let timings = warmed(LlmTimingMode::Observe, "model-a", 5);
        for _ in 0..5 {
            let call = timings.begin("model-b").unwrap();
            timings.complete(
                call,
                "model-b",
                Duration::from_millis(400),
                Some(Duration::from_millis(20)),
            );
        }
        let diagnostics = timings.diagnostics();
        assert_eq!(diagnostics.models.len(), 2);
        let a = &diagnostics.models[0];
        let b = &diagnostics.models[1];
        assert_eq!(a.model, "model-a");
        assert_eq!(b.model, "model-b");
        assert!((a.duration_ema_ms - 40.0).abs() < 1.0);
        assert!((b.duration_ema_ms - 400.0).abs() < 1.0);
        assert_eq!(diagnostics.counters.first_chunk_samples, 5);
        assert_eq!(diagnostics.counters.first_chunk_sum_ms, 100);
    }

    #[test]
    fn labels_are_bounded_and_never_echoed_as_keys() {
        let timings = LlmTimings::new(LlmTimingMode::Observe);
        assert!(timings.begin(&"x".repeat(MAX_LABEL_BYTES + 1)).is_none());
        assert!(timings.begin("").is_none());
        let call = timings.begin("model-a").unwrap();
        timings.complete(call, "model-a", Duration::from_millis(10), None);
        let rendered = serde_json::to_string(&timings.diagnostics()).unwrap();
        assert!(!rendered.contains("Llm("));
    }
}
