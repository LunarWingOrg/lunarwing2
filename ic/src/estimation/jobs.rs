//! Bounded job-run estimates (WI#318 Phase C1).
//!
//! Only local scheduler jobs are covered. Sandbox/container jobs
//! (`ContainerJobManager`) and external-worker jobs have their own start and
//! terminal mapping, so `estimated_duration` stays unset for them; a local
//! estimate is never applied to another mode just because it also uses
//! `create_job` (C2).
//!
//! The learner key is a constant, non-sensitive kind per user. Job titles,
//! descriptions and metadata never become keys, so the model cardinality is
//! bounded by the number of users with completed local runs.
//!
//! Duration only: no production job declares a cost baseline, so a job
//! estimate never invents one. Nothing here feeds scheduling, timeouts,
//! approval or cost accounting.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Canonical learner category for local scheduler jobs.
pub const LOCAL_JOB_KIND: &str = "local";

/// Private learner key for one user's local job model. Length framing keeps
/// identifiers containing `:` unambiguous. Never emit it to an API or log.
pub fn job_key(user_id: &str) -> Option<String> {
    if user_id.is_empty() || user_id.len() > 512 {
        return None;
    }
    Some(format!(
        "job:{}:{user_id}{LOCAL_JOB_KIND}",
        user_id.len()
    ))
}

/// Counts for the owner-only diagnostics surface. No category names, user
/// identity or parameters.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct JobEstimateCounters {
    pub estimates_offered: u64,
    pub estimates_withheld: u64,
    pub completed_runs: u64,
    pub non_completed_runs: u64,
}

impl JobEstimateCounters {
    pub(super) fn record_estimate(&mut self, offered: bool) {
        let counter = if offered {
            &mut self.estimates_offered
        } else {
            &mut self.estimates_withheld
        };
        *counter = counter.saturating_add(1);
    }

    pub(super) fn record_terminal(&mut self, completed: bool) {
        let counter = if completed {
            &mut self.completed_runs
        } else {
            &mut self.non_completed_runs
        };
        *counter = counter.saturating_add(1);
    }
}

/// Advisory timing shown for one job. `elapsed_secs` is measured, not
/// estimated. `estimated_total_secs` is the historical duration of comparable
/// completed local runs, frozen when the job was dispatched. A remaining time
/// is reported only when a finite plan exists; open-ended local jobs report
/// `unknown` instead of inventing one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobTimingView {
    pub elapsed_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_total_secs: Option<u64>,
    /// `unknown` while the job runs, `not_applicable` once it is finished.
    pub remaining_status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_secs: Option<u64>,
}

impl JobTimingView {
    /// Build the view from persisted timestamps. A missing `started_at` keeps
    /// elapsed unknown rather than reporting zero.
    pub fn new(
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
        estimated_total: Option<Duration>,
        now: DateTime<Utc>,
    ) -> Self {
        let elapsed_secs = started_at.map(|start| {
            let end = completed_at.unwrap_or(now);
            (end - start).num_seconds().max(0) as u64
        });
        let (remaining_status, remaining_secs) = if completed_at.is_some() {
            ("not_applicable", None)
        } else {
            ("unknown", None)
        };
        Self {
            elapsed_secs,
            estimated_total_secs: estimated_total.map(|duration| duration.as_secs()),
            remaining_status,
            remaining_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_keys_are_user_scoped_and_framed() {
        assert_eq!(job_key("alice").as_deref(), Some("job:5:alicelocal"));
        assert_eq!(job_key("").as_deref(), None);
        assert_eq!(job_key(&"x".repeat(513)), None);
        assert_ne!(job_key("alice"), job_key("alice:local"));
    }

    #[test]
    fn timing_separates_elapsed_from_unknown_remaining() {
        let start = Utc::now() - chrono::Duration::seconds(30);
        let view = JobTimingView::new(Some(start), None, Some(Duration::from_secs(90)), Utc::now());
        assert_eq!(view.elapsed_secs, Some(30));
        assert_eq!(view.estimated_total_secs, Some(90));
        assert_eq!(view.remaining_status, "unknown");
        assert_eq!(view.remaining_secs, None);

        let finished = JobTimingView::new(
            Some(start),
            Some(start + chrono::Duration::seconds(12)),
            Some(Duration::from_secs(90)),
            Utc::now(),
        );
        assert_eq!(finished.elapsed_secs, Some(12));
        assert_eq!(finished.remaining_status, "not_applicable");
    }

    #[test]
    fn missing_start_keeps_elapsed_unknown() {
        let view = JobTimingView::new(None, None, None, Utc::now());
        assert_eq!(view.elapsed_secs, None);
        assert_eq!(view.remaining_status, "unknown");
    }
}
