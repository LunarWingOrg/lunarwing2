//! One completion path for on-time and detached foreground turns (WI#164).
//!
//! It also refines how the agent loop reports the message (TUI design 8.2.3): the loop
//! only sees the returned text, so the completion records an outcome hint for stopped,
//! failed and detached turns, and the late watcher reports the second `Finished` of a
//! detached turn itself.

use std::sync::Arc;
use std::time::Duration;

use lunarwing_engine::ThreadOutcome;

use crate::agent::Agent;
use crate::agent::response_delivery::ResponseDelivery;
use crate::channels::{IncomingMessage, MessageLifecycle, StatusUpdate, TurnOutcome};
use crate::error::Error;
use crate::gate::pending::PendingGate;
use crate::history::roles::{ASSISTANT_ROLE, TOOL_CALLS_ROLE, TURN_RECOVERY_ROLE};

use super::{
    AwaitThreadCancellationGuard, EngineState, deliver_thread_event, drain_pending_thread_events,
    engine_err, engine_status_metadata, insert_and_notify_pending_gate, load_tool_calls_json,
    persist_and_notify_thread_gate, resolve_v1_conversation_for_message, thread_outcome_response,
};

const OUTCOME_TIMEOUT: Duration =
    Duration::from_secs(lunarwing_engine::executor::orchestrator::ORCHESTRATOR_TIMEOUT_SECS + 10);
const LATE_TIMEOUT: Duration =
    Duration::from_secs(lunarwing_engine::executor::orchestrator::ORCHESTRATOR_TIMEOUT_SECS);
const STOP_GRACE: Duration = Duration::from_secs(60);

struct TurnCompletion {
    state: EngineState,
    delivery: ResponseDelivery,
    message: IncomingMessage,
    conversation_id: lunarwing_engine::ConversationId,
    thread_id: lunarwing_engine::ThreadId,
    run_started_at: chrono::DateTime<chrono::Utc>,
    history: Option<HistoryReference>,
}

#[derive(serde::Deserialize)]
struct HistoryReference {
    thread_id: lunarwing_engine::ThreadId,
    user_message_id: uuid::Uuid,
    started_at: chrono::DateTime<chrono::Utc>,
}

/// Single-slot key written before WI#517. Read-only fallback so a turn that
/// started under the old write path can still record its terminal markers.
const LEGACY_HISTORY_KEY: &str = "engine_turn_history";
const HISTORY_KEY_PREFIX: &str = "engine_turn_history.";

/// Per-thread metadata key (WI#517). Every turn writes its own key, and both
/// backends merge a single key atomically, so overlapping turns on one
/// conversation can never overwrite each other's reference.
pub(super) fn turn_history_metadata_key(thread_id: lunarwing_engine::ThreadId) -> String {
    format!("{HISTORY_KEY_PREFIX}{thread_id}")
}

async fn history_reference(
    state: &EngineState,
    message: &IncomingMessage,
    thread_id: lunarwing_engine::ThreadId,
) -> Option<HistoryReference> {
    let db = state.db.as_ref()?;
    let cid = resolve_v1_conversation_for_message(db.as_ref(), message).await?;
    let metadata = db.get_conversation_metadata(cid).await.ok()??;
    let key = turn_history_metadata_key(thread_id);
    let stored = metadata
        .get(key.as_str())
        .or_else(|| metadata.get(LEGACY_HISTORY_KEY))?;
    let reference: HistoryReference = serde_json::from_value(stored.clone()).ok()?;
    (reference.thread_id == thread_id).then_some(reference)
}

pub(super) async fn await_thread_outcome(
    agent: &Agent,
    state: &EngineState,
    message: &IncomingMessage,
    conversation_id: lunarwing_engine::ConversationId,
    thread_id: lunarwing_engine::ThreadId,
    events: tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
) -> Result<Option<String>, Error> {
    let run_started_at = state
        .thread_manager
        .thread_run_started_at(thread_id)
        .await
        .map_err(|error| engine_err("thread lookup", error))?
        .ok_or_else(|| {
            engine_err(
                "thread lookup",
                lunarwing_engine::EngineError::ThreadNotFound(thread_id),
            )
        })?;
    let turn = TurnCompletion {
        state: state.clone(),
        delivery: agent.response_delivery(),
        message: message.clone(),
        conversation_id,
        thread_id,
        run_started_at,
        history: history_reference(state, message, thread_id).await,
    };
    turn.await_outcome(events, OUTCOME_TIMEOUT).await
}

impl TurnCompletion {
    /// Waits up to `budget` for the thread, then completes the turn. Past the budget
    /// the late watcher takes over delivery, and the agent loop reports this message
    /// `Finished(Detached)` (the hint below); the watcher reports its real finish. When
    /// the loop's soft timeout already finished the message, the hint is dropped and the
    /// watcher only delivers.
    async fn await_outcome(
        self,
        mut events: tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
        budget: Duration,
    ) -> Result<Option<String>, Error> {
        let mut cancellation = self.cancellation_guard();
        if !self.wait(&mut events, budget).await {
            tracing::warn!(thread_id = %self.thread_id, "Turn wait budget exhausted; transferring delivery to late watcher");
            self.delivery
                .channels
                .note_turn_outcome(self.message.id, TurnOutcome::Detached);
            tokio::spawn(async move {
                self.deliver_late(&mut events, LATE_TIMEOUT, STOP_GRACE)
                    .await;
            });
            cancellation.disarm();
            return Ok(Some(String::new()));
        }
        let Some(outcome) = self.join().await? else {
            cancellation.disarm();
            return Ok(Some(String::new()));
        };
        cancellation.disarm();
        self.complete_once(outcome).await
    }

    fn cancellation_guard(&self) -> AwaitThreadCancellationGuard {
        AwaitThreadCancellationGuard::new(
            Arc::clone(&self.state.thread_manager),
            self.thread_id,
            self.run_started_at,
            self.message.user_id.clone(),
        )
    }

    /// Poll while retaining the thread in the manager so /interrupt still works.
    /// The outer timeout also bounds status delivery and cannot be starved by
    /// continuous events from this or another thread.
    async fn wait(
        &self,
        events: &mut tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
        budget: Duration,
    ) -> bool {
        tokio::time::timeout(budget, async {
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            let mut events_open = true;
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        if !self.state.thread_manager.is_run_running(self.thread_id, self.run_started_at).await {
                            for event in drain_pending_thread_events(events, self.thread_id) {
                                self.forward(&event).await;
                            }
                            return;
                        }
                    }
                    event = events.recv(), if events_open => match event {
                        Ok(event) if event.thread_id == self.thread_id => self.forward(&event).await,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => events_open = false,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(thread_id = %self.thread_id, skipped, "engine event receiver lagged");
                        }
                        _ => {}
                    }
                }
            }
        }).await.is_ok()
    }

    async fn forward(&self, event: &lunarwing_engine::ThreadEvent) {
        if let Some(recovery) = super::model_recovery_info(event) {
            let state = match recovery.recovery_state.as_str() {
                "retrying" => "Retrying",
                "failed" => "Failed",
                _ => "Processing",
            };
            let completed_at = (state == "Failed").then(|| event.timestamp.to_rfc3339());
            self.write_recovery(recovery, state, completed_at).await;
        }
        deliver_thread_event(
            &self.delivery.channels,
            &self.state,
            &self.message,
            &self.thread_id.to_string(),
            event,
        )
        .await;
    }

    async fn join(&self) -> Result<Option<ThreadOutcome>, Error> {
        match self
            .state
            .thread_manager
            .join_thread_for_run(self.thread_id, self.run_started_at)
            .await
        {
            Ok(outcome) => Ok(Some(outcome)),
            Err(lunarwing_engine::EngineError::Thread(lunarwing_engine::ThreadError::Waiting(
                _,
            ))) => Ok(None),
            Err(error) => Err(engine_err("join error", error)),
        }
    }

    async fn deliver_late(
        self,
        events: &mut tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
        budget: Duration,
        stop_grace: Duration,
    ) {
        let mut cancellation = self.cancellation_guard();
        // Emit the waiting status before the late result, never racing it from
        // the returning caller. Bound a slow channel so it cannot stall recovery.
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            self.delivery.channels.send_status(
                &self.message.channel,
                StatusUpdate::Status("This turn is still running. Its result will arrive here when it finishes; /interrupt can stop it.".into()),
                &engine_status_metadata(&self.message, &self.thread_id.to_string()),
            ),
        ).await;
        let result = self
            .late_result(events, budget, stop_grace, stop_grace)
            .await;
        if !self
            .state
            .thread_manager
            .is_run_running(self.thread_id, self.run_started_at)
            .await
        {
            cancellation.disarm();
        }
        let outcome = self.deliver_late_result(result).await;
        self.report_late_finish(outcome).await;
    }

    /// Delivers the late result and rates it like the agent loop rates a handler result
    /// (TUI design 8.2.2): a reply is `replied` only once it reached the channel.
    async fn deliver_late_result(&self, result: Result<Option<String>, Error>) -> TurnOutcome {
        match result {
            Ok(Some(text)) if !text.is_empty() => {
                self.delivery.deliver(&self.message, text).await.outcome()
            }
            Ok(_) => TurnOutcome::NoReply,
            Err(error) => {
                tracing::error!(thread_id = %self.thread_id, %error, "Late turn completion failed");
                self.delivery
                    .deliver(&self.message, error.user_facing_message())
                    .await;
                TurnOutcome::Failed
            }
        }
    }

    /// The one `Finished` that follows `Detached`, after the late reply; hints the late
    /// completion recorded (interrupted, failed, paused) apply. Skipped when the agent
    /// loop already ended the message otherwise: its soft timeout can fire before this
    /// turn detached, and then `timed_out` was the message's last `Finished`.
    async fn report_late_finish(&self, outcome: TurnOutcome) {
        let channels = &self.delivery.channels;
        if channels.has_finished(self.message.id) {
            tracing::debug!(
                thread_id = %self.thread_id,
                %outcome,
                "Late turn finished after the agent loop ended its message; no second finish"
            );
            return;
        }
        channels
            .message_lifecycle(&self.message, MessageLifecycle::Finished(outcome))
            .await;
    }

    /// Once `budget` expires, the independent stop and post-stop graces bound a
    /// stuck turn's failure report to their sum: 120 seconds in production
    /// (plus channel delivery), rather than one shared 60-second grace (WI#673).
    async fn late_result(
        &self,
        events: &mut tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
        budget: Duration,
        stop_grace: Duration,
        post_stop_grace: Duration,
    ) -> Result<Option<String>, Error> {
        if self.wait(events, budget).await {
            return self.join_late(false).await;
        }
        self.late_result_after_budget(events, stop_grace, post_stop_grace)
            .await
    }

    /// The path of [`Self::late_result`] once the late budget ran out: stop the
    /// run, then join it.
    async fn late_result_after_budget(
        &self,
        events: &mut tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
        stop_grace: Duration,
        post_stop_grace: Duration,
    ) -> Result<Option<String>, Error> {
        let stopped = self
            .stop_late_thread(events, stop_grace, post_stop_grace)
            .await?;
        self.join_late(stopped).await
    }

    async fn await_late_stop_request(
        stop_grace: Duration,
        stop: impl std::future::Future<Output = Result<(), lunarwing_engine::EngineError>>,
    ) -> Result<Result<(), lunarwing_engine::EngineError>, Error> {
        tokio::time::timeout(stop_grace, stop).await.map_err(|_| {
            engine_err(
                "late turn timeout",
                "stop request did not complete within its grace period",
            )
        })
    }

    async fn require_late_stop_settled(
        wait: impl std::future::Future<Output = bool>,
    ) -> Result<(), Error> {
        if wait.await {
            return Ok(());
        }
        Err(engine_err(
            "late turn timeout",
            "stop succeeded but the post-stop wait did not complete within its grace period",
        ))
    }

    /// Stops a run whose late budget ran out. `Ok(true)`: the stop request
    /// landed and the run settled. `Ok(false)`: there was nothing to stop.
    ///
    /// The second case is the run that ended, or paused at a gate, since the
    /// wait's last tick. It is no longer registered, so the engine answers
    /// `ThreadNotFound`. That is a miss, not a failure: the engine's own
    /// shutdown path treats it the same way (`ThreadManager::wait_thread_or_stop`),
    /// and the caller joins the run and delivers its real result (WI#627).
    async fn stop_late_thread(
        &self,
        events: &mut tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
        stop_grace: Duration,
        post_stop_grace: Duration,
    ) -> Result<bool, Error> {
        tracing::debug!(thread_id = %self.thread_id, "Late turn budget exhausted; requesting thread stop");
        // Leave the handle addressable by /interrupt on either timeout; joining
        // could otherwise remove a still-running thread.
        let stop = Self::await_late_stop_request(
            stop_grace,
            self.state.thread_manager.stop_thread_for_run(
                self.thread_id,
                self.run_started_at,
                &self.message.user_id,
            ),
        )
        .await?;
        match stop {
            Ok(()) => {
                tracing::warn!(thread_id = %self.thread_id, "Late turn budget exhausted; stopping thread");
            }
            Err(lunarwing_engine::EngineError::ThreadNotFound(_)) => {
                tracing::debug!(
                    thread_id = %self.thread_id,
                    "Late turn was no longer running when its stop was requested"
                );
                for event in drain_pending_thread_events(events, self.thread_id) {
                    self.forward(&event).await;
                }
                return Ok(false);
            }
            Err(error) => return Err(engine_err("stop late thread", error)),
        }
        Self::require_late_stop_settled(self.wait(events, post_stop_grace)).await?;
        Ok(true)
    }

    /// Joins a late run. `stopped_by_watchdog` is true only when this turn's
    /// own stop request landed. If that request raced a user interrupt whose
    /// bookkeeping was still in progress, the time-limit outcome is retained:
    /// the engine does not record which accepted stop request came first.
    async fn join_late(&self, stopped_by_watchdog: bool) -> Result<Option<String>, Error> {
        let Some(mut outcome) = self.join().await? else {
            return Ok(None);
        };
        if stopped_by_watchdog && matches!(outcome, ThreadOutcome::Stopped) {
            // A watchdog stop must finish the pending turn visibly; the empty
            // stopped sentinel is reserved for a user-initiated interrupt.
            outcome = ThreadOutcome::Failed {
                kind: lunarwing_engine::ThreadFailureKind::Orchestrator,
                error:
                    "The turn exceeded its extended time limit and was stopped. Please try again."
                        .into(),
            };
        }
        self.complete_once(outcome).await
    }

    async fn complete_once(&self, outcome: ThreadOutcome) -> Result<Option<String>, Error> {
        if !self
            .state
            .thread_manager
            .claim_thread_completion(self.thread_id, self.run_started_at)
            .await
        {
            return Ok(Some(String::new()));
        }
        self.complete(outcome).await
    }

    /// Transform first, then record once, persist gates/history, and return only
    /// ordinary terminal text. The caller owns the single outbound delivery.
    async fn complete(&self, mut outcome: ThreadOutcome) -> Result<Option<String>, Error> {
        let mut auth_credential = None;
        if let ThreadOutcome::Completed {
            response: Some(text),
        } = &mut outcome
        {
            *text = self
                .delivery
                .transform(
                    &self.message,
                    &self.thread_id.to_string(),
                    std::mem::take(text),
                )
                .await
                .unwrap_or_default();
            auth_credential = auth_fallback_credential(text);
            if auth_credential.is_some() {
                // A credential prompt is control state, not assistant prose.
                text.clear();
            }
        }
        if !matches!(&outcome, ThreadOutcome::GatePaused { .. }) {
            self.reap_terminal_gates().await;
        }
        self.state
            .conversation_manager
            .record_thread_outcome(self.conversation_id, self.thread_id, &outcome)
            .await
            .map_err(|error| engine_err("conversation error", error))?;
        self.write_recovery_outcome(&outcome).await;

        if let Some(credential) = auth_credential {
            return self.auth_fallback(credential).await;
        }
        self.note_outcome_hint(&outcome);
        let result = match outcome {
            gate @ ThreadOutcome::GatePaused { .. } => {
                persist_and_notify_thread_gate(
                    &self.delivery.channels,
                    &self.state,
                    &self.message,
                    self.conversation_id,
                    self.thread_id,
                    gate,
                )
                .await?
            }
            other => thread_outcome_response(&other),
        };
        if let Some(text) = result.as_deref().filter(|text| !text.is_empty()) {
            self.write_history(text).await;
        }
        Ok(result)
    }

    /// The returned text alone reads as a reply or no reply, so stopped and failed
    /// turns leave a hint for the `Finished` of this message (TUI design 8.2.3). A
    /// pause is noted where its gate is announced (`insert_and_notify_pending_gate`).
    fn note_outcome_hint(&self, outcome: &ThreadOutcome) {
        let hint = match outcome {
            ThreadOutcome::Stopped => TurnOutcome::Interrupted,
            ThreadOutcome::Failed { .. } | ThreadOutcome::MaxIterations => TurnOutcome::Failed,
            ThreadOutcome::Completed { .. } | ThreadOutcome::GatePaused { .. } => return,
        };
        self.delivery
            .channels
            .note_turn_outcome(self.message.id, hint);
    }

    async fn reap_terminal_gates(&self) {
        let discarded = self
            .state
            .pending_gates
            .drain_for_thread(self.thread_id)
            .await;
        for gate in &discarded {
            tracing::warn!(
                user_id = %gate.user_id,
                thread_id = %gate.thread_id,
                request_id = %gate.request_id,
                gate = %gate.gate_name,
                action = %gate.action_name,
                "engine v2: discarded pending gate; owning turn reached a terminal outcome"
            );
        }
        super::broadcast_dead_gate_resolutions(&self.state, &discarded);
    }

    async fn write_recovery(
        &self,
        recovery: crate::channels::web::types::TurnRecoveryInfo,
        state: &str,
        completed_at: Option<String>,
    ) {
        let Some(reference) = &self.history else {
            return;
        };
        let user_message_id = reference.user_message_id;
        let Some(db) = &self.state.db else { return };
        let Some(cid) = resolve_v1_conversation_for_message(db.as_ref(), &self.message).await
        else {
            return;
        };
        let marker = serde_json::json!({
            "user_message_id": user_message_id,
            "state": state,
            "completed_at": completed_at,
            "recovery": recovery,
        });
        if let Err(error) = db
            .add_conversation_message(cid, TURN_RECOVERY_ROLE, &marker.to_string())
            .await
        {
            tracing::warn!(%error, "failed to persist Engine V2 recovery status");
        }
    }

    async fn write_recovery_outcome(&self, outcome: &ThreadOutcome) {
        let Some(reference) = &self.history else {
            return;
        };
        let Ok(Some(thread)) = self.state.store.load_thread(self.thread_id).await else {
            return;
        };
        let Some(mut recovery) = thread
            .events
            .iter()
            .rev()
            .filter(|event| event.timestamp >= reference.started_at)
            .find_map(super::model_recovery_info)
        else {
            return;
        };
        let state = match outcome {
            ThreadOutcome::Completed { .. } => {
                recovery.recovery_state = "recovered".into();
                recovery.error = "The model recovered and the turn completed.".into();
                "Completed"
            }
            ThreadOutcome::Failed { .. } => {
                recovery.recovery_state = "failed".into();
                recovery.error = "The turn failed after model recovery; no retry is running. Check for an existing job id before re-dispatching.".into();
                "Failed"
            }
            ThreadOutcome::Stopped => {
                recovery.recovery_state = "interrupted".into();
                recovery.error = "The turn was interrupted during model recovery.".into();
                "Interrupted"
            }
            _ => return,
        };
        self.write_recovery(recovery, state, Some(chrono::Utc::now().to_rfc3339()))
            .await;
    }

    async fn write_history(&self, text: &str) {
        let Some(db) = &self.state.db else { return };
        let Some(cid) = resolve_v1_conversation_for_message(db.as_ref(), &self.message).await
        else {
            return;
        };
        if let Some(json) = load_tool_calls_json(&self.state.store, self.thread_id).await
            && let Err(error) = db
                .add_conversation_message(cid, TOOL_CALLS_ROLE, &json)
                .await
        {
            tracing::warn!(%error, "failed to persist Engine V2 tool history");
        }
        if let Err(error) = db.add_conversation_message(cid, ASSISTANT_ROLE, text).await {
            tracing::warn!(%error, "failed to persist Engine V2 assistant history");
        }
    }

    async fn auth_fallback(&self, credential_name: String) -> Result<Option<String>, Error> {
        let instructions = self
            .state
            .auth_manager
            .as_ref()
            .and_then(|mgr| mgr.get_setup_instructions(&credential_name))
            .unwrap_or_else(|| format!("Provide your {credential_name} token"));
        let pending = PendingGate {
            request_id: uuid::Uuid::new_v4(),
            gate_name: "authentication".into(),
            user_id: self.message.user_id.clone(),
            thread_id: self.thread_id,
            conversation_id: self.conversation_id,
            source_channel: self.message.channel.clone(),
            action_name: "authentication_fallback".into(),
            call_id: format!("fallback-auth-{}", self.thread_id),
            parameters: serde_json::json!({ "credential_name": credential_name }),
            display_parameters: None,
            description: format!("Authentication required for '{credential_name}'."),
            resume_kind: lunarwing_engine::ResumeKind::Authentication {
                credential_name,
                instructions,
                auth_url: None,
            },
            created_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + self.state.pending_gate_expiry,
            original_message: Some(self.message.content.clone()),
            resume_output: None,
        };
        insert_and_notify_pending_gate(&self.delivery.channels, &self.state, &self.message, pending)
            .await
    }
}

/// A mention of the marker alone is ordinary text, never a hook exemption or
/// a request to store a credential named "unknown".
fn auth_fallback_credential(text: &str) -> Option<String> {
    if !text.contains("authentication_required") {
        return None;
    }
    text.split_once("credential_name")?
        .1
        .split(['"', '\'', '`'])
        .find(|part| !part.is_empty() && !part.contains(':') && !part.contains(' '))
        .filter(|name| {
            name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .map(str::to_string)
}

#[cfg(test)]
mod tests;
