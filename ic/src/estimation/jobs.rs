//! Bounded job-run estimates (WI#318 Phase C1).
//!
//! Only local scheduler jobs are covered. Sandbox/container jobs
//! (`ContainerJobManager`) and external-worker jobs have their own start and
//! terminal mapping, so `estimated_duration` stays unset for them; a local
//! estimate is never applied to another mode just because it also uses
//! `create_job` (C2).
//!
//! The learner key is a bounded, non-sensitive kind per user: routine-fired
//! runs and user-started jobs train separate models, because they are not
//! comparable completions. Job titles, descriptions and free-form metadata
//! never become keys, so the model cardinality is at most two per user with
//! completed local runs.
//!
//! Duration only: no production job declares a cost baseline, so a job
//! estimate never invents one. Nothing here feeds scheduling, timeouts,
//! approval or cost accounting.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Job metadata key the routine engine sets on the full-job runs it
/// dispatches, so the frozen estimate and the terminal observation read the
/// same [`LocalJobKind`].
pub const JOB_ESTIMATE_KIND_KEY: &str = "estimate_kind";

/// Bounded learner kind for a local scheduler job (WI#318 C1). A routine's
/// recurring run and a job a user starts by hand are not comparable
/// completions, so each has its own model per user. A closed enum keeps the
/// model count bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalJobKind {
    /// A full-job routine run dispatched by the routine engine.
    Routine,
    /// Every other local dispatch: `create_job`, `/job`, a gateway restart.
    User,
}

impl LocalJobKind {
    /// The non-sensitive key suffix and metadata value for this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Routine => "routine",
            Self::User => "user",
        }
    }

    /// The kind recorded in a job's metadata. Only the routine engine writes
    /// `estimate_kind = "routine"`; any other value, or none, is a user-started
    /// job. A restarted job is dispatched without its old metadata and so
    /// counts as user-started.
    pub fn from_metadata(metadata: Option<&serde_json::Value>) -> Self {
        let kind = metadata
            .and_then(|metadata| metadata.get(JOB_ESTIMATE_KIND_KEY))
            .and_then(|value| value.as_str());
        if kind == Some(Self::Routine.as_str()) {
            Self::Routine
        } else {
            Self::User
        }
    }
}

/// Private learner key for one user's local job model of one kind. Length
/// framing keeps identifiers containing `:` unambiguous. Never emit it to an
/// API or log.
pub fn job_key(user_id: &str, kind: LocalJobKind) -> Option<String> {
    if user_id.is_empty() || user_id.len() > 512 {
        return None;
    }
    Some(format!("job:{}:{user_id}:{}", user_id.len(), kind.as_str()))
}

/// Counts for the owner-only diagnostics surface. No category names, user
/// identity or parameters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
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
    fn job_keys_are_user_scoped_framed_and_split_by_kind() {
        let user = LocalJobKind::User;
        let routine = LocalJobKind::Routine;
        assert_eq!(job_key("alice", user).as_deref(), Some("job:5:alice:user"));
        assert_eq!(
            job_key("alice", routine).as_deref(),
            Some("job:5:alice:routine")
        );
        assert_eq!(job_key("", user).as_deref(), None);
        assert_eq!(job_key(&"x".repeat(513), user), None);
        assert_ne!(job_key("alice", user), job_key("alice:user", user));
        assert_ne!(job_key("alice", user), job_key("alice", routine));
    }

    #[test]
    fn job_kind_comes_only_from_the_routine_marker() {
        use serde_json::json;
        assert_eq!(LocalJobKind::from_metadata(None), LocalJobKind::User);
        assert_eq!(
            LocalJobKind::from_metadata(Some(&json!({ "max_iterations": 5 }))),
            LocalJobKind::User
        );
        assert_eq!(
            LocalJobKind::from_metadata(Some(&json!({ JOB_ESTIMATE_KIND_KEY: "routine" }))),
            LocalJobKind::Routine
        );
        for other in [json!("Routine"), json!("user"), json!(1), json!(null)] {
            assert_eq!(
                LocalJobKind::from_metadata(Some(&json!({ JOB_ESTIMATE_KIND_KEY: other }))),
                LocalJobKind::User
            );
        }
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
