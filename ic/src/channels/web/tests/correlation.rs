//! Gateway correlation, lifecycle events and the golden SSE contract (TUI design 4.2, 8.3).
//!
//! Every helper function carries `#[cfg(test)]`: the panic-policy checker only counts
//! item-level test attributes in separate test files.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use axum::response::IntoResponse;
use futures::Stream;
use serde_json::json;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::channels::web::GatewayChannel;
use crate::channels::web::server::gateway_message_metadata;
use crate::channels::web::sse::{CorrelatedEvent, Correlation, SseManager};
use crate::channels::web::status_sse_event;
use crate::channels::web::types::SseEvent;
use crate::channels::{
    Channel, ChannelManager, IncomingMessage, MessageLifecycle, OutgoingResponse, StatusUpdate,
    ToolDecision, TurnOutcome,
};
use crate::config::GatewayConfig;

const USER: &str = "fixture-user";
const THREAD: &str = "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10";
const TURN: &str = "0193f2a1-7c3e-4d4a-b1e2-6f0c9a8b7d6e";
/// A 1x1 PNG.
const TINY_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

#[cfg(test)]
fn gateway() -> GatewayChannel {
    let config = GatewayConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        auth_token: Some("correlation-test-token".to_string()),
        mascot_enabled: false,
        user_id: USER.to_string(),
        workspace_read_scopes: Vec::new(),
        memory_layers: Vec::new(),
        user_tokens: None,
        ws_ping_interval_secs: 30,
        ws_idle_timeout_secs: 120,
    };
    GatewayChannel::new(config, USER.to_string())
}

/// A message as `POST /api/chat/send` queues it, with a chosen id.
#[cfg(test)]
fn chat_message(id: Uuid, client_turn_id: Option<&str>) -> IncomingMessage {
    let mut msg = IncomingMessage::new("gateway", USER, "hi").with_thread(THREAD);
    msg.id = id;
    msg.with_metadata(gateway_message_metadata(USER, THREAD, id, client_turn_id))
}

#[cfg(test)]
fn correlation(id: Uuid, client_turn_id: Option<&str>) -> Option<Correlation> {
    Some(Correlation {
        message_id: id.to_string(),
        client_turn_id: client_turn_id.map(str::to_string),
    })
}

#[cfg(test)]
fn subscribe(sse: &SseManager) -> impl Stream<Item = CorrelatedEvent> + Unpin + use<> {
    Box::pin(
        sse.subscribe_raw_correlated(Some(USER.to_string()))
            .expect("subscribe"),
    )
}

#[cfg(test)]
async fn next_event(events: &mut (impl Stream<Item = CorrelatedEvent> + Unpin)) -> CorrelatedEvent {
    tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("an event in time")
        .expect("stream open")
}

#[cfg(test)]
fn outcome_of(event: &SseEvent) -> Option<TurnOutcome> {
    match event {
        SseEvent::TurnFinished { outcome, .. } => Some(*outcome),
        _ => None,
    }
}

#[tokio::test]
async fn send_status_carries_correlation_from_metadata() {
    let gateway = gateway();
    let mut events = subscribe(&gateway.state().sse);
    let id = Uuid::new_v4();
    let msg = chat_message(id, Some(TURN));

    gateway
        .send_status(
            StatusUpdate::Thinking("Processing...".into()),
            &msg.metadata,
        )
        .await
        .expect("status");
    let event = next_event(&mut events).await;
    assert!(
        matches!(&event.event, SseEvent::Thinking { thread_id, .. } if thread_id.as_deref() == Some(THREAD))
    );
    assert_eq!(event.correlation, correlation(id, Some(TURN)));

    // Engine V2 status metadata is the message metadata with the engine thread id.
    let mut engine_metadata = msg.metadata.clone();
    engine_metadata["thread_id"] = json!("engine-thread");
    gateway
        .send_status(
            StatusUpdate::ToolStarted {
                name: "http".into(),
            },
            &engine_metadata,
        )
        .await
        .expect("status");
    let event = next_event(&mut events).await;
    assert!(
        matches!(&event.event, SseEvent::ToolStarted { thread_id, .. } if thread_id.as_deref() == Some("engine-thread"))
    );
    assert_eq!(event.correlation, correlation(id, Some(TURN)));

    // Internal metadata without ids stays uncorrelated.
    gateway
        .send_status(
            StatusUpdate::Status("Done".into()),
            &json!({"user_id": USER, "thread_id": THREAD}),
        )
        .await
        .expect("status");
    assert_eq!(next_event(&mut events).await.correlation, None);
}

#[tokio::test]
async fn respond_carries_correlation_from_incoming_metadata() {
    let gateway = gateway();
    let mut events = subscribe(&gateway.state().sse);
    let id = Uuid::new_v4();

    gateway
        .respond(
            &chat_message(id, Some(TURN)),
            OutgoingResponse::text("hello"),
        )
        .await
        .expect("respond");
    let event = next_event(&mut events).await;
    assert!(
        matches!(&event.event, SseEvent::Response { content, thread_id, .. } if content == "hello" && thread_id == THREAD)
    );
    assert_eq!(event.correlation, correlation(id, Some(TURN)));

    // Without a client id the reply still names its message.
    gateway
        .respond(&chat_message(id, None), OutgoingResponse::text("hello"))
        .await
        .expect("respond");
    assert_eq!(
        next_event(&mut events).await.correlation,
        correlation(id, None)
    );

    // Proactive messages and replies to internal messages are out-of-band.
    gateway
        .broadcast(USER, OutgoingResponse::text("reminder").in_thread(THREAD))
        .await
        .expect("broadcast");
    assert_eq!(next_event(&mut events).await.correlation, None);
    let internal = IncomingMessage::new("gateway", USER, "job done").with_thread(THREAD);
    gateway
        .respond(&internal, OutgoingResponse::text("noted"))
        .await
        .expect("respond");
    assert_eq!(next_event(&mut events).await.correlation, None);
}

#[tokio::test]
async fn turn_finished_follows_correlated_response() {
    let gateway = gateway();
    let sse = Arc::clone(&gateway.state().sse);
    let mut events = subscribe(&sse);
    let manager = ChannelManager::new();
    manager.add(Box::new(gateway)).await;

    let id = Uuid::new_v4();
    let msg = chat_message(id, Some(TURN));
    manager
        .message_lifecycle(&msg, MessageLifecycle::Started)
        .await;
    manager
        .respond(&msg, OutgoingResponse::text("done"))
        .await
        .expect("respond");
    manager
        .message_lifecycle(&msg, MessageLifecycle::Finished(TurnOutcome::Replied))
        .await;

    let started = next_event(&mut events).await;
    assert!(matches!(&started.event, SseEvent::TurnStarted { thread_id } if thread_id == THREAD));
    let reply = next_event(&mut events).await;
    assert!(matches!(reply.event, SseEvent::Response { .. }));
    let finished = next_event(&mut events).await;
    assert_eq!(outcome_of(&finished.event), Some(TurnOutcome::Replied));
    for event in [&started, &reply, &finished] {
        assert_eq!(event.correlation, correlation(id, Some(TURN)));
    }

    // An outcome hint (an approval pause) refines the reported outcome.
    let paused = chat_message(Uuid::new_v4(), Some(TURN));
    manager.note_turn_outcome(paused.id, TurnOutcome::Paused);
    manager
        .message_lifecycle(&paused, MessageLifecycle::Finished(TurnOutcome::NoReply))
        .await;
    let finished = next_event(&mut events).await;
    assert_eq!(outcome_of(&finished.event), Some(TurnOutcome::Paused));
    assert_eq!(finished.correlation, correlation(paused.id, Some(TURN)));
}

#[tokio::test]
async fn lifecycle_not_emitted_without_message_id() {
    let gateway = gateway();
    let mut events = subscribe(&gateway.state().sse);

    // An internal message (job monitor, routine): no metadata.
    let internal = IncomingMessage::new("gateway", USER, "job done").with_thread(THREAD);
    // A WebSocket send or an older producer: user and thread, but no message id.
    let unstamped = IncomingMessage::new("gateway", USER, "hi")
        .with_thread(THREAD)
        .with_metadata(json!({"user_id": USER, "thread_id": THREAD}));
    // Stamped, but without a thread there is nothing to attribute the turn to.
    let mut threadless = chat_message(Uuid::new_v4(), Some(TURN));
    threadless.thread_id = None;
    for msg in [&internal, &unstamped, &threadless] {
        gateway
            .message_lifecycle(msg, MessageLifecycle::Started)
            .await;
        gateway
            .message_lifecycle(msg, MessageLifecycle::Finished(TurnOutcome::Replied))
            .await;
    }

    // The next event is the marker: nothing was broadcast before it.
    gateway.state().sse.broadcast(SseEvent::Heartbeat);
    assert!(matches!(
        next_event(&mut events).await.event,
        SseEvent::Heartbeat
    ));
}

// ---- status_sse_event (extracted from GatewayChannel::send_status) ----

/// Every `StatusUpdate` variant; adding one fails to compile here until it has a case.
#[cfg(test)]
fn status_variant(status: &StatusUpdate) -> &'static str {
    match status {
        StatusUpdate::Thinking(_) => "Thinking",
        StatusUpdate::ToolStarted { .. } => "ToolStarted",
        StatusUpdate::ToolCompleted { .. } => "ToolCompleted",
        StatusUpdate::ToolResult { .. } => "ToolResult",
        StatusUpdate::StreamChunk(_) => "StreamChunk",
        StatusUpdate::Status(_) => "Status",
        StatusUpdate::ExternalWaiting { .. } => "ExternalWaiting",
        StatusUpdate::JobStarted { .. } => "JobStarted",
        StatusUpdate::ApprovalNeeded { .. } => "ApprovalNeeded",
        StatusUpdate::AuthRequired { .. } => "AuthRequired",
        StatusUpdate::AuthCompleted { .. } => "AuthCompleted",
        StatusUpdate::ImageGenerated { .. } => "ImageGenerated",
        StatusUpdate::Suggestions { .. } => "Suggestions",
        StatusUpdate::SkillActivated { .. } => "SkillActivated",
        StatusUpdate::ReasoningUpdate { .. } => "ReasoningUpdate",
        StatusUpdate::ReasoningStream(_) => "ReasoningStream",
        StatusUpdate::TurnCost { .. } => "TurnCost",
    }
}

/// One case per variant with the JSON the inline `send_status` mapping produced.
#[cfg(test)]
fn status_mapping_cases() -> Vec<(StatusUpdate, serde_json::Value)> {
    let mut cases = progress_status_cases();
    cases.extend(prompt_and_summary_status_cases());
    cases
}

/// The progress statuses of [`status_mapping_cases`].
#[cfg(test)]
fn progress_status_cases() -> Vec<(StatusUpdate, serde_json::Value)> {
    vec![
        (
            StatusUpdate::Thinking("Processing...".into()),
            json!({"type": "thinking", "message": "Processing...", "thread_id": "t"}),
        ),
        (
            StatusUpdate::ToolStarted {
                name: "http".into(),
            },
            json!({"type": "tool_started", "name": "http", "thread_id": "t"}),
        ),
        (
            StatusUpdate::ToolCompleted {
                name: "shell".into(),
                success: false,
                error: Some("exit 1".into()),
                parameters: Some("{\"cmd\": \"[REDACTED]\"}".into()),
            },
            json!({"type": "tool_completed", "name": "shell", "success": false, "error": "exit 1",
                   "parameters": "{\"cmd\": \"[REDACTED]\"}", "thread_id": "t"}),
        ),
        (
            StatusUpdate::ToolResult {
                name: "http".into(),
                preview: "200 OK".into(),
            },
            json!({"type": "tool_result", "name": "http", "preview": "200 OK", "thread_id": "t"}),
        ),
        (
            StatusUpdate::StreamChunk("Let me".into()),
            json!({"type": "stream_chunk", "content": "Let me", "thread_id": "t"}),
        ),
        (
            StatusUpdate::Status("Done".into()),
            json!({"type": "status", "message": "Done", "thread_id": "t"}),
        ),
        (
            StatusUpdate::ExternalWaiting {
                gate_name: "deploy".into(),
            },
            json!({"type": "status", "message": "Waiting for external confirmation (gate: deploy)...",
                   "thread_id": "t"}),
        ),
    ]
}

/// The job, prompt, image and summary statuses of [`status_mapping_cases`].
#[cfg(test)]
fn prompt_and_summary_status_cases() -> Vec<(StatusUpdate, serde_json::Value)> {
    vec![
        (
            StatusUpdate::JobStarted {
                job_id: "j1".into(),
                title: "Build".into(),
                browse_url: "/jobs/j1".into(),
            },
            json!({"type": "job_started", "job_id": "j1", "title": "Build", "browse_url": "/jobs/j1"}),
        ),
        (
            StatusUpdate::ApprovalNeeded {
                request_id: "r1".into(),
                tool_name: "shell".into(),
                description: "Run a command".into(),
                parameters: json!({"command": "ls"}),
                allow_always: true,
            },
            json!({"type": "approval_needed", "request_id": "r1", "tool_name": "shell",
                   "description": "Run a command", "parameters": "{\n  \"command\": \"ls\"\n}",
                   "thread_id": "t", "allow_always": true}),
        ),
        (
            StatusUpdate::AuthRequired {
                extension_name: "notion".into(),
                instructions: Some("Paste a token".into()),
                auth_url: None,
                setup_url: Some("https://www.notion.so/my-integrations".into()),
            },
            json!({"type": "auth_required", "extension_name": "notion", "instructions": "Paste a token",
                   "setup_url": "https://www.notion.so/my-integrations"}),
        ),
        (
            StatusUpdate::AuthCompleted {
                extension_name: "notion".into(),
                success: true,
                message: "notion authenticated".into(),
            },
            json!({"type": "auth_completed", "extension_name": "notion", "success": true,
                   "message": "notion authenticated"}),
        ),
        (
            StatusUpdate::ImageGenerated {
                data_url: "data:image/png;base64,AAAA".into(),
                path: Some("generated/a.png".into()),
            },
            json!({"type": "image_generated", "data_url": "data:image/png;base64,AAAA",
                   "path": "generated/a.png", "thread_id": "t"}),
        ),
        (
            StatusUpdate::Suggestions {
                suggestions: vec!["Tell me more".into()],
            },
            json!({"type": "suggestions", "suggestions": ["Tell me more"], "thread_id": "t"}),
        ),
        (
            StatusUpdate::SkillActivated {
                skill_names: vec!["git".into(), "rust".into()],
            },
            json!({"type": "status", "message": "Skills activated: git, rust", "thread_id": "t"}),
        ),
        (
            StatusUpdate::ReasoningUpdate {
                narrative: "Selected 1 tool.".into(),
                decisions: vec![ToolDecision {
                    tool_name: "http".into(),
                    rationale: "Fetch the page.".into(),
                }],
            },
            json!({"type": "reasoning_update", "narrative": "Selected 1 tool.",
                   "decisions": [{"tool_name": "http", "rationale": "Fetch the page."}], "thread_id": "t"}),
        ),
        (
            StatusUpdate::ReasoningStream("bounded live reasoning".into()),
            json!({"type": "reasoning_update", "narrative": "bounded live reasoning",
                   "decisions": [], "thread_id": "t"}),
        ),
        (
            StatusUpdate::TurnCost {
                input_tokens: 10,
                output_tokens: 5,
                cost_usd: "0.0010".into(),
            },
            json!({"type": "turn_cost", "input_tokens": 10, "output_tokens": 5, "cost_usd": "0.0010",
                   "thread_id": "t"}),
        ),
    ]
}

#[test]
fn status_sse_event_matches_previous_inline_mapping() {
    let mut covered = BTreeSet::new();
    for (status, expected) in status_mapping_cases() {
        let variant = status_variant(&status);
        assert!(covered.insert(variant), "{variant} listed twice");
        let event = status_sse_event(status, Some("t".to_string()));
        assert_eq!(
            serde_json::to_value(&event).expect("JSON"),
            expected,
            "{variant}"
        );
    }
    assert_eq!(covered.len(), 17, "one case per StatusUpdate variant");

    // Without a thread id, thread-scoped events simply omit the field.
    let event = status_sse_event(StatusUpdate::Thinking("x".into()), None);
    assert_eq!(
        serde_json::to_value(&event).expect("JSON"),
        json!({"type": "thinking", "message": "x"})
    );
}

// ---- Golden contract: every SSE event as the TUI receives it ----

/// Path of the golden file the TypeScript client is checked against.
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/channels/web/tests/fixtures/sse_current.ndjson"
);
/// `LW_REGEN_FIXTURES=1` rewrites the golden file instead of comparing.
const REGEN_ENV: &str = "LW_REGEN_FIXTURES";
/// `SseEvent` variants; [`sse_variant`] fails to compile when one is added.
const SSE_VARIANTS: usize = 32;

const FIRST_MESSAGE: Uuid = Uuid::from_u128(0x0b8a_6c1e_5d2f_4c3b_9a1e_7f6d_5c4b_3a29);
const PAUSED_MESSAGE: Uuid = Uuid::from_u128(0x6f1d_2c3b_4a59_4e8d_8c7b_6a5f_4e3d_2c1b);
const APPROVAL_MESSAGE: Uuid = Uuid::from_u128(0x9c8b_7a69_5847_4362_9150_4f3e_2d1c_0b0a);
const SECOND_TURN: &str = "4a7c9e1b-2d3f-4b5a-8c6d-7e8f9a0b1c2d";
const GATE_REQUEST: &str = "6c0f2a8e-1f9b-4d3e-a1c2-7b5e9d0f4a21";

/// Every `SseEvent` variant by wire name; adding a variant fails to compile here until
/// the fixture builder emits a sample of it and [`SSE_VARIANTS`] counts it.
#[cfg(test)]
fn sse_variant(event: &SseEvent) -> &'static str {
    match event {
        SseEvent::TurnStarted { .. } => "turn_started",
        SseEvent::TurnFinished { .. } => "turn_finished",
        SseEvent::Response { .. } => "response",
        SseEvent::Thinking { .. } => "thinking",
        SseEvent::ToolStarted { .. } => "tool_started",
        SseEvent::ToolCompleted { .. } => "tool_completed",
        SseEvent::ToolResult { .. } => "tool_result",
        SseEvent::StreamChunk { .. } => "stream_chunk",
        SseEvent::StreamGap { .. } => "stream_gap",
        SseEvent::Status { .. } => "status",
        SseEvent::JobStarted { .. } => "job_started",
        SseEvent::ApprovalNeeded { .. } => "approval_needed",
        SseEvent::AuthRequired { .. } => "auth_required",
        SseEvent::AuthCompleted { .. } => "auth_completed",
        SseEvent::Error { .. } => "error",
        SseEvent::Heartbeat => "heartbeat",
        SseEvent::JobMessage { .. } => "job_message",
        SseEvent::JobToolUse { .. } => "job_tool_use",
        SseEvent::JobToolResult { .. } => "job_tool_result",
        SseEvent::JobStatus { .. } => "job_status",
        SseEvent::JobResult { .. } => "job_result",
        SseEvent::ImageGenerated { .. } => "image_generated",
        SseEvent::Suggestions { .. } => "suggestions",
        SseEvent::TurnCost { .. } => "turn_cost",
        SseEvent::ExtensionStatus { .. } => "extension_status",
        SseEvent::ReasoningUpdate { .. } => "reasoning_update",
        SseEvent::SkillActivated { .. } => "skill_activated",
        SseEvent::GateRequired { .. } => "gate_required",
        SseEvent::GateResolved { .. } => "gate_resolved",
        SseEvent::TurnRecovery { .. } => "turn_recovery",
        SseEvent::ThreadStateChanged { .. } => "thread_state_changed",
        SseEvent::ChildThreadSpawned { .. } => "child_thread_spawned",
    }
}

/// Sends `status` through the real channel method; returns the frames added (one).
#[cfg(test)]
async fn status(gateway: &GatewayChannel, msg: &IncomingMessage, status: StatusUpdate) -> usize {
    gateway
        .send_status(status, &msg.metadata)
        .await
        .expect("status");
    1
}

/// A first turn on Engine V2 with tools, a generated image and the reply.
#[cfg(test)]
async fn emit_tool_turn(gateway: &GatewayChannel) -> usize {
    let msg = chat_message(FIRST_MESSAGE, Some(TURN));
    let sse = &gateway.state().sse;
    let mut frames = 0;
    gateway
        .message_lifecycle(&msg, MessageLifecycle::Started)
        .await;
    frames += 1;
    frames += status(
        gateway,
        &msg,
        StatusUpdate::Thinking("Processing...".into()),
    )
    .await;
    // Engine V2 structural events use the correlated broadcast of the bridge.
    sse.broadcast_correlated(
        Some(USER),
        &msg.metadata,
        SseEvent::ThreadStateChanged {
            thread_id: THREAD.into(),
            from_state: "Created".into(),
            to_state: "Running".into(),
            reason: None,
        },
    );
    frames += 1;
    frames += status(
        gateway,
        &msg,
        StatusUpdate::StreamChunk("Let me draw that. ".into()),
    )
    .await;
    let skills = StatusUpdate::SkillActivated {
        skill_names: vec!["image-tools".into()],
    };
    frames += status(gateway, &msg, skills).await;
    let reasoning = StatusUpdate::ReasoningUpdate {
        narrative: "Selected 2 tools based on task requirements and available context.".into(),
        decisions: vec![ToolDecision {
            tool_name: "image_generate".into(),
            rationale: "Create the requested image.".into(),
        }],
    };
    frames += status(gateway, &msg, reasoning).await;
    frames += emit_tool_calls(gateway, &msg).await;
    let cost = StatusUpdate::TurnCost {
        input_tokens: 2101,
        output_tokens: 88,
        cost_usd: "0.0042".into(),
    };
    frames += status(gateway, &msg, cost).await;
    let suggestions = StatusUpdate::Suggestions {
        suggestions: vec!["Make it darker".into(), "Add a moon".into()],
    };
    frames += status(gateway, &msg, suggestions).await;
    let reply = OutgoingResponse::text("Here is the image. *stamps hooves*");
    gateway.respond(&msg, reply).await.expect("respond");
    gateway
        .message_lifecycle(&msg, MessageLifecycle::Finished(TurnOutcome::Replied))
        .await;
    frames + 2
}

/// The tool events of the first turn: an image tool and a failing HTTP call.
#[cfg(test)]
async fn emit_tool_calls(gateway: &GatewayChannel, msg: &IncomingMessage) -> usize {
    let statuses = [
        StatusUpdate::ToolStarted {
            name: "image_generate".into(),
        },
        StatusUpdate::ImageGenerated {
            data_url: format!("data:image/png;base64,{TINY_PNG_B64}"),
            path: Some("generated/sunset.png".into()),
        },
        StatusUpdate::ToolCompleted {
            name: "image_generate".into(),
            success: true,
            error: None,
            parameters: Some("8123ms".into()),
        },
        StatusUpdate::ToolStarted {
            name: "http".into(),
        },
        StatusUpdate::ToolResult {
            name: "http".into(),
            preview: "{\"status\": 503}".into(),
        },
        StatusUpdate::ToolCompleted {
            name: "http".into(),
            success: false,
            error: Some("HTTP 503 Service Unavailable".into()),
            parameters: Some("{\n  \"url\": \"https://example.com/weather\"\n}".into()),
        },
        StatusUpdate::Status("Done".into()),
    ];
    let mut frames = 0;
    for update in statuses {
        frames += status(gateway, msg, update).await;
    }
    frames
}

/// A second turn that pauses at an approval, then its continuation (the approval
/// message carries the owning turn's client id) with a credential request.
#[cfg(test)]
async fn emit_approval_turn(gateway: &GatewayChannel) -> usize {
    let paused = chat_message(PAUSED_MESSAGE, Some(SECOND_TURN));
    let sse = &gateway.state().sse;
    gateway
        .message_lifecycle(&paused, MessageLifecycle::Started)
        .await;
    let parameters = json!({"command": "ls -la"});
    let resume_kind = lunarwing_engine::ResumeKind::Approval { allow_always: true };
    sse.broadcast_correlated(
        Some(USER),
        &paused.metadata,
        SseEvent::GateRequired {
            request_id: GATE_REQUEST.into(),
            gate_name: "approval".into(),
            tool_name: "shell".into(),
            description: "Run a shell command".into(),
            parameters: serde_json::to_string_pretty(&parameters).expect("JSON"),
            resume_kind: serde_json::to_value(&resume_kind).expect("JSON"),
            thread_id: Some(THREAD.into()),
        },
    );
    let approval = StatusUpdate::ApprovalNeeded {
        request_id: GATE_REQUEST.into(),
        tool_name: "shell".into(),
        description: "Run a shell command".into(),
        parameters,
        allow_always: true,
    };
    let frames = 2 + status(gateway, &paused, approval).await;
    gateway
        .message_lifecycle(&paused, MessageLifecycle::Finished(TurnOutcome::Paused))
        .await;
    frames + 1 + emit_continuation(gateway).await
}

/// The resumed turn: the gate decision, credential prompts, engine events, a job, the
/// reply.
#[cfg(test)]
async fn emit_continuation(gateway: &GatewayChannel) -> usize {
    let msg = chat_message(APPROVAL_MESSAGE, Some(SECOND_TURN));
    let sse = &gateway.state().sse;
    // The agent loop starts the approval message before its handler resolves the gate.
    gateway
        .message_lifecycle(&msg, MessageLifecycle::Started)
        .await;
    // `resolve_gate` announces the decision without correlation.
    sse.broadcast_for_user(
        USER,
        SseEvent::GateResolved {
            request_id: GATE_REQUEST.into(),
            gate_name: "approval".into(),
            tool_name: "shell".into(),
            resolution: "approved".into(),
            message: "Gate approved. Resuming execution.".into(),
            thread_id: Some(THREAD.into()),
        },
    );
    let mut frames = 2;
    let statuses = [
        StatusUpdate::ExternalWaiting {
            gate_name: "deploy-approval".into(),
        },
        StatusUpdate::AuthRequired {
            extension_name: "notion".into(),
            instructions: Some("Paste your Notion integration token.".into()),
            auth_url: None,
            setup_url: Some("https://www.notion.so/my-integrations".into()),
        },
        StatusUpdate::AuthCompleted {
            extension_name: "notion".into(),
            success: true,
            message: "notion authenticated (3 tools loaded)".into(),
        },
        StatusUpdate::JobStarted {
            job_id: "7d3e9f1a-2b4c-4d5e-8f6a-1b2c3d4e5f6a".into(),
            title: "Build the site".into(),
            browse_url: "/projects/7d3e9f1a-2b4c-4d5e-8f6a-1b2c3d4e5f6a/".into(),
        },
    ];
    for update in statuses {
        frames += status(gateway, &msg, update).await;
    }
    let engine_events = [
        SseEvent::TurnRecovery {
            thread_id: THREAD.into(),
            failure_kind: "transient".into(),
            recovery_state: "retrying".into(),
            recovery_attempt: 1,
            message: "The model call failed; retrying.".into(),
        },
        SseEvent::ChildThreadSpawned {
            parent_thread_id: THREAD.into(),
            child_thread_id: "1e2d3c4b-5a69-4788-9a0b-c1d2e3f4a5b6".into(),
            goal: "Summarize the build log".into(),
        },
    ];
    for event in engine_events {
        sse.broadcast_correlated(Some(USER), &msg.metadata, event);
        frames += 1;
    }
    gateway
        .respond(&msg, OutgoingResponse::text("The command ran."))
        .await
        .expect("respond");
    gateway
        .message_lifecycle(&msg, MessageLifecycle::Finished(TurnOutcome::Replied))
        .await;
    frames + 2
}

/// Out-of-band events: proactive messages, the job bus, extensions, globals.
#[cfg(test)]
async fn emit_out_of_band(gateway: &GatewayChannel) -> usize {
    let sse = &gateway.state().sse;
    let notice = OutgoingResponse::text("Reminder: stand-up in 5 minutes.").in_thread(THREAD);
    gateway.broadcast(USER, notice).await.expect("broadcast");
    let job = "7d3e9f1a-2b4c-4d5e-8f6a-1b2c3d4e5f6a";
    let scoped = [
        SseEvent::JobMessage {
            job_id: job.into(),
            role: "assistant".into(),
            content: "Installing dependencies.".into(),
        },
        SseEvent::JobToolUse {
            job_id: job.into(),
            tool_name: "shell".into(),
            input: json!({"command": "npm ci"}),
        },
        SseEvent::JobToolResult {
            job_id: job.into(),
            tool_name: "shell".into(),
            output: "added 120 packages".into(),
        },
        SseEvent::JobStatus {
            job_id: job.into(),
            message: "Running".into(),
        },
        SseEvent::JobResult {
            job_id: job.into(),
            status: "completed".into(),
            session_id: Some("s-1".into()),
            fallback_deliverable: None,
        },
        SseEvent::ExtensionStatus {
            extension_name: "notion".into(),
            status: "active".into(),
            message: None,
        },
        // No producer in the current tree: shapes the client must still accept.
        SseEvent::SkillActivated {
            skill_names: vec!["image-tools".into()],
            thread_id: Some(THREAD.into()),
        },
        SseEvent::Error {
            message: "Something went wrong.".into(),
            thread_id: Some(THREAD.into()),
        },
    ];
    let frames = 1 + scoped.len();
    for event in scoped {
        sse.broadcast_for_user(USER, event);
    }
    sse.broadcast(SseEvent::Heartbeat);
    frames + 1
}

/// Splits one SSE body chunk (`event: <name>\ndata: <json>\n\n`) into name and data.
#[cfg(test)]
fn parse_frame(chunk: &[u8]) -> (String, String) {
    let frame = std::str::from_utf8(chunk).expect("UTF-8 frame");
    let body = frame.strip_suffix("\n\n").expect("frame terminator");
    let (event, data) = body.split_once('\n').expect("event and data lines");
    let name = event.strip_prefix("event: ").expect("event line");
    let data = data.strip_prefix("data: ").expect("one data line");
    assert!(!data.contains('\n'), "data must fit on one line");
    (name.to_string(), data.to_string())
}

/// Reads `count` frames of an SSE response body, skipping keep-alive comments.
#[cfg(test)]
async fn read_frames(sse: impl IntoResponse, count: usize) -> Vec<(String, String)> {
    let mut body = sse.into_response().into_body().into_data_stream();
    let mut frames = Vec::with_capacity(count);
    while frames.len() < count {
        let chunk = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("a frame in time")
            .expect("stream open")
            .expect("frame bytes");
        if !chunk.starts_with(b":") {
            frames.push(parse_frame(&chunk));
        }
    }
    frames
}

/// The `stream_gap` a lagging subscriber receives (a separate manager, so the main
/// stream is not lagged).
#[cfg(test)]
async fn stream_gap_frame() -> (String, String) {
    let sse = SseManager::new();
    let stream = sse.subscribe(None).expect("subscribe");
    for _ in 0..257 {
        sse.broadcast(SseEvent::Heartbeat);
    }
    read_frames(stream, 1).await.remove(0)
}

/// The NDJSON golden file: one `{"event": <name>, "data": <payload>}` per frame, with
/// the payload bytes exactly as sent on the `data:` line.
#[cfg(test)]
fn ndjson(frames: &[(String, String)]) -> String {
    frames
        .iter()
        .map(|(name, data)| {
            let name = serde_json::to_string(name).expect("JSON string");
            format!("{{\"event\":{name},\"data\":{data}}}\n")
        })
        .collect()
}

/// Every event an M2a gateway emits, produced by the real channel methods and the
/// SSE manager's broadcast paths (Engine V2 bridge events through the correlated
/// broadcast, as `deliver_thread_event` and the gate prompt use it) and read back
/// from the real SSE response body. The TypeScript client is checked against the
/// golden file; regenerate it with `LW_REGEN_FIXTURES=1`.
#[tokio::test]
async fn sse_current_fixture_matches_server_output() {
    let gateway = gateway();
    let sse = Arc::clone(&gateway.state().sse);
    let stream = sse.subscribe(Some(USER.to_string())).expect("subscribe");
    let mut raw = subscribe(&sse);

    let mut count = emit_tool_turn(&gateway).await;
    count += emit_approval_turn(&gateway).await;
    count += emit_out_of_band(&gateway).await;
    let mut frames = read_frames(stream, count).await;
    frames.push(stream_gap_frame().await);

    // Each frame's event name is its payload's `type`, and the merged ids never
    // duplicate a field of the event itself.
    let mut names = BTreeSet::new();
    for (name, data) in &frames {
        let payload: serde_json::Value = serde_json::from_str(data).expect("JSON payload");
        assert_eq!(payload["type"], json!(name), "{data}");
        for key in ["\"message_id\":", "\"client_turn_id\":"] {
            assert!(data.matches(key).count() <= 1, "duplicate {key} in {data}");
        }
        names.insert(name.clone());
    }
    // And every variant is sampled.
    let mut variants = BTreeSet::from(["stream_gap"]);
    for _ in 0..count {
        variants.insert(sse_variant(&next_event(&mut raw).await.event));
    }
    assert_eq!(variants.len(), SSE_VARIANTS, "{variants:?}");
    assert_eq!(
        names,
        variants
            .iter()
            .map(|name| name.to_string())
            .collect::<BTreeSet<_>>(),
        "frames and variants disagree"
    );

    let actual = ndjson(&frames);
    if std::env::var_os(REGEN_ENV).is_some_and(|value| value == "1") {
        std::fs::write(FIXTURE, &actual).expect("write the golden file");
        return;
    }
    let expected = std::fs::read_to_string(FIXTURE).unwrap_or_default();
    assert!(
        expected == actual,
        "{FIXTURE} is out of date; rerun with {REGEN_ENV}=1 and review the diff.\nexpected:\n{expected}\nactual:\n{actual}"
    );
}

#[test]
fn sse_current_fixture_lines_are_event_and_data_objects() {
    let Ok(golden) = std::fs::read_to_string(FIXTURE) else {
        panic!("{FIXTURE} is missing; generate it with {REGEN_ENV}=1");
    };
    for line in golden.lines() {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("one JSON object per line");
        let object = record.as_object().expect("an object");
        assert_eq!(object.len(), 2, "{line}");
        assert_eq!(record["data"]["type"], record["event"], "{line}");
    }
}
