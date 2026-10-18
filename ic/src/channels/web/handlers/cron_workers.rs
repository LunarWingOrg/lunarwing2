//! Owner-only recovery for cron worker missions, independent of the LLM engine.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::server::GatewayState;
use crate::error::OrchestratorError;
use crate::orchestrator::ExternalWorkerManager;
use crate::orchestrator::external_worker::CronCommandOrigin;

/// Overall budget for one control request: enqueue plus the status reply.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CronAction {
    Pause,
    Resume,
    Status,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CronControlRequest {
    action: CronAction,
}

pub async fn cron_worker_status_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(worker): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    control(&state, &user.user_id, &worker, CronAction::Status).await
}

pub async fn cron_worker_control_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(worker): Path<String>,
    Json(request): Json<CronControlRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    control(&state, &user.user_id, &worker, request.action).await
}

/// Enqueue the Pause/Resume mutation for `action`. Returns `Ok(false)` for
/// Status (nothing to enqueue). Completes when the supervisor's command
/// channel accepts the command — which is NOT the same as the supervisor
/// having applied it; only the status reply proves application (MR !1531
/// DQ-4).
async fn enqueue_mutation(
    manager: &ExternalWorkerManager,
    worker: &str,
    user_id: &str,
    action: CronAction,
) -> Result<bool, OrchestratorError> {
    match action {
        CronAction::Pause => manager
            .pause_cron_mission(worker, user_id, CronCommandOrigin::OwnerApi)
            .await
            .map(|()| true),
        CronAction::Resume => manager
            .resume_cron_mission(worker, user_id, CronCommandOrigin::OwnerApi)
            .await
            .map(|()| true),
        CronAction::Status => Ok(false),
    }
}

fn command_error(error: OrchestratorError) -> (StatusCode, String) {
    let code = match error {
        OrchestratorError::CronMissionNotOwned { .. } => StatusCode::FORBIDDEN,
        OrchestratorError::ExternalWorkerNotCron { .. } => StatusCode::NOT_FOUND,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (code, error.to_string())
}

async fn control(
    state: &GatewayState,
    user_id: &str,
    worker: &str,
    action: CronAction,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    // Check identity before looking up a worker or even the manager: a
    // non-owner cannot enumerate mission configuration through errors.
    if user_id != state.owner_id {
        return Err((StatusCode::FORBIDDEN, "Owner access required".to_string()));
    }
    let manager = state.external_worker_manager.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "External worker manager not available".to_string(),
    ))?;
    let deadline = tokio::time::Instant::now() + COMMAND_TIMEOUT;
    // Phase 1 (mutations only): enqueue the command. A timeout here means the
    // channel never accepted it — nothing was enqueued, so 504 is accurate.
    let enqueued =
        match tokio::time::timeout_at(deadline, enqueue_mutation(manager, worker, user_id, action))
            .await
        {
            Ok(Ok(enqueued)) => enqueued,
            Ok(Err(error)) => return Err(command_error(error)),
            Err(_) => {
                return Err((
                    StatusCode::GATEWAY_TIMEOUT,
                    "Cron supervisor did not accept the command in time; nothing was enqueued"
                        .to_string(),
                ));
            }
        };
    // Phase 2: the status reply is queued behind the mutation, so it reflects
    // the applied command rather than merely acknowledging its enqueue. The
    // mission's `durability_error` (WI#371) tells the caller whether the
    // applied state is actually durable across a restart; surface it
    // top-level as `durable` so a failed persist can never look like success.
    match tokio::time::timeout_at(deadline, manager.cron_mission_status(worker, user_id)).await {
        Ok(Ok(status)) => {
            let durable = status.durability_error.is_none();
            Ok((
                StatusCode::OK,
                Json(serde_json::json!({"action": action, "mission": status, "durable": durable})),
            ))
        }
        Ok(Err(error)) => Err(command_error(error)),
        Err(_) if enqueued => {
            // DQ-4: the command was accepted by the queue but not confirmed in
            // time — it may already be applied or still queued. Pause/resume
            // are idempotent (a repeated resume resets the breaker again and
            // writes a duplicate audit row), so report 202 instead of a
            // misleading 504.
            Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({
                    "action": action,
                    "queued": true,
                    "detail": "command accepted by the supervisor queue but not confirmed \
                               in time; re-query status before retrying",
                })),
            ))
        }
        Err(_) => Err((
            StatusCode::GATEWAY_TIMEOUT,
            "Cron supervisor did not respond".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::web::test_helpers::TestGatewayBuilder;
    use crate::orchestrator::external_worker::{CronCommand, CronMissionControl};

    /// Park a control channel whose receiver is never polled: the channel
    /// accepts sends until full but no supervisor ever drains or answers —
    /// the stalled-supervisor fixture for the 10 s bound pins (WI#375 DR-5).
    fn stalled_control(owner: &str) -> (CronMissionControl, tokio::task::JoinHandle<()>) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let parked = tokio::spawn(async move {
            let _rx = rx;
            std::future::pending::<()>().await;
        });
        (
            CronMissionControl {
                owner_id: owner.to_string(),
                tx,
            },
            parked,
        )
    }

    fn insert_control(manager: &ExternalWorkerManager, worker: &str, control: CronMissionControl) {
        manager
            .cron_controls
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(worker.to_string(), control);
    }

    /// WI#375 DR-5: a mutation the stalled supervisor never confirms within
    /// the 10 s budget is `202 Accepted` (queued), not a misleading 504 —
    /// pause/resume are idempotent, so the owner may re-query and retry.
    #[tokio::test]
    async fn stalled_supervisor_unconfirmed_mutation_is_accepted_not_timed_out() {
        let manager = Arc::new(ExternalWorkerManager::new(vec![]));
        let (registration, _parked) = stalled_control("owner-1");
        insert_control(&manager, "mock", registration);
        let state = TestGatewayBuilder::new()
            .user_id("owner-1")
            .external_worker_manager(manager)
            .build();
        let (status, body) = control(&state, "owner-1", "mock", CronAction::Pause)
            .await
            .expect("accepted"); // safety: test
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["queued"], true);
    }

    /// WI#375 DR-5: when the stalled supervisor's queue never accepts the
    /// command inside the 10 s budget, nothing was enqueued — a plain 504.
    #[tokio::test]
    async fn stalled_supervisor_full_queue_is_a_plain_timeout() {
        let manager = Arc::new(ExternalWorkerManager::new(vec![]));
        let (registration, _parked) = stalled_control("owner-1");
        // Fill the bounded supervisor queue so the enqueue itself blocks.
        for _ in 0..16 {
            registration.tx.try_send(CronCommand::FireNow).unwrap(); // safety: test
        }
        insert_control(&manager, "mock", registration);
        let state = TestGatewayBuilder::new()
            .user_id("owner-1")
            .external_worker_manager(manager)
            .build();
        let (status, message) = control(&state, "owner-1", "mock", CronAction::Pause)
            .await
            .expect_err("timeout"); // safety: test
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert!(message.contains("nothing was enqueued"));
    }
}
