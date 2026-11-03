use std::sync::Mutex;

use async_trait::async_trait;
use lunarwing_engine::{
    CapabilityRegistry, ConversationManager, LeaseManager, LlmBackend, LlmOutput, LlmResponse,
    PolicyEngine, ThreadManager, TokenUsage,
};
use tokio::sync::{Notify, broadcast};

use super::*;
use crate::bridge::router::{
    PendingGateResolution, engine_conversation_key, matching_engine_approval_gate,
    matching_engine_auth_gate,
    tests::{
        LifecycleLog, LifecycleProbe, TestStore, make_expected_test_state, reported_finish,
        sample_pending_gate, save_waiting_thread,
    },
};
use crate::channels::OutgoingResponse;
use crate::hooks::{
    Hook, HookContext, HookError, HookEvent, HookFailureMode, HookOutcome, HookPoint, HookRegistry,
};
use crate::testing::StubChannel;

struct ControlledLlm {
    started: Notify,
    release: Notify,
    text: String,
}

#[async_trait]
impl LlmBackend for ControlledLlm {
    fn model_name(&self) -> &str {
        "late-result-test"
    }
    async fn complete(
        &self,
        _: &[lunarwing_engine::ThreadMessage],
        _: &[lunarwing_engine::ActionDef],
        _: &lunarwing_engine::LlmCallConfig,
    ) -> Result<LlmOutput, lunarwing_engine::EngineError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(LlmOutput {
            response: LlmResponse::Text(self.text.clone()),
            usage: TokenUsage::default(),
            finish_reason: "stop".into(),
        })
    }
}

enum Policy {
    Modify,
    Reject,
    Fail,
    OutboundReject,
    RemoveAuth,
}
struct TestPolicy(Policy);

#[async_trait]
impl Hook for TestPolicy {
    fn name(&self) -> &str {
        "late-response-policy"
    }
    fn hook_points(&self) -> &[HookPoint] {
        if matches!(self.0, Policy::OutboundReject) {
            &[HookPoint::BeforeOutbound]
        } else {
            &[HookPoint::TransformResponse]
        }
    }
    fn failure_mode(&self) -> HookFailureMode {
        HookFailureMode::FailClosed
    }
    async fn execute(&self, event: &HookEvent, _: &HookContext) -> Result<HookOutcome, HookError> {
        match self.0 {
            Policy::Modify => {
                let HookEvent::ResponseTransform { response, .. } = event else {
                    panic!("wrong hook")
                };
                Ok(HookOutcome::modify(format!("transformed: {response}")))
            }
            Policy::Reject | Policy::OutboundReject => Ok(HookOutcome::reject("test rejection")),
            Policy::RemoveAuth => Ok(HookOutcome::modify("Use another method.".into())),
            Policy::Fail => Err(HookError::ExecutionFailed {
                reason: "test failure".into(),
            }),
        }
    }
}

struct Rig {
    turn: TurnCompletion,
    llm: Arc<ControlledLlm>,
    events: broadcast::Receiver<lunarwing_engine::ThreadEvent>,
    replies: Arc<Mutex<Vec<(IncomingMessage, OutgoingResponse)>>>,
    statuses: Arc<Mutex<Vec<StatusUpdate>>>,
    _disk: tempfile::TempDir,
    #[cfg(feature = "libsql")]
    _db_disk: tempfile::TempDir,
}

// Keep the helper's test-only boundary explicit for per-file CI source checks.
#[cfg(test)]
impl Rig {
    async fn new(response: Option<&str>, policy: Option<Policy>) -> Self {
        let store = Arc::new(TestStore::new());
        let mut state = make_expected_test_state(Arc::clone(&store));
        let llm = Arc::new(ControlledLlm {
            started: Notify::new(),
            release: Notify::new(),
            text: response.unwrap_or_default().into(),
        });
        state.thread_manager = Arc::new(ThreadManager::new(
            llm.clone(),
            state.effect_adapter.clone(),
            store.clone(),
            Arc::new(CapabilityRegistry::new()),
            Arc::new(LeaseManager::new()),
            Arc::new(PolicyEngine::new()),
        ));
        state.conversation_manager = Arc::new(ConversationManager::new(
            state.thread_manager.clone(),
            store.clone(),
        ));
        let disk = tempfile::tempdir().unwrap();
        state.pending_gates = Arc::new(crate::gate::store::PendingGateStore::new(Some(Arc::new(
            crate::gate::persistence::FileGatePersistence::new(disk.path().join("gates.json")),
        ))));
        #[cfg(feature = "libsql")]
        let db_disk = {
            let (db, disk) = crate::testing::test_db().await;
            state.db = Some(db);
            disk
        };
        let thread = save_waiting_thread(&store, state.default_project_id, "alice").await;
        let message = IncomingMessage::new("xmpp", "alice", "original request")
            .with_thread("room-thread")
            .with_conversation_scope("dm:alice@example.test")
            .with_metadata(
                serde_json::json!({"sender": "alice@example.test", "route": "original-route"}),
            );
        let conversation_id = state
            .conversation_manager
            .get_or_create_conversation(&engine_conversation_key(&message), "alice")
            .await
            .unwrap();
        let channels = Arc::new(crate::channels::ChannelManager::new());
        let (channel, _sender) = StubChannel::new("xmpp");
        let replies = channel.captured_responses_handle();
        let statuses = channel.captured_statuses_handle();
        channels.add(Box::new(channel)).await;
        let hooks = Arc::new(HookRegistry::default());
        if let Some(policy) = policy {
            hooks.register(Arc::new(TestPolicy(policy))).await;
        }
        let events = state.thread_manager.subscribe_events();
        if response.is_some() {
            state
                .thread_manager
                .resume_thread(thread.id, "alice", None, None, None)
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), llm.started.notified())
                .await
                .unwrap();
        }
        Self {
            turn: TurnCompletion {
                state,
                delivery: ResponseDelivery {
                    channels,
                    hooks,
                    owner_id: "alice".into(),
                    coalesce: None,
                },
                message,
                conversation_id,
                thread_id: thread.id,
                run_started_at: thread.created_at,
                history: None,
            },
            llm,
            events,
            replies,
            statuses,
            _disk: disk,
            #[cfg(feature = "libsql")]
            _db_disk: db_disk,
        }
    }

    async fn finish(mut self, expected: Option<&str>) {
        // Force the initial wait to expire while the real model call is blocked.
        assert!(
            !self
                .turn
                .wait(&mut self.events, Duration::from_millis(1))
                .await
        );
        assert!(
            self.turn
                .state
                .thread_manager
                .is_running(self.turn.thread_id)
                .await
        );
        let state = self.turn.state.clone();
        let cid = self.turn.conversation_id;
        let message = self.turn.message.clone();
        self.llm.release.notify_one();
        self.turn
            .deliver_late(
                &mut self.events,
                Duration::from_secs(5),
                Duration::from_secs(1),
            )
            .await;
        let replies = self.replies.lock().unwrap().clone();
        assert_eq!(replies.len(), usize::from(expected.is_some()));
        if let Some(text) = expected {
            assert_eq!(replies[0].1.content, text);
            assert_eq!(replies[0].0.metadata, message.metadata);
            assert_eq!(replies[0].0.thread_id, message.thread_id);
        }
        let conversation = state
            .conversation_manager
            .get_conversation(cid)
            .await
            .unwrap();
        let entries: Vec<_> = conversation
            .entries
            .iter()
            .filter(|entry| matches!(entry.sender, lunarwing_engine::EntrySender::Agent { .. }))
            .map(|entry| entry.content.as_str())
            .collect();
        assert_eq!(entries, expected.into_iter().collect::<Vec<_>>());
        assert!(state.pending_gates.list_for_user("alice").await.is_empty());
        #[cfg(feature = "libsql")]
        {
            let db = state.db.as_ref().unwrap();
            let v1_id = resolve_v1_conversation_for_message(db.as_ref(), &message)
                .await
                .unwrap();
            let rows = db.list_conversation_messages(v1_id).await.unwrap();
            let assistant: Vec<_> = rows
                .iter()
                .filter(|row| row.role == "assistant")
                .map(|row| row.content.as_str())
                .collect();
            assert_eq!(assistant, expected.into_iter().collect::<Vec<_>>());
        }
    }
}

#[tokio::test]
async fn two_waiters_complete_one_run_and_record_one_response() {
    let rig = Rig::new(Some("raw answer"), None).await;
    rig.llm.release.notify_one();
    let first = rig.turn.join().await.unwrap().unwrap();
    let second = rig.turn.join().await.unwrap().unwrap();

    assert_eq!(
        rig.turn.complete_once(first).await.unwrap().as_deref(),
        Some("raw answer")
    );
    assert_eq!(
        rig.turn.complete_once(second).await.unwrap().as_deref(),
        Some("")
    );
    let conversation = rig
        .turn
        .state
        .conversation_manager
        .get_conversation(rig.turn.conversation_id)
        .await
        .unwrap();
    let responses: Vec<_> = conversation
        .entries
        .iter()
        .filter(|entry| matches!(entry.sender, lunarwing_engine::EntrySender::Agent { .. }))
        .map(|entry| entry.content.as_str())
        .collect();
    assert_eq!(responses, ["raw answer"]);
}

#[tokio::test]
async fn two_waiters_persist_only_one_gate_for_one_run() {
    let rig = Rig::new(Some("unused"), None).await;
    rig.llm.release.notify_one();
    rig.turn.join().await.unwrap().unwrap();
    rig.turn.join().await.unwrap().unwrap();
    let paused = ThreadOutcome::GatePaused {
        gate_name: "approval".into(),
        action_name: "shell".into(),
        call_id: "shared-call".into(),
        parameters: serde_json::json!({"command":"ls"}),
        resume_kind: lunarwing_engine::ResumeKind::Approval {
            allow_always: false,
        },
        resume_output: None,
    };

    rig.turn.complete_once(paused.clone()).await.unwrap();
    rig.turn.complete_once(paused).await.unwrap();
    assert_eq!(
        rig.turn
            .state
            .pending_gates
            .list_for_user("alice")
            .await
            .len(),
        1
    );
    assert_eq!(
        rig.statuses
            .lock()
            .unwrap()
            .iter()
            .filter(|status| matches!(status, StatusUpdate::ApprovalNeeded { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn foreground_gate_waiting_join_is_silent() {
    let rig = Rig::new(None, None).await;
    assert!(rig.turn.join().await.unwrap().is_none());
    assert_eq!(
        rig.turn
            .state
            .store
            .load_thread(rig.turn.thread_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        lunarwing_engine::ThreadState::Waiting
    );
}

#[tokio::test]
async fn gate_waiting_join_is_silent_and_preserves_the_thread() {
    let mut rig = Rig::new(None, None).await;
    let result = rig
        .turn
        .late_result(
            &mut rig.events,
            Duration::from_millis(1),
            Duration::from_millis(1),
        )
        .await
        .unwrap();
    assert!(result.is_none());
    assert!(rig.replies.lock().unwrap().is_empty());
    let saved = rig
        .turn
        .state
        .store
        .load_thread(rig.turn.thread_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.state, lunarwing_engine::ThreadState::Waiting);
}

#[tokio::test]
async fn late_response_transforms_history_and_preserves_reply_route() {
    Rig::new(Some("raw answer"), Some(Policy::Modify))
        .await
        .finish(Some("transformed: raw answer"))
        .await;
}

#[tokio::test]
async fn late_response_rejection_and_failure_suppress_both_histories_and_delivery() {
    for policy in [Policy::Reject, Policy::Fail] {
        Rig::new(Some("secret authentication_required text"), Some(policy))
            .await
            .finish(None)
            .await;
    }
    Rig::new(
        Some(r#"{"error":"authentication_required","credential_name":"gitlab"}"#),
        Some(Policy::Reject),
    )
    .await
    .finish(None)
    .await;
}

#[tokio::test]
async fn late_response_transform_can_replace_an_authentication_request() {
    Rig::new(
        Some(r#"{"error":"authentication_required","credential_name":"gitlab"}"#),
        Some(Policy::RemoveAuth),
    )
    .await
    .finish(Some("Use another method."))
    .await;
}

#[tokio::test]
async fn late_response_obeys_before_outbound_hook() {
    let mut rig = Rig::new(Some("raw answer"), Some(Policy::OutboundReject)).await;
    rig.llm.release.notify_one();
    rig.turn
        .deliver_late(
            &mut rig.events,
            Duration::from_secs(5),
            Duration::from_secs(1),
        )
        .await;
    assert!(rig.replies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn late_authentication_fallback_persists_a_resolvable_gate() {
    let mut rig = Rig::new(
        Some(r#"{"error":"authentication_required","credential_name":"gitlab"}"#),
        None,
    )
    .await;
    let state = rig.turn.state.clone();
    let message = rig.turn.message.clone();
    let cid = rig.turn.conversation_id;
    rig.llm.release.notify_one();
    rig.turn
        .deliver_late(
            &mut rig.events,
            Duration::from_secs(5),
            Duration::from_secs(1),
        )
        .await;
    assert!(rig.replies.lock().unwrap().is_empty());
    assert!(rig.statuses.lock().unwrap().iter().any(|s| matches!(s, StatusUpdate::AuthRequired { extension_name, .. } if extension_name == "gitlab")));
    let PendingGateResolution::Resolved(gate) = matching_engine_auth_gate(&state, &message).await
    else {
        panic!("auth fallback must be resolvable")
    };
    assert_eq!(gate.original_message.as_deref(), Some("original request"));
    let restored = crate::gate::store::PendingGateStore::new(Some(Arc::new(
        crate::gate::persistence::FileGatePersistence::new(rig._disk.path().join("gates.json")),
    )));
    assert_eq!(restored.restore_from_persistence().await.unwrap(), 1);
    restored
        .take_verified(&gate.key(), gate.request_id, "xmpp")
        .await
        .unwrap();
    let conversation = state
        .conversation_manager
        .get_conversation(cid)
        .await
        .unwrap();
    assert!(
        !conversation
            .entries
            .iter()
            .any(|entry| matches!(entry.sender, lunarwing_engine::EntrySender::Agent { .. }))
    );
}

async fn insert_shell_gate(rig: &Rig) -> PendingGate {
    let mut gate = sample_pending_gate(
        "alice",
        rig.turn.thread_id,
        lunarwing_engine::ResumeKind::Approval {
            allow_always: false,
        },
    );
    gate.conversation_id = rig.turn.conversation_id;
    gate.source_channel = "xmpp".into();
    rig.turn
        .state
        .pending_gates
        .insert(gate.clone())
        .await
        .unwrap(); // safety: test-only gate fixture
    gate
}

#[tokio::test]
async fn terminal_turns_reap_shell_gates_from_memory_and_persistence() {
    let outcomes = [
        ThreadOutcome::Completed {
            response: Some("done".into()),
        },
        ThreadOutcome::Failed {
            kind: lunarwing_engine::ThreadFailureKind::Orchestrator,
            error: "failed".into(),
        },
        ThreadOutcome::Stopped,
        ThreadOutcome::MaxIterations,
    ];
    for outcome in outcomes {
        let rig = Rig::new(None, None).await;
        let gate = insert_shell_gate(&rig).await;
        rig.turn.complete(outcome).await.unwrap();

        assert!(rig.turn.state.pending_gates.list_all().await.is_empty());
        let restored = crate::gate::store::PendingGateStore::new(Some(Arc::new(
            crate::gate::persistence::FileGatePersistence::new(rig._disk.path().join("gates.json")),
        )));
        assert_eq!(restored.restore_from_persistence().await.unwrap(), 0);
        assert!(
            rig.turn
                .state
                .pending_gates
                .peek(&gate.key())
                .await
                .is_none()
        );
    }
}

#[tokio::test]
async fn gate_paused_turn_keeps_existing_and_new_shell_gates_pending() {
    let rig = Rig::new(None, None).await;
    // A separate user key lets the new paused gate coexist with this sentinel,
    // which detects an accidental drain of every gate for the thread.
    let mut sentinel = sample_pending_gate(
        "bob",
        rig.turn.thread_id,
        lunarwing_engine::ResumeKind::Approval {
            allow_always: false,
        },
    );
    sentinel.conversation_id = rig.turn.conversation_id;
    sentinel.source_channel = "xmpp".into();
    rig.turn
        .state
        .pending_gates
        .insert(sentinel.clone())
        .await
        .unwrap();

    rig.turn
        .complete(ThreadOutcome::GatePaused {
            gate_name: "approval".into(),
            action_name: "shell".into(),
            call_id: "paused-call".into(),
            parameters: serde_json::json!({"command":"ls"}),
            resume_kind: lunarwing_engine::ResumeKind::Approval {
                allow_always: false,
            },
            resume_output: None,
        })
        .await
        .unwrap();

    assert_eq!(
        rig.turn
            .state
            .pending_gates
            .peek(&sentinel.key())
            .await
            .map(|gate| gate.request_id),
        Some(sentinel.request_id.to_string())
    );
    let sentinel_list = rig.turn.state.pending_gates.list_for_user("bob").await;
    assert_eq!(sentinel_list.len(), 1);
    assert_eq!(sentinel_list[0].request_id, sentinel.request_id);

    let parked = matching_engine_approval_gate(&rig.turn.state, &rig.turn.message, None)
        .await
        .unwrap();
    assert_eq!(
        rig.turn
            .state
            .pending_gates
            .peek(&parked.key())
            .await
            .map(|gate| gate.request_id),
        Some(parked.request_id.to_string())
    );
    assert!(
        rig.turn
            .state
            .pending_gates
            .list_for_user("alice")
            .await
            .iter()
            .any(|gate| gate.request_id == parked.request_id)
    );
}

#[tokio::test]
async fn terminal_turn_broadcasts_thread_terminated_gate_resolution() {
    use crate::channels::web::{GatewayChannel, types::SseEvent};
    use futures::{FutureExt, StreamExt};

    let mut rig = Rig::new(None, None).await;
    let gateway = GatewayChannel::new(
        crate::config::GatewayConfig {
            host: "127.0.0.1".into(),
            port: 0,
            auth_token: Some("test-token".into()),
            mascot_enabled: false,
            user_id: "alice".into(),
            workspace_read_scopes: vec![],
            memory_layers: vec![],
            user_tokens: None,
            ws_ping_interval_secs: 30,
            ws_idle_timeout_secs: 120,
        },
        "alice".into(),
    );
    let sse = Arc::clone(&gateway.state().sse);
    let mut events = sse.subscribe_raw(Some("alice".into())).unwrap();
    rig.turn.state.sse = Some(sse);
    let gate = insert_shell_gate(&rig).await;

    rig.turn.complete(ThreadOutcome::Stopped).await.unwrap();

    let mut resolutions = Vec::new();
    while let Some(Some(event)) = events.next().now_or_never() {
        if let SseEvent::GateResolved {
            request_id,
            resolution,
            thread_id,
            ..
        } = event
        {
            resolutions.push((request_id, resolution, thread_id));
        }
    }
    assert_eq!(
        resolutions,
        vec![(
            gate.request_id.to_string(),
            "thread_terminated".into(),
            Some(gate.thread_id.to_string()),
        )]
    );
}

#[tokio::test]
async fn late_approval_uses_durable_normal_gate_path() {
    let rig = Rig::new(None, None).await;
    let result = rig
        .turn
        .complete(ThreadOutcome::GatePaused {
            gate_name: "approval".into(),
            action_name: "shell".into(),
            call_id: "late-call".into(),
            parameters: serde_json::json!({"command":"ls"}),
            resume_kind: lunarwing_engine::ResumeKind::Approval {
                allow_always: false,
            },
            resume_output: None,
        })
        .await
        .unwrap();
    assert_eq!(result.as_deref(), Some(""));
    let gate = matching_engine_approval_gate(&rig.turn.state, &rig.turn.message, None)
        .await
        .unwrap();
    assert_eq!(gate.call_id, "late-call");
    assert!(
        rig.statuses
            .lock()
            .unwrap()
            .iter()
            .any(|s| matches!(s, StatusUpdate::ApprovalNeeded { .. }))
    );
    rig.turn
        .state
        .pending_gates
        .take_verified(&gate.key(), gate.request_id, "xmpp")
        .await
        .unwrap();
}

#[tokio::test]
async fn late_second_deadline_stops_a_real_blocked_model_call() {
    let mut rig = Rig::new(Some("never released"), None).await;
    let manager = Arc::clone(&rig.turn.state.thread_manager);
    let tid = rig.turn.thread_id;
    tokio::time::timeout(
        Duration::from_secs(5),
        rig.turn.deliver_late(
            &mut rig.events,
            Duration::from_millis(1),
            Duration::from_secs(2),
        ),
    )
    .await
    .expect("watcher must be bounded");
    assert!(!manager.is_running(tid).await);
    let replies = rig.replies.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert!(replies[0].1.content.contains("extended time limit"));
}

#[tokio::test]
async fn late_gateway_response_uses_real_channel_once_with_original_thread() {
    use crate::channels::web::{GatewayChannel, types::SseEvent};
    use futures::{FutureExt, StreamExt};
    let mut rig = Rig::new(Some("late gateway answer"), None).await;
    let gateway = GatewayChannel::new(
        crate::config::GatewayConfig {
            host: "127.0.0.1".into(),
            port: 0,
            auth_token: Some("test-token".into()),
            mascot_enabled: false,
            user_id: "alice".into(),
            workspace_read_scopes: vec![],
            memory_layers: vec![],
            user_tokens: None,
            ws_ping_interval_secs: 30,
            ws_idle_timeout_secs: 120,
        },
        "alice".into(),
    );
    let sse = Arc::clone(&gateway.state().sse);
    let mut events = sse.subscribe_raw(Some("alice".into())).unwrap();
    let channels = Arc::new(crate::channels::ChannelManager::new());
    channels.add(Box::new(gateway)).await;
    rig.turn.delivery.channels = channels;
    rig.turn.state.sse = Some(sse);
    rig.turn.message = IncomingMessage::new("gateway", "alice", "original request")
        .with_thread("client-conversation")
        .with_metadata(serde_json::json!({"user_id":"alice","thread_id":"client-conversation"}));
    rig.llm.release.notify_one();
    rig.turn
        .deliver_late(
            &mut rig.events,
            Duration::from_secs(5),
            Duration::from_secs(1),
        )
        .await;
    let mut replies = Vec::new();
    while let Some(Some(event)) = events.next().now_or_never() {
        if let SseEvent::Response {
            content, thread_id, ..
        } = event
        {
            replies.push((content, thread_id));
        }
    }
    assert_eq!(
        replies,
        vec![("late gateway answer".into(), "client-conversation".into())]
    );
}

// Keep the helper's test-only boundary explicit for per-file CI source checks.
#[cfg(test)]
impl Rig {
    /// Replaces the channels with a lifecycle probe named like the message's channel.
    async fn probe_lifecycle(&mut self) -> LifecycleLog {
        let channels = Arc::new(crate::channels::ChannelManager::new());
        let (probe, log) = LifecycleProbe::new(&self.turn.message.channel);
        channels.add(Box::new(probe)).await;
        self.turn.delivery.channels = channels;
        log
    }

    /// The outcome reported when the agent loop finishes the message with `computed`.
    async fn reported(&self, log: &LifecycleLog, computed: TurnOutcome) -> Option<TurnOutcome> {
        reported_finish(
            &self.turn.delivery.channels,
            log,
            &self.turn.message,
            computed,
        )
        .await
    }
}

#[cfg(test)]
fn test_gateway() -> crate::channels::web::GatewayChannel {
    crate::channels::web::GatewayChannel::new(
        crate::config::GatewayConfig {
            host: "127.0.0.1".into(),
            port: 0,
            auth_token: Some("test-token".into()),
            mascot_enabled: false,
            user_id: "alice".into(),
            workspace_read_scopes: vec![],
            memory_layers: vec![],
            user_tokens: None,
            ws_ping_interval_secs: 30,
            ws_idle_timeout_secs: 120,
        },
        "alice".into(),
    )
}

#[tokio::test]
async fn stopped_turn_reports_interrupted_hint() {
    let mut rig = Rig::new(None, None).await;
    let log = rig.probe_lifecycle().await;

    let result = rig.turn.complete(ThreadOutcome::Stopped).await;

    // The stopped sentinel reads as `no_reply`; the hint reports the interrupt.
    assert_eq!(result.ok().flatten().as_deref(), Some("")); // safety: test-only assertion
    let reported = rig.reported(&log, TurnOutcome::NoReply).await;
    assert_eq!(reported, Some(TurnOutcome::Interrupted)); // safety: test-only assertion
}

#[tokio::test]
async fn failed_turn_reports_failed_hint() {
    let failures = [
        ThreadOutcome::Failed {
            kind: lunarwing_engine::ThreadFailureKind::Orchestrator,
            error: "the model call failed".into(),
        },
        ThreadOutcome::MaxIterations,
    ];
    for outcome in failures {
        let mut rig = Rig::new(None, None).await;
        let log = rig.probe_lifecycle().await;

        let result = rig.turn.complete(outcome).await.ok().flatten();

        // The error text reads as a reply; the hint reports the failure.
        assert!(result.is_some_and(|text| !text.is_empty())); // safety: test-only assertion
        let reported = rig.reported(&log, TurnOutcome::Replied).await;
        assert_eq!(reported, Some(TurnOutcome::Failed)); // safety: test-only assertion
    }

    // A completed turn leaves no hint: its reply stays `replied`.
    let mut rig = Rig::new(None, None).await;
    let log = rig.probe_lifecycle().await;
    let completed = ThreadOutcome::Completed {
        response: Some("done".into()),
    };
    let result = rig.turn.complete(completed).await.ok().flatten();
    assert_eq!(result.as_deref(), Some("done")); // safety: test-only assertion
    let reported = rig.reported(&log, TurnOutcome::Replied).await;
    assert_eq!(reported, Some(TurnOutcome::Replied)); // safety: test-only assertion
}

#[tokio::test]
async fn detached_turn_reports_detached_then_late_finish() {
    use crate::channels::web::sse::{CorrelatedEvent, Correlation};
    use crate::channels::web::types::SseEvent;
    use futures::StreamExt;

    let mut rig = Rig::new(Some("late gateway answer"), None).await;
    let gateway = test_gateway();
    let sse = Arc::clone(&gateway.state().sse);
    let mut events = sse
        .subscribe_raw_correlated(Some("alice".into()))
        .expect("SSE subscription"); // safety: test-only fixture
    let channels = Arc::new(crate::channels::ChannelManager::new());
    channels.add(Box::new(gateway)).await;
    rig.turn.delivery.channels = Arc::clone(&channels);
    rig.turn.state.sse = Some(sse);
    let mut message = IncomingMessage::new("gateway", "alice", "original request")
        .with_thread("client-conversation");
    message.metadata = serde_json::json!({
        "user_id": "alice",
        "thread_id": "client-conversation",
        "message_id": message.id.to_string(),
        "client_turn_id": "c-0001",
    });
    rig.turn.message = message.clone();
    let turn = rig.turn;
    let thread_events = rig.events;

    // The model call is still blocked, so a 1 ms budget detaches the turn.
    let detached = turn
        .await_outcome(thread_events, Duration::from_millis(1))
        .await
        .ok()
        .flatten();
    assert_eq!(detached.as_deref(), Some("")); // safety: test-only assertion
    // The agent loop reads the empty sentinel as `no_reply`; the hint makes it
    // `detached`. Then the model answers and the late watcher finishes the turn.
    channels
        .message_lifecycle(&message, MessageLifecycle::Finished(TurnOutcome::NoReply))
        .await;
    rig.llm.release.notify_one();

    let mut seen: Vec<CorrelatedEvent> = Vec::new();
    let finished = tokio::time::timeout(Duration::from_secs(10), async {
        let finishes = |seen: &[CorrelatedEvent]| {
            seen.iter()
                .filter(|item| matches!(item.event, SseEvent::TurnFinished { .. }))
                .count()
        };
        while finishes(&seen) < 2 {
            match events.next().await {
                Some(item) => seen.push(item),
                None => break,
            }
        }
    })
    .await;
    assert!(finished.is_ok(), "late finish missing: {seen:?}"); // safety: test-only assertion

    let finishes: Vec<(usize, &str, TurnOutcome)> = seen
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match &item.event {
            SseEvent::TurnFinished { thread_id, outcome } => {
                Some((index, thread_id.as_str(), *outcome))
            }
            _ => None,
        })
        .collect();
    let outcomes: Vec<_> = finishes
        .iter()
        .map(|(_, thread, outcome)| (*thread, *outcome))
        .collect();
    let expected_outcomes = [
        ("client-conversation", TurnOutcome::Detached),
        ("client-conversation", TurnOutcome::Replied),
    ];
    assert_eq!(outcomes, expected_outcomes); // safety: test-only assertion
    let is_late_reply = |item: &CorrelatedEvent| match &item.event {
        SseEvent::Response { content, .. } => content == "late gateway answer",
        _ => false,
    };
    let replies: Vec<usize> = seen
        .iter()
        .enumerate()
        .filter(|(_, item)| is_late_reply(item))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(replies.len(), 1, "{seen:?}"); // safety: test-only assertion
    let (detach, late, reply) = (finishes[0].0, finishes[1].0, replies[0]);
    // The late reply is delivered before the late finish, never before the detach.
    assert!(detach < reply && reply < late, "{seen:?}"); // safety: test-only assertion
    // Both finishes and the reply belong to the original turn.
    let expected = Correlation::from_metadata(&message.metadata);
    assert!(expected.is_some()); // safety: test-only assertion
    for index in [detach, reply, late] {
        assert_eq!(seen[index].correlation, expected, "{:?}", seen[index].event); // safety: test-only assertion
    }
}

#[cfg(feature = "libsql")]
#[tokio::test]
async fn recovery_history_reference_survives_gate_resume_and_is_user_scoped() {
    use lunarwing_engine::types::event::ModelRecoveryState;
    use lunarwing_engine::{EventKind, ModelFailureKind, ThreadEvent};

    let mut rig = Rig::new(None, None).await;
    let db = rig.turn.state.db.as_ref().unwrap().clone();
    let cid = resolve_v1_conversation_for_message(db.as_ref(), &rig.turn.message)
        .await
        .unwrap();
    let user_message_id = db
        .add_conversation_message(cid, "user", "delegate once")
        .await
        .unwrap();
    let started_at = rig.turn.message.received_at;
    db.update_conversation_metadata_field(
        cid,
        &turn_history_metadata_key(rig.turn.thread_id),
        &serde_json::json!({
            "thread_id": rig.turn.thread_id.to_string(), "user_message_id": user_message_id,
            "started_at": started_at,
        }),
    )
    .await
    .unwrap();
    let mut callback = rig.turn.message.clone();
    callback.id = uuid::Uuid::new_v4();
    callback.received_at = started_at + chrono::TimeDelta::minutes(1);
    let reference = history_reference(&rig.turn.state, &callback, rig.turn.thread_id)
        .await
        .unwrap();
    assert_eq!(reference.user_message_id, user_message_id);
    assert_eq!(reference.started_at, started_at);
    assert!(
        history_reference(
            &rig.turn.state,
            &callback,
            lunarwing_engine::ThreadId::new()
        )
        .await
        .is_none()
    );
    let mut foreign = callback.clone();
    foreign.user_id = "bob".into();
    assert!(
        history_reference(&rig.turn.state, &foreign, rig.turn.thread_id)
            .await
            .is_none()
    );

    let mut thread = rig
        .turn
        .state
        .store
        .load_thread(rig.turn.thread_id)
        .await
        .unwrap()
        .unwrap();
    let mut event = ThreadEvent::new(
        thread.id,
        EventKind::ModelRecovery {
            failure_kind: ModelFailureKind::ReplArgumentCorruption,
            recovery_state: ModelRecoveryState::Recovered,
            attempt: 1,
        },
    );
    event.timestamp = started_at + chrono::TimeDelta::seconds(1);
    thread.events.push(event);
    rig.turn.state.store.save_thread(&thread).await.unwrap();
    rig.turn.message = callback;
    rig.turn.history = Some(reference);
    rig.turn
        .write_recovery_outcome(&ThreadOutcome::Completed {
            response: Some("job id".into()),
        })
        .await;
    let messages = db.list_conversation_messages(cid).await.unwrap();
    let turns = crate::channels::web::util::build_turns_from_db_messages(&messages);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].state, "Completed");
    assert_eq!(
        turns[0].recovery.as_ref().unwrap().failure_kind,
        "repl_argument_corruption"
    );
    assert_eq!(
        turns[0].recovery.as_ref().unwrap().recovery_state,
        "recovered"
    );
}

/// WI#517: two overlapping turns on one conversation write distinct metadata
/// keys, so the later turn cannot overwrite the earlier turn's reference.
#[cfg(feature = "libsql")]
#[tokio::test]
async fn overlapping_turns_keep_separate_history_references() {
    let rig = Rig::new(None, None).await;
    let db = rig.turn.state.db.as_ref().unwrap().clone();
    let cid = resolve_v1_conversation_for_message(db.as_ref(), &rig.turn.message)
        .await
        .unwrap();
    let first_id = db
        .add_conversation_message(cid, "user", "first turn")
        .await
        .unwrap();
    let second_id = db
        .add_conversation_message(cid, "user", "second turn")
        .await
        .unwrap();
    let thread_a = lunarwing_engine::ThreadId::new();
    let thread_b = lunarwing_engine::ThreadId::new();
    let started_a = rig.turn.message.received_at;
    let started_b = started_a + chrono::TimeDelta::seconds(3);
    for (thread_id, user_message_id, started_at) in [
        (thread_a, first_id, started_a),
        (thread_b, second_id, started_b),
    ] {
        db.update_conversation_metadata_field(
            cid,
            &turn_history_metadata_key(thread_id),
            &serde_json::json!({
                "thread_id": thread_id.to_string(), "user_message_id": user_message_id,
                "started_at": started_at,
            }),
        )
        .await
        .unwrap();
    }

    let reference_a = history_reference(&rig.turn.state, &rig.turn.message, thread_a)
        .await
        .unwrap();
    assert_eq!(reference_a.user_message_id, first_id);
    assert_eq!(reference_a.started_at, started_a);
    let reference_b = history_reference(&rig.turn.state, &rig.turn.message, thread_b)
        .await
        .unwrap();
    assert_eq!(reference_b.user_message_id, second_id);
    assert_eq!(reference_b.started_at, started_b);

    let metadata = db.get_conversation_metadata(cid).await.unwrap().unwrap();
    assert!(
        metadata.get("engine_turn_history").is_none(),
        "overlapping turns must not share the legacy single slot"
    );
}

/// A turn that started before the per-thread key existed still resolves its
/// reference from the legacy single slot, guarded by the thread match.
#[cfg(feature = "libsql")]
#[tokio::test]
async fn legacy_history_slot_resolves_only_for_its_thread() {
    let rig = Rig::new(None, None).await;
    let db = rig.turn.state.db.as_ref().unwrap().clone();
    let cid = resolve_v1_conversation_for_message(db.as_ref(), &rig.turn.message)
        .await
        .unwrap();
    let user_message_id = db
        .add_conversation_message(cid, "user", "pre-upgrade turn")
        .await
        .unwrap();
    db.update_conversation_metadata_field(
        cid,
        "engine_turn_history",
        &serde_json::json!({
            "thread_id": rig.turn.thread_id.to_string(), "user_message_id": user_message_id,
            "started_at": rig.turn.message.received_at,
        }),
    )
    .await
    .unwrap();

    let reference = history_reference(&rig.turn.state, &rig.turn.message, rig.turn.thread_id)
        .await
        .unwrap();
    assert_eq!(reference.user_message_id, user_message_id);
    assert!(
        history_reference(
            &rig.turn.state,
            &rig.turn.message,
            lunarwing_engine::ThreadId::new()
        )
        .await
        .is_none(),
        "the legacy slot must not serve a different thread"
    );
}
