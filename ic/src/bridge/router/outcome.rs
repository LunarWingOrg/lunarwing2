//! One completion path for on-time and detached foreground turns (WI#164).

use std::sync::Arc;
use std::time::Duration;

use lunarwing_engine::ThreadOutcome;

use crate::agent::Agent;
use crate::agent::response_delivery::ResponseDelivery;
use crate::channels::{IncomingMessage, StatusUpdate};
use crate::error::Error;
use crate::gate::pending::PendingGate;

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
}

pub(super) async fn await_thread_outcome(
    agent: &Agent,
    state: &EngineState,
    message: &IncomingMessage,
    conversation_id: lunarwing_engine::ConversationId,
    thread_id: lunarwing_engine::ThreadId,
    mut events: tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
) -> Result<Option<String>, Error> {
    let turn = TurnCompletion {
        state: state.clone(),
        delivery: agent.response_delivery(),
        message: message.clone(),
        conversation_id,
        thread_id,
    };
    let mut cancellation = turn.cancellation_guard();
    if !turn.wait(&mut events, OUTCOME_TIMEOUT).await {
        tracing::warn!(%thread_id, "Turn wait budget exhausted; transferring delivery to late watcher");
        tokio::spawn(async move {
            turn.deliver_late(&mut events, LATE_TIMEOUT, STOP_GRACE)
                .await;
        });
        cancellation.disarm();
        return Ok(Some(String::new()));
    }
    let outcome = turn.join().await?;
    cancellation.disarm();
    turn.complete(outcome).await
}

impl TurnCompletion {
    fn cancellation_guard(&self) -> AwaitThreadCancellationGuard {
        AwaitThreadCancellationGuard::new(
            Arc::clone(&self.state.thread_manager),
            self.thread_id,
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
                        if !self.state.thread_manager.is_running(self.thread_id).await {
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
        deliver_thread_event(
            &self.delivery.channels,
            &self.state,
            &self.message,
            &self.thread_id.to_string(),
            event,
        )
        .await;
    }

    async fn join(&self) -> Result<ThreadOutcome, Error> {
        self.state
            .thread_manager
            .join_thread(self.thread_id)
            .await
            .map_err(|error| engine_err("join error", error))
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
        let result = self.late_result(events, budget, stop_grace).await;
        if !self.state.thread_manager.is_running(self.thread_id).await {
            cancellation.disarm();
        }
        match result {
            Ok(Some(text)) => self.delivery.deliver(&self.message, text).await,
            Ok(None) => {}
            Err(error) => {
                tracing::error!(thread_id = %self.thread_id, %error, "Late turn completion failed");
                self.delivery
                    .deliver(
                        &self.message,
                        format!("Error completing the pending turn: {error}"),
                    )
                    .await;
            }
        }
    }

    async fn late_result(
        &self,
        events: &mut tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
        budget: Duration,
        stop_grace: Duration,
    ) -> Result<Option<String>, Error> {
        let timed_out = !self.wait(events, budget).await;
        if timed_out {
            tracing::warn!(thread_id = %self.thread_id, "Late turn budget exhausted; stopping thread");
            let stopped = tokio::time::timeout(stop_grace, async {
                self.state
                    .thread_manager
                    .stop_thread(self.thread_id, &self.message.user_id)
                    .await
                    .map_err(|error| engine_err("stop late thread", error))?;
                Ok::<_, Error>(self.wait(events, stop_grace).await)
            })
            .await;
            if !matches!(stopped, Ok(Ok(true))) {
                // Never remove a still-running handle just to join it. Keep it
                // addressable by /interrupt if cancellation itself is stuck.
                return Err(engine_err(
                    "late turn timeout",
                    "thread did not stop within its grace period",
                ));
            }
        }
        let mut outcome = self.join().await?;
        if timed_out && matches!(outcome, ThreadOutcome::Stopped) {
            // A watchdog stop must finish the pending turn visibly; the empty
            // stopped sentinel is reserved for a user-initiated interrupt.
            outcome = ThreadOutcome::Failed {
                kind: lunarwing_engine::ThreadFailureKind::Orchestrator,
                error:
                    "The turn exceeded its extended time limit and was stopped. Please try again."
                        .into(),
            };
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
        self.state
            .conversation_manager
            .record_thread_outcome(self.conversation_id, self.thread_id, &outcome)
            .await
            .map_err(|error| engine_err("conversation error", error))?;

        if let Some(credential) = auth_credential {
            return self.auth_fallback(credential).await;
        }
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

    async fn write_history(&self, text: &str) {
        let Some(db) = &self.state.db else { return };
        let Some(cid) = resolve_v1_conversation_for_message(db.as_ref(), &self.message).await
        else {
            return;
        };
        if let Some(json) = load_tool_calls_json(&self.state.store, self.thread_id).await
            && let Err(error) = db.add_conversation_message(cid, "tool_calls", &json).await
        {
            tracing::warn!(%error, "failed to persist Engine V2 tool history");
        }
        if let Err(error) = db.add_conversation_message(cid, "assistant", text).await {
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
