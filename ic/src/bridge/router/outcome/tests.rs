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
    tests::{TestStore, make_expected_test_state, save_waiting_thread},
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
