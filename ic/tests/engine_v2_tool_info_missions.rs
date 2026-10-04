//! WI#295 acceptance on the real Engine V2 path: `init_engine` wires the
//! mission manager, a Foreground gateway thread holds its planned leases, and
//! CodeAct calls `tool_info` positionally. `tool_info("mission_create")`
//! describes the mission function (`kind: engine_capability`,
//! `callable: true`) while `tool_info("echo")` keeps the plain registry
//! answer.
//!
//! Lease-source identity is covered by the bridge unit tests and the engine
//! `ptr::eq` test: because a thread the host holds no leases for is answered
//! `callable: true`, this test cannot tell a mis-wired lease source apart.
mod support;

use std::time::Duration;

use uuid::Uuid;

use lunarwing::channels::IncomingMessage;

use support::engine_v2_env::{ENGINE_V2_ENV_LOCK, EngineV2EnvGuard};
use support::test_rig::{TestRig, TestRigBuilder};
use support::trace_llm::{LlmTrace, TraceResponse, TraceStep};

const USER_ID: &str = "default";
const PROMPT: &str = "describe mission_create and echo";
/// One CodeAct step. Both answers arrive as Python dicts; the model-visible
/// result is assembled with conditional expressions so it does not depend on
/// how Monty formats a bool.
const CODE: &str = r#"```repl
info = await tool_info("mission_create")
plain = await tool_info("echo")
can = "callable" if info["callable"] else "not-callable"
shape = "has-kind" if "kind" in plain else "plain"
FINAL(info["kind"] + "|" + can + "|" + shape)
```"#;

fn gateway_message(thread_id: Uuid, content: &str) -> IncomingMessage {
    IncomingMessage::new("gateway", USER_ID, content)
        .with_thread(thread_id.to_string())
        .with_metadata(serde_json::json!({
            "thread_id": thread_id,
            "user_id": USER_ID,
        }))
}

async fn register_gateway_thread(rig: &TestRig, thread_id: Uuid) {
    let conversation = rig
        .database()
        .get_or_create_scoped_conversation("gateway", USER_ID, &thread_id.to_string())
        .await
        .expect("gateway conversation should be registered");
    assert_eq!(conversation, thread_id);
}

#[tokio::test]
async fn engine_thread_tool_info_describes_mission_functions() {
    let _lock = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("gateway"));
    lunarwing::bridge::reset_engine_state().await;

    let trace = LlmTrace::single_turn(
        "engine-v2-tool-info-missions",
        PROMPT,
        vec![TraceStep {
            request_hint: None,
            response: TraceResponse::Text {
                content: CODE.to_string(),
                input_tokens: 8,
                output_tokens: 4,
            },
            expected_tool_results: Vec::new(),
        }],
    );
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_trace(trace)
        .build()
        .await;

    let thread_id = Uuid::new_v4();
    register_gateway_thread(&rig, thread_id).await;
    rig.send_incoming(gateway_message(thread_id, PROMPT)).await;

    let responses = rig.wait_for_responses(1, Duration::from_secs(15)).await;
    assert_eq!(responses.len(), 1, "one terminal response expected");
    assert_eq!(responses[0].content, "engine_capability|callable|plain");

    // Code-step calls surface as ToolCompleted events: exactly the two
    // tool_info calls, both successful.
    let completed: Vec<(String, bool)> = rig
        .tool_calls_completed()
        .into_iter()
        .filter(|(name, _)| name.starts_with("tool_info"))
        .collect();
    assert_eq!(
        completed.len(),
        2,
        "two tool_info completions expected: {completed:?}"
    );
    assert!(
        completed.iter().all(|(_, success)| *success),
        "both tool_info calls succeed: {completed:?}"
    );

    rig.shutdown_and_wait().await;
    env.cleanup().await;
}
