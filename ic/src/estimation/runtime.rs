//! Registry-owned, user-isolated tool calibration. No I/O or process globals.
//!
//! The learner bounds its model count; canceled/failed/timed-out calls do not
//! train successful runtime. Only short statistical updates hold the mutex.

use std::sync::Mutex;
use std::time::Duration;

use rust_decimal::Decimal;

use crate::tools::Tool;

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
}

/// Process-lifetime cache owned by one tool registry. Restart starts cold.
pub struct ToolEstimates {
    estimator: Mutex<Estimator>,
    mode: ToolEstimationMode,
}

/// Per-invocation data frozen before execution; contains no arguments/results.
pub(crate) struct ToolObservation {
    key: String,
    actuals: Actuals,
}

impl ToolEstimates {
    pub fn new(mode: ToolEstimationMode) -> Self {
        Self {
            estimator: Mutex::new(Estimator::new()),
            mode,
        }
    }

    pub fn display_enabled(&self) -> bool {
        self.mode == ToolEstimationMode::Display
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
        self.estimator
            .lock()
            .ok()?
            .estimate_operation(&key, baseline_time, baseline_cost)
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
        let predicted_time = self
            .estimator
            .lock()
            .ok()?
            .estimate_operation(&key, baseline_time, baseline_cost)
            .map(|estimate| estimate.duration)
            .or(baseline_time);
        Some(ToolObservation {
            key,
            actuals: Actuals {
                baseline_time,
                baseline_cost,
                predicted_time,
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
        Self::new(ToolEstimationMode::from_setting(
            std::env::var("LUNARWING_TOOL_ESTIMATES").ok().as_deref(),
        ))
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
