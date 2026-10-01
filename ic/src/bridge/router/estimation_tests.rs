//! WI191: real completions → approval → live delivery → disk recovery.

use super::tests::{TestStore, make_expected_test_state};
use super::*;
use crate::context::JobContext;
use crate::gate::pending::PendingGateView;
use crate::gate::persistence::FileGatePersistence;
use crate::gate::store::PendingGateStore;
use crate::tools::{Tool, ToolError, ToolOutput};
use futures::StreamExt;
use lunarwing_engine::{EffectExecutor, EngineError};
use serde_json::{Value, json};
use std::time::Duration;

struct TimedApprovalTool;

#[async_trait::async_trait]
impl Tool for TimedApprovalTool {
    fn name(&self) -> &str {
        "timed_approval"
    }
    fn description(&self) -> &str {
        "A timed action"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object"})
    }
    fn estimated_duration(&self, _: &Value) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
    fn sensitive_params(&self) -> &[&str] {
        &["token"]
    }
    fn requires_approval(&self, _: &Value) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Always
    }
    async fn execute(&self, params: Value, _: &JobContext) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(Duration::from_secs(params["seconds"].as_u64().unwrap_or(2))).await;
        Ok(ToolOutput::success(json!({"ok":true}), Duration::ZERO))
    }
}

#[tokio::test(start_paused = true)]
async fn learned_approval_hint_is_identical_in_live_and_recovered_views() {
    let manager = Arc::new(crate::channels::ChannelManager::new());
    let (stub, _tx) = crate::testing::StubChannel::new("xmpp");
    let statuses = stub.captured_statuses_handle();
    manager.add(Box::new(stub)).await;
    let params = json!({"token":"must-not-leak"});
    let mut state = warmed_state(&params).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pending-gates.json");
    state.pending_gates = Arc::new(PendingGateStore::new(Some(Arc::new(
        FileGatePersistence::new(path.clone()),
    ))));
    let sse = Arc::new(SseManager::new());
    let mut events = sse.subscribe_raw(Some("alice".into())).unwrap();
    state.sse = Some(sse);
    let tools = state.effect_adapter.tools();
    let thread_id = lunarwing_engine::ThreadId::new();
    let message = IncomingMessage::new("xmpp", "alice", "run it");
    let outcome = pause_action(&state, thread_id, params.clone()).await;
    persist_and_notify_thread_gate(
        &manager,
        &state,
        &message,
        lunarwing_engine::ConversationId::new(),
        thread_id,
        outcome,
    )
    .await
    .unwrap();
    let original = state
        .pending_gates
        .list_for_user("alice")
        .await
        .pop()
        .unwrap();
    assert!(original.description.contains("~2.00s"));
    assert_eq!(
        original
            .description
            .matches("Typical tool runtime:")
            .count(),
        1
    );
    assert_eq!(
        tools
            .estimates()
            .estimate("alice", &TimedApprovalTool, &params)
            .unwrap()
            .samples,
        5
    );
    let event = events.next().await.unwrap();
    match event {
        crate::channels::web::types::SseEvent::GateRequired {
            description,
            parameters,
            ..
        } => {
            assert_eq!(description, original.description);
            assert!(!parameters.contains("must-not-leak"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
    {
        let captured = statuses.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(
            matches!(&captured[0], StatusUpdate::ApprovalNeeded { description, .. } if description == &original.description)
        );
    }
    // A long gate wait and a later slower completion cannot rewrite its hint.
    tokio::time::advance(Duration::from_secs(60)).await;
    run_action(&state, thread_id, json!({"seconds":4}), true)
        .await
        .unwrap();
    let next = tools
        .estimates()
        .estimate("alice", &TimedApprovalTool, &params)
        .unwrap();
    assert_eq!(next.samples, 6);
    assert!((next.duration.as_secs_f64() - 2.2).abs() < 0.001);
    assert_recovered_hint(path, &original).await;
    assert_auth_has_no_hint(&manager, &state, &message, original).await;
}

// Keep helper test context explicit for the per-file panic-policy scanner.
#[cfg(test)]
async fn warmed_state(params: &Value) -> EngineState {
    let mut state = make_expected_test_state(Arc::new(TestStore::new()));
    state.effect_adapter = Arc::new(EffectBridgeAdapter::new(
        Arc::new(
            crate::tools::ToolRegistry::new()
                .with_estimation_mode(crate::estimation::ToolEstimationMode::Display),
        ),
        Arc::new(crate::safety::SafetyLayer::new(
            &crate::config::SafetyConfig {
                max_output_length: 100_000,
                injection_check_enabled: false,
            },
        )),
        Arc::new(crate::hooks::HookRegistry::default()),
    ));
    state
        .effect_adapter
        .tools()
        .register(Arc::new(TimedApprovalTool))
        .await;
    for _ in 0..5 {
        let result = run_action(
            &state,
            lunarwing_engine::ThreadId::new(),
            params.clone(),
            true,
        )
        .await
        .unwrap();
        assert!(!result.is_error);
    }
    state
}

async fn run_action(
    state: &EngineState,
    thread_id: lunarwing_engine::ThreadId,
    params: Value,
    approved: bool,
) -> Result<lunarwing_engine::ActionResult, EngineError> {
    let lease = lunarwing_engine::CapabilityLease {
        id: lunarwing_engine::types::capability::LeaseId::new(),
        thread_id,
        capability_name: "tools".into(),
        granted_actions: lunarwing_engine::GrantedActions::All,
        granted_at: chrono::Utc::now(),
        expires_at: None,
        max_uses: None,
        uses_remaining: None,
        revoked: false,
        revoked_reason: None,
    };
    let context = lunarwing_engine::ThreadExecutionContext {
        thread_id,
        thread_type: lunarwing_engine::types::thread::ThreadType::Foreground,
        project_id: lunarwing_engine::ProjectId::new(),
        user_id: "alice".into(),
        step_id: lunarwing_engine::StepId::new(),
        current_call_id: Some("call-1".into()),
        source_channel: Some("xmpp".into()),
        supervised_mode: false,
        shared_conversation: false,
        supervised_timeout_secs: 300,
        prompt_context: Arc::from([]),
    };
    if approved {
        state
            .effect_adapter
            .execute_resolved_pending_action("timed_approval", params, &lease, &context, true)
            .await
    } else {
        state
            .effect_adapter
            .execute_action("timed_approval", params, &lease, &context)
            .await
    }
}

async fn pause_action(
    state: &EngineState,
    thread: lunarwing_engine::ThreadId,
    params: Value,
) -> ThreadOutcome {
    match run_action(state, thread, params, false).await.unwrap_err() {
        EngineError::GatePaused {
            gate_name,
            action_name,
            call_id,
            parameters,
            resume_kind,
            resume_output,
        } => ThreadOutcome::GatePaused {
            gate_name,
            action_name,
            call_id,
            parameters: *parameters,
            resume_kind: *resume_kind,
            resume_output: resume_output.map(|output| *output),
        },
        error => panic!("expected approval pause, got {error:?}"),
    }
}

#[cfg(test)]
async fn assert_recovered_hint(path: std::path::PathBuf, original: &PendingGate) {
    let recovered = PendingGateStore::new(Some(Arc::new(FileGatePersistence::new(path))));
    recovered.restore_from_persistence().await.unwrap();
    let gates = recovered.list_for_user("alice").await;
    let view = PendingGateView::from(&gates[0]);
    assert_eq!(view.description, original.description);
    assert!(
        !serde_json::to_string(&view)
            .unwrap()
            .contains("must-not-leak")
    );
    assert_eq!(gates[0].expires_at, original.expires_at);
    assert_eq!(gates[0].request_id, original.request_id);
    assert!(recovered.list_for_user("bob").await.is_empty());

    let mut legacy = original.clone();
    legacy.display_parameters = None;
    assert!(gate_display_parameters(&legacy).is_null());
    let view = PendingGateView::from(&legacy);
    assert_eq!(view.description, original.description);
    assert_eq!(view.parameters, "null");
}

#[cfg(test)]
async fn assert_auth_has_no_hint(
    manager: &Arc<crate::channels::ChannelManager>,
    state: &EngineState,
    message: &IncomingMessage,
    mut auth: PendingGate,
) {
    // Auth pauses for the same warmed tool must not acquire a tool ETA.
    auth.thread_id = lunarwing_engine::ThreadId::new();
    auth.request_id = uuid::Uuid::new_v4();
    auth.gate_name = "authentication".into();
    auth.description = "Provide a credential".into();
    auth.resume_kind = lunarwing_engine::ResumeKind::Authentication {
        credential_name: "token".into(),
        instructions: "Provide a credential".into(),
        auth_url: None,
    };
    insert_and_notify_pending_gate(manager, state, message, auth.clone())
        .await
        .unwrap();
    let stored = state.pending_gates.peek(&auth.key()).await.unwrap();
    assert_eq!(stored.description, "Provide a credential");
}
