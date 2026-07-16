mod support;

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::response::sse::{Event, Sse};
use axum::routing::post;
use axum::{Json, Router};
use futures::StreamExt;
use rig_core::client::CompletionClient;
use rig_core::http_client::ReqwestClient;
use rig_core::providers::openai;
use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

use lunarwing::channels::{IncomingMessage, StatusUpdate};
use lunarwing::context::JobContext;
use lunarwing::llm::{LlmProvider, RigAdapter};
use lunarwing::tools::{Tool, ToolError, ToolOutput};

use support::engine_v2_env::{ENGINE_V2_ENV_LOCK, EngineV2EnvGuard};
use support::test_channel::CapturedDelivery;
use support::test_rig::{TestRig, TestRigBuilder};

const USER_ID: &str = "test-user";
const MODEL: &str = "test-model";
const TOOL_NAME: &str = "tensorzero_echo";
const TOOL_CALL_ID: &str = "calltz001";
const TOOL_RESULT_MARKER: &str = "tensorzero-rust-echo";

#[derive(Clone)]
enum SseFrame {
    Data(String),
    Abort,
}

impl SseFrame {
    fn json(value: JsonValue) -> Self {
        Self::Data(value.to_string())
    }

    fn done() -> Self {
        Self::Data("[DONE]".to_string())
    }
}

#[derive(Clone)]
struct SseState {
    scripts: Arc<Vec<Vec<SseFrame>>>,
    request_count: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<JsonValue>>>,
}

struct TensorZeroFixture {
    base_url: String,
    request_count: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<JsonValue>>>,
    server: tokio::task::JoinHandle<()>,
}

impl TensorZeroFixture {
    async fn start(scripts: Vec<Vec<SseFrame>>) -> Self {
        let request_count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = SseState {
            scripts: Arc::new(scripts),
            request_count: Arc::clone(&request_count),
            requests: Arc::clone(&requests),
        };
        let app = Router::new()
            .route("/openai/v1/chat/completions", post(sse_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TensorZero fixture should bind to loopback");
        let address = listener
            .local_addr()
            .expect("TensorZero fixture should expose its bound address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("TensorZero fixture should serve requests");
        });

        Self {
            base_url: format!("http://{address}/openai/v1"),
            request_count,
            requests,
            server,
        }
    }

    fn provider(&self) -> Arc<dyn LlmProvider> {
        let http_client = ReqwestClient::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("TensorZero fixture HTTP client should build");
        let client = openai::Client::builder()
            .api_key("test-key")
            .base_url(&self.base_url)
            .http_client(http_client)
            .build()
            .expect("TensorZero fixture Rig client should build")
            .completions_api();
        let model = client.completion_model(MODEL);
        Arc::new(RigAdapter::new(model, MODEL))
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }

    fn captured_requests(&self) -> Vec<JsonValue> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl Drop for TensorZeroFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn sse_handler(
    State(state): State<SseState>,
    Json(request): Json<JsonValue>,
) -> Sse<impl futures::Stream<Item = Result<Event, io::Error>>> {
    let request_index = state.request_count.fetch_add(1, Ordering::SeqCst);
    state
        .requests
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(request);
    let frames = state
        .scripts
        .get(request_index)
        .cloned()
        .unwrap_or_else(|| {
            vec![SseFrame::Data(
                json!({"error": {"message": "unexpected fixture request"}}).to_string(),
            )]
        });
    let events = futures::stream::iter(frames).then(|frame| async move {
        match frame {
            SseFrame::Data(data) => Ok(Event::default().data(data)),
            SseFrame::Abort => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "fixture closed before terminal event",
                ))
            }
        }
    });
    Sse::new(events)
}

struct EchoTool {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        "Return a deterministic structured echo result"
    }

    fn parameters_schema(&self) -> JsonValue {
        json!({
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, params: JsonValue, _ctx: &JobContext) -> Result<ToolOutput, ToolError> {
        let value = params
            .get("value")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ToolError::InvalidParameters("value must be a string".to_string()))?;
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::success(
            json!({"echoed": value, "source": TOOL_RESULT_MARKER}),
            Duration::ZERO,
        ))
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

fn text_stream_script() -> Vec<SseFrame> {
    vec![
        text_delta("```repl\n", None),
        text_delta("FINAL('tensorzero-", None),
        text_delta("text-ok')\n```", Some("stop")),
        usage_chunk(17, 7),
        SseFrame::done(),
    ]
}

fn fragmented_tool_script() -> Vec<SseFrame> {
    vec![
        tool_delta(Some(TOOL_CALL_ID), None, None, None),
        tool_delta(None, Some(TOOL_NAME), Some(""), None),
        tool_delta(None, None, Some("{\"value\":\"frag"), None),
        tool_delta(None, None, Some("mented\"}"), None),
        tool_delta(None, None, None, Some("tool_calls")),
        usage_chunk(11, 5),
        SseFrame::done(),
    ]
}

fn terminal_script(response: &str, input_tokens: u32, output_tokens: u32) -> Vec<SseFrame> {
    vec![
        text_delta(&format!("```repl\nFINAL('{response}')\n```"), Some("stop")),
        usage_chunk(input_tokens, output_tokens),
        SseFrame::done(),
    ]
}

fn midstream_error_script() -> Vec<SseFrame> {
    vec![
        text_delta("```repl\nFINAL('partial", None),
        SseFrame::json(json!({
            "error": {
                "message": "TensorZero provider failed mid-stream",
                "type": "inference_error"
            }
        })),
        SseFrame::done(),
    ]
}

fn premature_eof_script() -> Vec<SseFrame> {
    vec![text_delta("```repl\nFINAL('partial", None), SseFrame::Abort]
}

fn text_delta(content: &str, finish_reason: Option<&str>) -> SseFrame {
    SseFrame::json(json!({
        "id": "inference-1",
        "model": MODEL,
        "choices": [{
            "index": 0,
            "finish_reason": finish_reason,
            "delta": {"role": "assistant", "content": content}
        }],
        "usage": null
    }))
}

fn tool_delta(
    id: Option<&str>,
    name: Option<&str>,
    arguments: Option<&str>,
    finish_reason: Option<&str>,
) -> SseFrame {
    let tool_calls = if id.is_none() && name.is_none() && arguments.is_none() {
        Vec::new()
    } else {
        vec![json!({
            "index": 0,
            "id": id,
            "type": "function",
            "function": {"name": name, "arguments": arguments}
        })]
    };
    SseFrame::json(json!({
        "id": "inference-tool-1",
        "model": MODEL,
        "choices": [{
            "index": 0,
            "finish_reason": finish_reason,
            "delta": {"tool_calls": tool_calls}
        }],
        "usage": null
    }))
}

fn usage_chunk(input_tokens: u32, output_tokens: u32) -> SseFrame {
    SseFrame::json(json!({
        "id": "inference-1",
        "model": MODEL,
        "choices": [],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    }))
}

fn gateway_message(thread_id: Uuid, content: &str) -> IncomingMessage {
    IncomingMessage::new("gateway", USER_ID, content)
        .with_thread(thread_id.to_string())
        .with_metadata(json!({"thread_id": thread_id, "user_id": USER_ID}))
}

async fn register_gateway_thread(rig: &TestRig, thread_id: Uuid) {
    let conversation = rig
        .database()
        .get_or_create_scoped_conversation("gateway", USER_ID, &thread_id.to_string())
        .await
        .expect("gateway conversation should be registered");
    assert_eq!(conversation, thread_id);
}

fn assert_stream_request(request: &JsonValue) {
    assert_eq!(request.pointer("/stream"), Some(&JsonValue::Bool(true)));
    assert_eq!(
        request.pointer("/stream_options/include_usage"),
        Some(&JsonValue::Bool(true))
    );
    assert_eq!(
        request.pointer("/model"),
        Some(&JsonValue::String(MODEL.to_string()))
    );
}

fn captured_stream_chunks(rig: &TestRig) -> Vec<String> {
    rig.captured_status_events()
        .into_iter()
        .filter_map(|status| match status {
            StatusUpdate::StreamChunk(content) => Some(content),
            _ => None,
        })
        .collect()
}

async fn assistant_message_count(rig: &TestRig, thread_id: Uuid) -> usize {
    assistant_messages(rig, thread_id).await.len()
}

async fn assistant_messages(rig: &TestRig, thread_id: Uuid) -> Vec<String> {
    let messages = rig
        .database()
        .list_conversation_messages(thread_id)
        .await
        .expect("conversation history should load");
    messages
        .into_iter()
        .filter(|message| message.role == "assistant")
        .map(|message| message.content)
        .collect()
}

#[tokio::test]
async fn text_deltas_usage_and_done_cross_the_real_adapter_chain() {
    let _lock = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("gateway"));
    lunarwing::bridge::reset_engine_state().await;

    let fixture = TensorZeroFixture::start(vec![text_stream_script()]).await;
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_llm(fixture.provider())
        .build()
        .await;
    let thread_id = Uuid::new_v4();
    register_gateway_thread(&rig, thread_id).await;
    rig.send_incoming(gateway_message(thread_id, "stream three chunks"))
        .await;

    let responses = rig.wait_for_responses(1, Duration::from_secs(15)).await;
    assert_eq!(
        responses.len(),
        1,
        "one terminal response should be delivered"
    );
    assert_eq!(responses[0].content, "tensorzero-text-ok");
    let chunks = captured_stream_chunks(&rig);
    assert_eq!(
        chunks,
        vec![
            "```repl\n".to_string(),
            "FINAL('tensorzero-".to_string(),
            "text-ok')\n```".to_string()
        ]
    );
    assert_eq!(chunks.concat(), "```repl\nFINAL('tensorzero-text-ok')\n```");

    let deliveries = rig.captured_deliveries();
    let response_index = deliveries
        .iter()
        .position(|delivery| matches!(delivery, CapturedDelivery::Response { .. }))
        .expect("terminal response should be present");
    assert_eq!(
        deliveries[..response_index]
            .iter()
            .filter(|delivery| matches!(
                delivery,
                CapturedDelivery::Status(StatusUpdate::StreamChunk(_))
            ))
            .count(),
        3
    );
    assert_eq!(fixture.request_count(), 1);
    let requests = fixture.captured_requests();
    assert_eq!(requests.len(), 1);
    assert_stream_request(&requests[0]);
    assert_eq!(
        rig.llm_call_count(),
        1,
        "only terminal usage records a call"
    );
    assert_eq!(rig.total_input_tokens(), 17);
    assert_eq!(rig.total_output_tokens(), 7);
    assert_eq!(assistant_message_count(&rig, thread_id).await, 1);

    rig.shutdown_and_wait().await;
    env.cleanup().await;
}

#[tokio::test]
async fn fragmented_tool_fields_reconstruct_one_action_and_structured_result() {
    let _lock = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("gateway"));
    lunarwing::bridge::reset_engine_state().await;

    let fixture = TensorZeroFixture::start(vec![
        fragmented_tool_script(),
        terminal_script("tensorzero-tool-ok", 13, 4),
    ])
    .await;
    let executions = Arc::new(AtomicUsize::new(0));
    let echo: Arc<dyn Tool> = Arc::new(EchoTool {
        executions: Arc::clone(&executions),
    });
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_llm(fixture.provider())
        .with_extra_tools(vec![echo])
        .build()
        .await;
    let thread_id = Uuid::new_v4();
    register_gateway_thread(&rig, thread_id).await;
    rig.send_incoming(gateway_message(thread_id, "run the fragmented echo"))
        .await;

    let responses = rig.wait_for_responses(1, Duration::from_secs(15)).await;
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, "tensorzero-tool-ok");
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(
        rig.tool_calls_completed()
            .iter()
            .filter(|(name, success)| name.starts_with(TOOL_NAME) && *success)
            .count(),
        1
    );
    let structured_results = rig
        .tool_results()
        .into_iter()
        .filter(|(name, _)| name.starts_with(TOOL_NAME))
        .map(|(_, preview)| preview)
        .collect::<Vec<_>>();
    assert_eq!(structured_results.len(), 1);
    assert!(structured_results[0].contains(TOOL_RESULT_MARKER));
    assert!(structured_results[0].contains("fragmented"));

    assert_eq!(fixture.request_count(), 2);
    let requests = fixture.captured_requests();
    assert_eq!(requests.len(), 2);
    assert_stream_request(&requests[0]);
    assert_stream_request(&requests[1]);
    let tool_result_message = requests[1]
        .pointer("/messages")
        .and_then(JsonValue::as_array)
        .and_then(|messages| {
            messages.iter().find(|message| {
                message.pointer("/role").and_then(JsonValue::as_str) == Some("tool")
            })
        })
        .expect("second request should contain the structured tool result");
    assert_eq!(
        tool_result_message
            .pointer("/tool_call_id")
            .and_then(JsonValue::as_str),
        Some(TOOL_CALL_ID)
    );
    let serialized_result = tool_result_message.to_string();
    assert!(serialized_result.contains(TOOL_RESULT_MARKER));
    assert!(serialized_result.contains("fragmented"));
    assert_eq!(rig.llm_call_count(), 2);
    assert_eq!(rig.total_input_tokens(), 24);
    assert_eq!(rig.total_output_tokens(), 9);
    assert_eq!(assistant_message_count(&rig, thread_id).await, 1);

    rig.shutdown_and_wait().await;
    env.cleanup().await;
}

async fn assert_strict_stream_failure(script: Vec<SseFrame>, expected_error: Option<&str>) {
    let _lock = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("gateway"));
    lunarwing::bridge::reset_engine_state().await;

    let fixture = TensorZeroFixture::start(vec![script]).await;
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_llm(fixture.provider())
        .build()
        .await;
    let thread_id = Uuid::new_v4();
    register_gateway_thread(&rig, thread_id).await;
    rig.send_incoming(gateway_message(thread_id, "fail after one partial chunk"))
        .await;

    let responses = rig.wait_for_responses(1, Duration::from_secs(15)).await;
    assert_eq!(
        responses.len(),
        1,
        "at most one error response should be delivered"
    );
    assert!(responses[0].content.starts_with("Error:"));
    if let Some(expected_error) = expected_error {
        assert!(
            responses[0].content.contains(expected_error),
            "error response should contain {expected_error:?}: {}",
            responses[0].content
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.captured_responses().len(), 1);
    assert_eq!(
        rig.captured_deliveries()
            .iter()
            .filter(|delivery| matches!(delivery, CapturedDelivery::Response { .. }))
            .count(),
        1
    );
    assert_eq!(
        captured_stream_chunks(&rig),
        vec!["```repl\nFINAL('partial".to_string()]
    );
    assert_eq!(fixture.request_count(), 1);
    let requests = fixture.captured_requests();
    assert_eq!(requests.len(), 1);
    assert_stream_request(&requests[0]);
    assert_eq!(
        rig.llm_call_count(),
        0,
        "no synthetic Done should be recorded"
    );
    assert_eq!(rig.total_input_tokens(), 0);
    assert_eq!(rig.total_output_tokens(), 0);
    let assistants = assistant_messages(&rig, thread_id).await;
    assert!(
        assistants.len() <= 1,
        "at most the terminal error may persist"
    );
    assert!(
        assistants
            .iter()
            .all(|content| !content.contains("FINAL('partial")),
        "partial provider content must not persist: {assistants:?}"
    );

    rig.shutdown_and_wait().await;
    env.cleanup().await;
}

#[tokio::test]
async fn midstream_json_error_does_not_commit_a_partial_response() {
    assert_strict_stream_failure(
        midstream_error_script(),
        Some("TensorZero provider failed mid-stream"),
    )
    .await;
}

#[tokio::test]
async fn premature_eof_before_terminal_event_does_not_synthesize_done() {
    assert_strict_stream_failure(premature_eof_script(), None).await;
}
