mod support;

use std::collections::VecDeque;
use std::future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use futures::StreamExt;
use rust_decimal::Decimal;
use tokio::sync::Notify;

use lunarwing::channels::wasm::{
    ChannelCapabilitiesFile, SharedWasmChannel, WasmChannel, WasmChannelRuntime,
    WasmChannelRuntimeConfig,
};
use lunarwing::channels::{Channel, IncomingMessage, OutgoingResponse, StatusUpdate};
use lunarwing::context::JobContext;
use lunarwing::error::LlmError;
use lunarwing::llm::{
    CompletionRequest, CompletionResponse, FinishReason, LlmProvider, LlmStream, LlmStreamChunk,
    TokenUsage, ToolCall, ToolCompletionRequest, ToolCompletionResponse,
};
use lunarwing::pairing::PairingStore;
use lunarwing::secrets::{CreateSecretParams, InMemorySecretsStore, SecretsCrypto, SecretsStore};
use lunarwing::tools::{ApprovalRequirement, Tool, ToolError, ToolOutput};

use support::engine_v2_env::{ENGINE_V2_ENV_LOCK, EngineV2EnvGuard};
use support::test_rig::TestRigBuilder;

const TERMINAL_RESPONSE: &str = "real-wasm-engine-final";
const APPROVED_RESPONSE: &str = "approved-through-real-component";
const AUTHENTICATED_RESPONSE: &str = "authenticated-through-real-component";
const CONTROL_CREDENTIAL_SENTINEL: &str = "real-wasm-credential-must-not-enter-history";
const CONTROL_TOOL_CALL_ID: &str = "realwasmgate1";
const DARKIRC_BEARER_SENTINEL: &str = "darkirc-integration-bearer-sentinel";
const WEECHAT_PASSWORD_SENTINEL: &str = "weechat-integration-password-sentinel";

struct DeterministicEngineLlm;

struct DropProbe(Arc<AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct PendingEngineLlm {
    started: Notify,
    dropped: Arc<AtomicBool>,
}

struct ControlEngineLlm {
    calls: AtomicUsize,
}

impl ControlEngineLlm {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn text_response(content: &str) -> String {
        format!("```repl\nFINAL('{content}')\n```")
    }

    fn auth_required_response() -> String {
        Self::text_response(
            r#"{"error":"authentication_required","credential_name":"real_wasm_credential"}"#,
        )
    }

    fn next_stream(&self) -> Result<LlmStream<'static>, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match call {
            0 => vec![
                Ok(LlmStreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some(CONTROL_TOOL_CALL_ID.to_string()),
                    name: Some("real_wasm_gate".to_string()),
                    args_delta: "{}".to_string(),
                }),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "tool_calls".to_string(),
                }),
            ],
            1 => vec![
                Ok(LlmStreamChunk::TextDelta(Self::text_response(
                    APPROVED_RESPONSE,
                ))),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "stop".to_string(),
                }),
            ],
            2 => vec![
                Ok(LlmStreamChunk::TextDelta(Self::auth_required_response())),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "stop".to_string(),
                }),
            ],
            3 => vec![
                Ok(LlmStreamChunk::TextDelta(Self::text_response(
                    AUTHENTICATED_RESPONSE,
                ))),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "stop".to_string(),
                }),
            ],
            _ => {
                return Err(LlmError::RequestFailed {
                    provider: "real-wasm-control-test".to_string(),
                    reason: format!("unexpected control-provider call {call}"),
                });
            }
        };
        Ok(futures::stream::iter(chunks).boxed())
    }

    fn next_tool_response(&self) -> Result<ToolCompletionResponse, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ToolCompletionResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: CONTROL_TOOL_CALL_ID.to_string(),
                    name: "real_wasm_gate".to_string(),
                    arguments: serde_json::json!({}),
                    reasoning: None,
                }],
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::ToolUse,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            1..=3 => {
                let content = match call {
                    1 => Self::text_response(APPROVED_RESPONSE),
                    2 => Self::auth_required_response(),
                    3 => Self::text_response(AUTHENTICATED_RESPONSE),
                    _ => unreachable!(),
                };
                Ok(ToolCompletionResponse {
                    content: Some(content),
                    tool_calls: Vec::new(),
                    input_tokens: 1,
                    output_tokens: 1,
                    finish_reason: FinishReason::Stop,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                })
            }
            _ => Err(LlmError::RequestFailed {
                provider: "real-wasm-control-test".to_string(),
                reason: format!("unexpected control-provider call {call}"),
            }),
        }
    }
}

struct ControlApprovalTool {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for ControlApprovalTool {
    fn name(&self) -> &str {
        "real_wasm_gate"
    }

    fn description(&self) -> &str {
        "Real WASM approval routing fixture"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("real WASM gate executed", Duration::ZERO))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

impl PendingEngineLlm {
    fn new(dropped: Arc<AtomicBool>) -> Self {
        Self {
            started: Notify::new(),
            dropped,
        }
    }

    async fn wait_forever<T>(&self) -> T {
        let _probe = DropProbe(Arc::clone(&self.dropped));
        self.started.notify_one();
        future::pending::<T>().await
    }
}

impl DeterministicEngineLlm {
    fn response_text() -> String {
        format!("```repl\nFINAL('{TERMINAL_RESPONSE}')\n```")
    }

    fn stream() -> LlmStream<'static> {
        futures::stream::iter([
            Ok(LlmStreamChunk::TextDelta(Self::response_text())),
            Ok(LlmStreamChunk::Done {
                usage: Some(TokenUsage::default()),
                finish_reason: "stop".to_string(),
            }),
        ])
        .boxed()
    }
}

#[async_trait]
impl LlmProvider for DeterministicEngineLlm {
    fn model_name(&self) -> &str {
        "real-wasm-engine-test"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        Ok(CompletionResponse {
            content: Self::response_text(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        Ok(Self::stream())
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        Ok(ToolCompletionResponse {
            content: Some(Self::response_text()),
            tool_calls: Vec::new(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_with_tools_stream(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        Ok(Self::stream())
    }
}

#[async_trait]
impl LlmProvider for PendingEngineLlm {
    fn model_name(&self) -> &str {
        "real-wasm-pending-test"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        self.wait_forever().await
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.wait_forever().await
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        self.wait_forever().await
    }

    async fn complete_with_tools_stream(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.wait_forever().await
    }
}

#[async_trait]
impl LlmProvider for ControlEngineLlm {
    fn model_name(&self) -> &str {
        "real-wasm-control-test"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        Err(LlmError::RequestFailed {
            provider: self.model_name().to_string(),
            reason: "control fixture requires tool-capable completion".to_string(),
        })
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        Err(LlmError::RequestFailed {
            provider: self.model_name().to_string(),
            reason: "control fixture requires tool-capable completion".to_string(),
        })
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        self.next_tool_response()
    }

    async fn complete_with_tools_stream(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.next_stream()
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("ic should have a repository parent")
        .to_path_buf()
}

fn required_component(path: PathBuf, build_hint: &str) -> PathBuf {
    assert!(
        path.exists(),
        "required real WASM component is absent at {}. {build_hint}",
        path.display()
    );
    path
}

async fn load_real_channel(
    name: &str,
    wasm_path: &Path,
    capabilities_path: &Path,
    config: serde_json::Value,
    pairing_store: Arc<PairingStore>,
    owner_actor_id: Option<String>,
) -> Arc<WasmChannel> {
    let runtime = Arc::new(
        WasmChannelRuntime::new(WasmChannelRuntimeConfig::for_testing())
            .expect("WASM channel runtime should initialize"),
    );
    let wasm = std::fs::read(wasm_path).expect("required WASM component should be readable");
    let prepared = runtime
        .prepare(
            name,
            &wasm,
            None,
            Some(format!("{name} integration fixture")),
        )
        .await
        .expect("real WASM component should prepare");
    let capabilities = ChannelCapabilitiesFile::from_bytes(
        &std::fs::read(capabilities_path).expect("capabilities should be readable"),
    )
    .expect("capabilities should parse")
    .to_capabilities();

    Arc::new(
        WasmChannel::new(
            runtime,
            prepared,
            capabilities,
            "default",
            config.to_string(),
            pairing_store,
            None,
        )
        .with_owner_actor_id(owner_actor_id),
    )
}

async fn with_darkirc_adapter_secret(channel: Arc<WasmChannel>) -> Arc<WasmChannel> {
    let crypto = Arc::new(
        SecretsCrypto::new(secrecy::SecretString::from(
            lunarwing::secrets::keychain::generate_master_key_hex(),
        ))
        .expect("test secret crypto should initialize"),
    );
    let secrets = Arc::new(InMemorySecretsStore::new(crypto));
    secrets
        .create(
            "default",
            CreateSecretParams::new("darkirc_adapter_secret", DARKIRC_BEARER_SENTINEL),
        )
        .await
        .expect("DarkIRC fixture secret should be stored");
    Arc::new(
        Arc::try_unwrap(channel)
            .expect("channel should have one owner before credential injection")
            .with_secrets_store(secrets),
    )
}

#[derive(Debug, Clone)]
struct DarkircRequest {
    authorization: Option<String>,
    payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Default)]
enum DarkircPollMode {
    #[default]
    Normal,
    Malformed,
    Failed,
}

#[derive(Default)]
struct DarkircFixtureState {
    poll_count: usize,
    ack_count: usize,
    sends: Vec<DarkircRequest>,
    poll_mode: DarkircPollMode,
    poll_batches: VecDeque<serde_json::Value>,
}

fn darkirc_batch(text: &str, timestamp: &str) -> serde_json::Value {
    serde_json::json!([{
        "from": "Alice",
        "text": text,
        "ts": timestamp
    }])
}

fn weechat_dm_line(id: i64, text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "tags": [
            "irc_privmsg",
            "nick_Alice",
            "host_user@example.test",
            "account_TrustedUser",
            "casemapping_rfc1459"
        ],
        "prefix": "Alice",
        "message": text
    })
}

async fn wait_for_darkirc_text(fixture: &Arc<Mutex<DarkircFixtureState>>, expected: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let found = fixture
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .sends
                .iter()
                .any(|request| {
                    request.payload["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(expected))
                });
            if found {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("DarkIRC fixture did not receive text containing {expected:?}"));
}

async fn wait_for_weechat_command(fixture: &Arc<Mutex<WeechatFixtureState>>, expected: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let found = fixture
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .inputs
                .iter()
                .any(|request| {
                    request.payload["command"]
                        .as_str()
                        .is_some_and(|command| command.contains(expected))
                });
            if found {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("WeeChat fixture did not receive text containing {expected:?}"));
}

async fn assert_engine_history_excludes(user_id: &str, sentinel: &str) {
    let projects = lunarwing::bridge::list_engine_projects(user_id)
        .await
        .expect("engine projects should load");
    for project in projects {
        let threads = lunarwing::bridge::list_engine_threads(Some(&project.id), user_id)
            .await
            .expect("engine threads should load");
        for thread in threads {
            let detail = lunarwing::bridge::get_engine_thread(&thread.id, user_id)
                .await
                .expect("engine thread lookup should succeed")
                .expect("engine thread should exist");
            assert!(detail.messages.iter().all(|message| {
                message
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|content| !content.contains(sentinel))
            }));
        }
    }
}

#[tokio::test]
async fn control_fixture_completes_approval_and_auth_without_persisting_credential() {
    let _serial = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("darkirc"));
    lunarwing::bridge::reset_engine_state().await;

    let provider = Arc::new(ControlEngineLlm::new());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool: Arc<dyn Tool> = Arc::new(ControlApprovalTool {
        executions: Arc::clone(&executions),
    });
    let rig = TestRigBuilder::new()
        .with_channel_name("darkirc")
        .with_llm(llm)
        .with_auto_approve_tools(false)
        .with_extra_tools(vec![tool])
        .build()
        .await;
    let scope = "real-wasm-control-fixture";
    let metadata = serde_json::json!({"target": "fixture"});

    rig.send_incoming(
        IncomingMessage::new("darkirc", "default", "run the approval fixture")
            .with_conversation_scope(scope)
            .with_metadata(metadata.clone()),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if rig
                .captured_status_events()
                .iter()
                .any(|status| matches!(status, StatusUpdate::ApprovalNeeded { .. }))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("control fixture should reach the approval gate");
    rig.send_incoming(
        IncomingMessage::new("darkirc", "default", "yes")
            .with_conversation_scope(scope)
            .with_metadata(metadata.clone()),
    )
    .await;
    let responses = rig.wait_for_responses(1, Duration::from_secs(15)).await;
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, APPROVED_RESPONSE);

    rig.send_incoming(
        IncomingMessage::new("darkirc", "default", "authenticate this DM")
            .with_conversation_scope(scope)
            .with_metadata(metadata.clone()),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if rig
                .captured_status_events()
                .iter()
                .any(|status| matches!(status, StatusUpdate::AuthRequired { .. }))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("control fixture should reach the authentication gate");
    rig.send_incoming(
        IncomingMessage::new("darkirc", "default", CONTROL_CREDENTIAL_SENTINEL)
            .with_conversation_scope(scope)
            .with_metadata(metadata),
    )
    .await;
    let responses = rig.wait_for_responses(2, Duration::from_secs(15)).await;
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1].content, AUTHENTICATED_RESPONSE);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);

    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation("darkirc", "default", scope)
        .await
        .expect("control fixture scope should resolve");
    let messages = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("control fixture history should load");
    assert!(
        messages
            .iter()
            .all(|message| !message.content.contains(CONTROL_CREDENTIAL_SENTINEL)),
        "the submitted credential must not enter compatibility history"
    );
    assert_engine_history_excludes("default", CONTROL_CREDENTIAL_SENTINEL).await;

    rig.shutdown_and_wait().await;
    env.cleanup().await;
}

async fn start_darkirc_fixture() -> (
    String,
    Arc<Mutex<DarkircFixtureState>>,
    tokio::task::JoinHandle<()>,
) {
    async fn authenticated(headers: &HeaderMap) -> bool {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == format!("Bearer {DARKIRC_BEARER_SENTINEL}"))
    }

    let state = Arc::new(Mutex::new(DarkircFixtureState {
        poll_batches: VecDeque::from([darkirc_batch(
            "finish through the real DarkIRC component",
            "2026-07-19T00:00:00Z",
        )]),
        ..DarkircFixtureState::default()
    }));
    let app = Router::new()
        .route(
            "/health",
            get(|headers: HeaderMap| async move {
                if authenticated(&headers).await {
                    (StatusCode::OK, Json(serde_json::json!({"irc_connected": true})))
                } else {
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({"error": "unauthorized"})),
                    )
                }
            }),
        )
        .route(
            "/poll",
            get(
                |State(state): State<Arc<Mutex<DarkircFixtureState>>>,
                 headers: HeaderMap| async move {
                    if !authenticated(&headers).await {
                        return (
                            StatusCode::UNAUTHORIZED,
                            Json(serde_json::json!({"error": "unauthorized"})),
                        )
                            .into_response();
                    }
                    let mut state = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.poll_count += 1;
                    match state.poll_mode {
                        DarkircPollMode::Malformed => {
                            return (StatusCode::OK, "{\"messages\":").into_response();
                        }
                        DarkircPollMode::Failed => {
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                Json(serde_json::json!({"error": "fixture failure"})),
                            )
                                .into_response();
                        }
                        DarkircPollMode::Normal => {}
                    }
                    let messages = state
                        .poll_batches
                        .pop_front()
                        .unwrap_or_else(|| serde_json::json!([]));
                    (StatusCode::OK, Json(serde_json::json!({"messages": messages})))
                        .into_response()
                },
            ),
        )
        .route(
            "/ack",
            post(
                |State(state): State<Arc<Mutex<DarkircFixtureState>>>,
                 headers: HeaderMap| async move {
                    if !authenticated(&headers).await {
                        return StatusCode::UNAUTHORIZED;
                    }
                    state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .ack_count += 1;
                    StatusCode::OK
                },
            ),
        )
        .route(
            "/send",
            post(
                |State(state): State<Arc<Mutex<DarkircFixtureState>>>,
                 headers: HeaderMap,
                 body: Bytes| async move {
                    if !authenticated(&headers).await {
                        return StatusCode::UNAUTHORIZED;
                    }
                    let payload = serde_json::from_slice(&body)
                        .expect("DarkIRC send payload should be valid JSON");
                    let authorization = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .sends
                        .push(DarkircRequest {
                            authorization,
                            payload,
                        });
                    StatusCode::OK
                },
            ),
        )
        .with_state(Arc::clone(&state));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("DarkIRC loopback fixture should bind");
    let address = listener
        .local_addr()
        .expect("fixture address should resolve");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}"), state, task)
}

#[derive(Debug, Clone)]
struct WeechatInput {
    authorization: Option<String>,
    payload: serde_json::Value,
}

#[derive(Default)]
struct WeechatFixtureState {
    dm_line_requests: usize,
    group_line_requests: usize,
    serve_self_message: bool,
    fail_next_input: bool,
    dm_lines: VecDeque<serde_json::Value>,
    group_lines: VecDeque<serde_json::Value>,
    inputs: Vec<WeechatInput>,
}

async fn start_weechat_fixture() -> (
    String,
    Arc<Mutex<WeechatFixtureState>>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(Mutex::new(WeechatFixtureState {
        dm_lines: VecDeque::from([weechat_dm_line(
            1,
            "finish through the real WeeChat component",
        )]),
        group_lines: VecDeque::from([serde_json::json!({
            "id": 11,
            "tags": [
                "irc_privmsg",
                "nick_Bob",
                "host_bob@example.test",
                "account_GroupUser",
                "casemapping_rfc1459"
            ],
            "prefix": "Bob",
            "message": "finish through the real WeeChat group"
        })]),
        ..WeechatFixtureState::default()
    }));
    let app = Router::new()
        .route(
            "/api/version",
            get(|| async {
                Json(serde_json::json!({
                    "weechat_version": "integration",
                    "relay_api_version": "2"
                }))
            }),
        )
        .route(
            "/api/buffers",
            get(|| async {
                Json(serde_json::json!([
                    {
                        "id": 1,
                        "full_name": "irc.libera.Alice",
                        "short_name": "Alice"
                    },
                    {
                        "id": 2,
                        "full_name": "irc.libera.&lunarwing",
                        "short_name": "&lunarwing"
                    }
                ]))
            }),
        )
        .route(
            "/api/config",
            get(|| async {
                Json(serde_json::json!({
                    "dm_policy": "open",
                    "group_policy": "open",
                    "allow_from": [],
                    "networks": ["libera"]
                }))
            }),
        )
        .route(
            "/api/buffers/irc.libera.Alice/lines",
            get(
                |State(state): State<Arc<Mutex<WeechatFixtureState>>>| async move {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.dm_line_requests += 1;
                    let lines = if state.serve_self_message {
                        state.serve_self_message = false;
                        serde_json::json!([{
                            "id": 3,
                            "tags": ["irc_privmsg", "self_msg", "nick_LunarWing"],
                            "prefix": "LunarWing",
                            "message": "must not loop"
                        }])
                    } else if state.dm_line_requests > 1 {
                        state
                            .dm_lines
                            .pop_front()
                            .map_or_else(|| serde_json::json!([]), |line| serde_json::json!([line]))
                    } else {
                        serde_json::json!([])
                    };
                    Json(lines)
                },
            ),
        )
        .route(
            "/api/buffers/irc.libera.&lunarwing/lines",
            get(
                |State(state): State<Arc<Mutex<WeechatFixtureState>>>| async move {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.group_line_requests += 1;
                    let lines = match state.group_line_requests {
                        1 => serde_json::json!([{
                            "id": 10,
                            "tags": ["irc_privmsg", "nick_seed"],
                            "prefix": "seed",
                            "message": "historical group line"
                        }]),
                        request if request > 1 => state.group_lines.pop_front().map_or_else(
                            || serde_json::json!([]),
                            |line| serde_json::json!([line]),
                        ),
                        _ => serde_json::json!([]),
                    };
                    Json(lines)
                },
            ),
        )
        .route(
            "/api/input",
            post(
                |State(state): State<Arc<Mutex<WeechatFixtureState>>>,
                 headers: HeaderMap,
                 body: Bytes| async move {
                    let payload = serde_json::from_slice(&body)
                        .expect("WeeChat input payload should be valid JSON");
                    let authorization = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    let mut state = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.inputs.push(WeechatInput {
                        authorization,
                        payload,
                    });
                    if state.fail_next_input {
                        state.fail_next_input = false;
                        StatusCode::INTERNAL_SERVER_ERROR
                    } else {
                        StatusCode::OK
                    }
                },
            ),
        )
        .with_state(Arc::clone(&state));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("WeeChat loopback fixture should bind");
    let address = listener
        .local_addr()
        .expect("fixture address should resolve");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}"), state, task)
}

#[tokio::test]
async fn real_darkirc_component_routes_engine_v2_and_proactive_delivery() {
    let _serial = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("darkirc"));
    lunarwing::bridge::reset_engine_state().await;

    let (adapter_url, fixture, fixture_task) = start_darkirc_fixture().await;
    let root = repo_root();
    let wasm_path = required_component(
        root.join("darkirc_channel_for_lunarwing/darkirc/target/wasm32-wasip2/release/darkirc_channel.wasm"),
        "Build it with cargo component build --release --target wasm32-wasip2.",
    );
    let capabilities_path =
        root.join("darkirc_channel_for_lunarwing/darkirc/darkirc.capabilities.json");
    let config = serde_json::json!({
        "display_name": "DarkIRC",
        "adapter_url": adapter_url,
        "dm_policy": "pairing",
        "allow_from": [],
        "polling_enabled": false,
        "poll_interval_ms": 3000
    });
    let pairing_dir = tempfile::tempdir().expect("DarkIRC pairing directory should be created");
    let pairing_store = Arc::new(PairingStore::with_base_dir(
        pairing_dir.path().to_path_buf(),
    ));
    let request = pairing_store
        .upsert_request("darkirc", "darkirc:nick:alice", None)
        .expect("DarkIRC pairing request should be created");
    pairing_store
        .approve("darkirc", &request.code)
        .expect("DarkIRC pairing approval should succeed")
        .expect("DarkIRC pairing request should exist");
    let unauthorized = load_real_channel(
        "darkirc",
        &wasm_path,
        &capabilities_path,
        config.clone(),
        Arc::clone(&pairing_store),
        None,
    )
    .await;
    unauthorized
        .call_on_start()
        .await
        .expect("missing credentials must not trap DarkIRC startup");
    unauthorized
        .call_on_poll()
        .await
        .expect("missing credentials must fail closed without trapping");
    {
        let state = fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            state.poll_count, 0,
            "unauthorized poll must not be accepted"
        );
        assert_eq!(state.ack_count, 0, "unauthorized poll must not be acked");
        assert!(state.sends.is_empty());
    }

    let channel = load_real_channel(
        "darkirc",
        &wasm_path,
        &capabilities_path,
        config,
        Arc::clone(&pairing_store),
        Some("darkirc:nick:alice".to_string()),
    )
    .await;
    let channel = with_darkirc_adapter_secret(channel).await;

    let llm: Arc<dyn LlmProvider> = Arc::new(DeterministicEngineLlm);
    let rig = TestRigBuilder::new()
        .with_channel_name("capture")
        .with_channel(Box::new(SharedWasmChannel::new(Arc::clone(&channel))))
        .with_llm(llm)
        .build()
        .await;

    channel
        .call_on_poll()
        .await
        .expect("DarkIRC poll callback should execute");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let sends = fixture
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .sends
                .len();
            if sends >= 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("Engine V2 should respond through the DarkIRC adapter");

    assert!(
        pairing_store
            .is_sender_allowed("darkirc", "darkirc:nick:alice", None)
            .expect("DarkIRC pairing allow-list should load"),
        "the real component ingress must pass through an approved pairing principal"
    );
    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation("darkirc", "default", "darkirc:dm:v2:darkirc:nick:alice")
        .await
        .expect("normalized DarkIRC DM scope should resolve");
    let messages = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("DarkIRC conversation history should load");
    assert!(messages.iter().any(|message| {
        message.role == "user"
            && message
                .content
                .contains("finish through the real DarkIRC component")
    }));

    {
        fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .poll_mode = DarkircPollMode::Malformed;
    }
    channel
        .call_on_poll()
        .await
        .expect("malformed DarkIRC poll response must not trap");
    assert_eq!(
        fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ack_count,
        1,
        "malformed poll responses must not be acknowledged"
    );
    {
        fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .poll_mode = DarkircPollMode::Failed;
    }
    channel
        .call_on_poll()
        .await
        .expect("failed DarkIRC poll response must not trap");
    assert_eq!(
        fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ack_count,
        1,
        "failed poll responses must not be acknowledged"
    );

    Channel::broadcast(
        channel.as_ref(),
        "Bob",
        OutgoingResponse::text("proactive-darkirc"),
    )
    .await
    .expect("explicit DarkIRC proactive delivery should succeed");
    Channel::broadcast(
        channel.as_ref(),
        "default",
        OutgoingResponse::text("owner-routed-darkirc"),
    )
    .await
    .expect("DarkIRC owner-scope delivery should use persisted DM metadata");

    channel
        .call_on_status(
            &StatusUpdate::ExternalWaiting {
                gate_name: "operator".to_string(),
            },
            &serde_json::json!({"nick": "Alice"}),
        )
        .await
        .expect("typed external waiting status should execute");
    channel
        .call_on_status(
            &StatusUpdate::ApprovalNeeded {
                request_id: "approval-1".to_string(),
                tool_name: "deploy".to_string(),
                description: "deploy the release".to_string(),
                parameters: serde_json::json!({"environment": "test"}),
                allow_always: false,
            },
            &serde_json::json!({"nick": "Alice"}),
        )
        .await
        .expect("approval status should stay in the originating DarkIRC DM");
    channel
        .call_on_status(
            &StatusUpdate::AuthRequired {
                extension_name: "registry".to_string(),
                instructions: Some("Complete authentication.".to_string()),
                auth_url: Some("https://auth.example/authorize".to_string()),
                setup_url: None,
            },
            &serde_json::json!({"nick": "Alice"}),
        )
        .await
        .expect("auth status should stay in the originating DarkIRC DM");

    {
        let state = fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(state.ack_count, 1, "successful poll must be acked once");
        assert_eq!(state.sends.len(), 6);
        assert_eq!(
            state.sends[0].payload,
            serde_json::json!({"to": "Alice", "text": TERMINAL_RESPONSE})
        );
        assert_eq!(
            state.sends[1].payload,
            serde_json::json!({"to": "Bob", "text": "proactive-darkirc"})
        );
        assert_eq!(
            state.sends[2].payload,
            serde_json::json!({"to": "Alice", "text": "owner-routed-darkirc"})
        );
        assert_eq!(state.sends[3].payload["to"], "Alice");
        assert!(
            state.sends[3].payload["text"]
                .as_str()
                .is_some_and(|text| text.contains("Waiting for external confirmation"))
        );
        assert_eq!(state.sends[4].payload["to"], "Alice");
        assert!(
            state.sends[4].payload["text"]
                .as_str()
                .is_some_and(|text| text.contains("Approval needed"))
        );
        assert_eq!(state.sends[5].payload["to"], "Alice");
        assert!(
            state.sends[5].payload["text"]
                .as_str()
                .is_some_and(|text| text.contains("https://auth.example/authorize"))
        );
        for request in &state.sends {
            assert_eq!(
                request.authorization.as_deref(),
                Some(format!("Bearer {DARKIRC_BEARER_SENTINEL}").as_str())
            );
        }
    }

    rig.shutdown_and_wait().await;
    fixture_task.abort();
    env.cleanup().await;
}

#[tokio::test]
async fn real_darkirc_component_keeps_approval_and_auth_in_the_originating_dm() {
    let _serial = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("darkirc"));
    lunarwing::bridge::reset_engine_state().await;

    let (adapter_url, fixture, fixture_task) = start_darkirc_fixture().await;
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .poll_batches = VecDeque::from([darkirc_batch(
        "run the approval fixture",
        "2026-07-19T00:00:00Z",
    )]);
    let root = repo_root();
    let wasm_path = required_component(
        root.join("darkirc_channel_for_lunarwing/darkirc/target/wasm32-wasip2/release/darkirc_channel.wasm"),
        "Build it with cargo component build --release --target wasm32-wasip2.",
    );
    let pairing_dir = tempfile::tempdir().expect("DarkIRC pairing directory should be created");
    let channel = load_real_channel(
        "darkirc",
        &wasm_path,
        &root.join("darkirc_channel_for_lunarwing/darkirc/darkirc.capabilities.json"),
        serde_json::json!({
            "display_name": "DarkIRC",
            "adapter_url": adapter_url,
            "dm_policy": "open",
            "allow_from": [],
            "polling_enabled": false,
            "poll_interval_ms": 3000
        }),
        Arc::new(PairingStore::with_base_dir(
            pairing_dir.path().to_path_buf(),
        )),
        None,
    )
    .await;
    let channel = with_darkirc_adapter_secret(channel).await;

    let provider = Arc::new(ControlEngineLlm::new());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool: Arc<dyn Tool> = Arc::new(ControlApprovalTool {
        executions: Arc::clone(&executions),
    });
    let rig = TestRigBuilder::new()
        .with_channel_name("capture")
        .with_channel(Box::new(SharedWasmChannel::new(Arc::clone(&channel))))
        .with_llm(llm)
        .with_auto_approve_tools(false)
        .with_extra_tools(vec![tool])
        .build()
        .await;

    channel
        .call_on_poll()
        .await
        .expect("DarkIRC approval-trigger poll should execute");
    wait_for_darkirc_text(&fixture, "Approval needed").await;
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .poll_batches
        .push_back(darkirc_batch("yes", "2026-07-19T00:00:01Z"));
    channel
        .call_on_poll()
        .await
        .expect("DarkIRC approval response poll should execute");
    wait_for_darkirc_text(&fixture, APPROVED_RESPONSE).await;

    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .poll_batches
        .push_back(darkirc_batch(
            "authenticate this DM",
            "2026-07-19T00:00:02Z",
        ));
    channel
        .call_on_poll()
        .await
        .expect("DarkIRC auth-trigger poll should execute");
    wait_for_darkirc_text(&fixture, "Authentication required").await;
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .poll_batches
        .push_back(darkirc_batch(
            CONTROL_CREDENTIAL_SENTINEL,
            "2026-07-19T00:00:03Z",
        ));
    channel
        .call_on_poll()
        .await
        .expect("DarkIRC credential poll should execute");
    wait_for_darkirc_text(&fixture, AUTHENTICATED_RESPONSE).await;

    {
        let state = fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(state.ack_count, 4, "each valid control batch needs one ack");
        assert!(
            state
                .sends
                .iter()
                .all(|request| request.payload["to"] == "Alice")
        );
        assert!(state.sends.iter().all(|request| {
            request.payload["text"]
                .as_str()
                .is_none_or(|text| !text.contains(CONTROL_CREDENTIAL_SENTINEL))
        }));
    }
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation("darkirc", "default", "darkirc:dm:v2:darkirc:nick:alice")
        .await
        .expect("DarkIRC control scope should resolve");
    let messages = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("DarkIRC control history should load");
    assert!(
        messages
            .iter()
            .all(|message| !message.content.contains(CONTROL_CREDENTIAL_SENTINEL)),
        "the submitted credential must not enter compatibility history"
    );
    assert_engine_history_excludes("default", CONTROL_CREDENTIAL_SENTINEL).await;

    rig.shutdown_and_wait().await;
    fixture_task.abort();
    env.cleanup().await;
}

#[tokio::test]
async fn real_darkirc_component_keeps_interrupt_in_the_originating_dm() {
    let _serial = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("darkirc"));
    lunarwing::bridge::reset_engine_state().await;

    let (adapter_url, fixture, fixture_task) = start_darkirc_fixture().await;
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .poll_batches = VecDeque::from([serde_json::json!([{
        "from": "Alice",
        "text": "wait for a scoped interrupt",
        "ts": "2026-07-19T00:00:00Z"
    }])]);
    let root = repo_root();
    let wasm_path = required_component(
        root.join("darkirc_channel_for_lunarwing/darkirc/target/wasm32-wasip2/release/darkirc_channel.wasm"),
        "Build it with cargo component build --release --target wasm32-wasip2.",
    );
    let pairing_dir = tempfile::tempdir().expect("DarkIRC pairing directory should be created");
    let channel = load_real_channel(
        "darkirc",
        &wasm_path,
        &root.join("darkirc_channel_for_lunarwing/darkirc/darkirc.capabilities.json"),
        serde_json::json!({
            "display_name": "DarkIRC",
            "adapter_url": adapter_url,
            "dm_policy": "open",
            "allow_from": [],
            "polling_enabled": false,
            "poll_interval_ms": 3000
        }),
        Arc::new(PairingStore::with_base_dir(
            pairing_dir.path().to_path_buf(),
        )),
        None,
    )
    .await;
    let channel = with_darkirc_adapter_secret(channel).await;

    let dropped = Arc::new(AtomicBool::new(false));
    let pending = Arc::new(PendingEngineLlm::new(Arc::clone(&dropped)));
    let llm: Arc<dyn LlmProvider> = pending.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("capture")
        .with_channel(Box::new(SharedWasmChannel::new(Arc::clone(&channel))))
        .with_llm(llm)
        .build()
        .await;

    channel
        .call_on_poll()
        .await
        .expect("DarkIRC pending-message poll should execute");
    tokio::time::timeout(Duration::from_secs(15), pending.started.notified())
        .await
        .expect("DarkIRC message should reach the pending Engine V2 provider");
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .poll_batches
        .push_back(serde_json::json!([{
            "from": "Alice",
            "text": "/interrupt",
            "ts": "2026-07-19T00:00:01Z"
        }]));
    channel
        .call_on_poll()
        .await
        .expect("DarkIRC interrupt poll should execute");

    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let interrupted = fixture
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .sends
                .iter()
                .any(|request| request.payload["text"] == "Interrupted.");
            if interrupted && dropped.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("DarkIRC interrupt should cancel the originating Engine V2 thread");
    tokio::time::sleep(Duration::from_millis(100)).await;

    {
        let state = fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(state.ack_count, 2, "both valid poll batches need one ack");
        assert_eq!(
            state.sends.len(),
            1,
            "interrupt must not allow a late final"
        );
        assert_eq!(
            state.sends[0].payload,
            serde_json::json!({"to": "Alice", "text": "Interrupted."})
        );
    }
    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation("darkirc", "default", "darkirc:dm:v2:darkirc:nick:alice")
        .await
        .expect("DarkIRC interrupt scope should resolve");
    let messages = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("DarkIRC interrupt history should load");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        0,
        "a cancelled turn must not persist a late assistant response"
    );

    rig.shutdown_and_wait().await;
    fixture_task.abort();
    env.cleanup().await;
}

#[tokio::test]
async fn real_weechat_component_routes_engine_v2_status_and_proactive_delivery() {
    let _serial = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("weechat"));
    lunarwing::bridge::reset_engine_state().await;

    let (relay_url, fixture, fixture_task) = start_weechat_fixture().await;
    let root = repo_root();
    let pairing_dir = tempfile::tempdir().expect("WeeChat pairing directory should be created");
    let wasm_path = required_component(
        root.join("lunarwing_weechat_wss/weechat_relay/target/wasm32-wasip2/release/weechat_relay_channel.wasm"),
        "Build it with cargo component build --release --target wasm32-wasip2.",
    );
    let channel = load_real_channel(
        "weechat",
        &wasm_path,
        &root.join("lunarwing_weechat_wss/weechat_relay/weechat.capabilities.json"),
        serde_json::json!({
            "display_name": "WeeChat",
            "relay_url": relay_url,
            "relay_password": WEECHAT_PASSWORD_SENTINEL,
            "connection_mode": "http",
            "ws_adapter_url": "",
            "networks": ["libera"],
            "dm_policy": "open",
            "dm_policy_explicit": true,
            "group_policy": "open",
            "allow_from": [],
            "max_chunk_length": 420,
            "polling_enabled": false,
            "poll_interval_seconds": 3
        }),
        Arc::new(PairingStore::with_base_dir(
            pairing_dir.path().to_path_buf(),
        )),
        Some("account:libera:trusteduser".to_string()),
    )
    .await;

    let llm: Arc<dyn LlmProvider> = Arc::new(DeterministicEngineLlm);
    let rig = TestRigBuilder::new()
        .with_channel_name("capture")
        .with_channel(Box::new(SharedWasmChannel::new(Arc::clone(&channel))))
        .with_llm(llm)
        .build()
        .await;

    channel
        .call_on_poll()
        .await
        .expect("WeeChat poll callback should execute");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let count = fixture
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .inputs
                .len();
            if count >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("Engine V2 should respond through the WeeChat relay");

    for scope in [
        "weechat:dm:v2:account:libera:trusteduser",
        "weechat:group:libera:&lunarwing",
    ] {
        let conversation_id = rig
            .database()
            .get_or_create_scoped_conversation("weechat", "default", scope)
            .await
            .expect("real WeeChat scope should resolve");
        let messages = rig
            .database()
            .list_conversation_messages(conversation_id)
            .await
            .expect("real WeeChat conversation history should load");
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.role == "assistant")
                .count(),
            1,
            "DM and group scopes must retain independent terminal responses: {scope}"
        );
    }

    let before_self_message = fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .inputs
        .len();
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .serve_self_message = true;
    channel
        .call_on_poll()
        .await
        .expect("self-message poll callback should execute");
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inputs
            .len(),
        before_self_message,
        "self_msg tag must not produce an Engine V2 response loop"
    );

    let dm_metadata = serde_json::json!({
        "buffer": "irc.libera.Alice",
        "network": "libera",
        "target": "Alice",
        "nick": "Alice",
        "is_dm": true
    });
    let group_metadata = serde_json::json!({
        "buffer": "irc.libera.#shared",
        "network": "libera",
        "target": "#shared",
        "nick": "Alice",
        "is_dm": false
    });
    channel
        .call_on_status(
            &StatusUpdate::ExternalWaiting {
                gate_name: format!("operator-{}", "🦆".repeat(200)),
            },
            &dm_metadata,
        )
        .await
        .expect("DM external-waiting status should execute");
    channel
        .call_on_status(
            &StatusUpdate::ExternalWaiting {
                gate_name: "private-group-wait".to_string(),
            },
            &group_metadata,
        )
        .await
        .expect("group suppression callback should execute");
    channel
        .call_on_status(
            &StatusUpdate::AuthRequired {
                extension_name: "registry".to_string(),
                instructions: Some("private instructions".to_string()),
                auth_url: Some("https://auth.example/private".to_string()),
                setup_url: None,
            },
            &group_metadata,
        )
        .await
        .expect("group auth suppression callback should execute");
    channel
        .call_on_status(
            &StatusUpdate::ReasoningUpdate {
                narrative: "must remain web-only".to_string(),
                decisions: Vec::new(),
            },
            &dm_metadata,
        )
        .await
        .expect("ignored reasoning callback should execute");
    Channel::broadcast(
        channel.as_ref(),
        "irc.libera.#lunarwing",
        OutgoingResponse::text("proactive-weechat"),
    )
    .await
    .expect("explicit WeeChat proactive delivery should succeed");
    Channel::broadcast(
        channel.as_ref(),
        "irc.libera.Alice",
        OutgoingResponse::text("proactive-weechat-dm"),
    )
    .await
    .expect("explicit WeeChat proactive DM delivery should succeed");
    Channel::broadcast(
        channel.as_ref(),
        "default",
        OutgoingResponse::text("owner-routed-weechat"),
    )
    .await
    .expect("WeeChat owner-scope delivery should use the persisted full buffer");
    let invalid = Channel::broadcast(
        channel.as_ref(),
        "Alice",
        OutgoingResponse::text("ambiguous"),
    )
    .await
    .expect_err("ambiguous WeeChat proactive target must fail");
    assert!(invalid.to_string().contains("irc.<network>"));
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .fail_next_input = true;
    let relay_failure = Channel::broadcast(
        channel.as_ref(),
        "irc.libera.#lunarwing",
        OutgoingResponse::text("must-report-relay-failure"),
    )
    .await
    .expect_err("WeeChat relay failures must propagate through Channel::broadcast");
    assert!(relay_failure.to_string().contains("HTTP 500"));

    {
        let state = fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            state.inputs.len(),
            7,
            "self messages, group-private statuses, and reasoning must not send"
        );
        assert!(state.inputs.iter().any(|input| input.payload
            == serde_json::json!({
                "buffer_name": "irc.libera.Alice",
                "command": TERMINAL_RESPONSE
            })));
        assert!(state.inputs.iter().any(|input| input.payload
            == serde_json::json!({
                "buffer_name": "irc.libera.&lunarwing",
                "command": TERMINAL_RESPONSE
            })));
        let status = state
            .inputs
            .iter()
            .find_map(|input| {
                input.payload["command"]
                    .as_str()
                    .filter(|command| command.starts_with("[status]"))
            })
            .expect("typed waiting status should produce one status command")
            .to_string();
        assert!(status.starts_with("[status] Waiting for external confirmation"));
        assert!(
            status.len() <= 400,
            "status must respect the IRC byte budget"
        );
        assert!(state.inputs.iter().any(|input| input.payload
            == serde_json::json!({
                "buffer_name": "irc.libera.#lunarwing",
                "command": "proactive-weechat"
            })));
        assert!(state.inputs.iter().any(|input| input.payload
            == serde_json::json!({
                "buffer_name": "irc.libera.Alice",
                "command": "proactive-weechat-dm"
            })));
        assert!(state.inputs.iter().any(|input| input.payload
            == serde_json::json!({
                "buffer_name": "irc.libera.Alice",
                "command": "owner-routed-weechat"
            })));
        assert!(state.inputs.iter().any(|input| input.payload
            == serde_json::json!({
                "buffer_name": "irc.libera.#lunarwing",
                "command": "must-report-relay-failure"
            })));
        let expected_authorization = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("plain:{WEECHAT_PASSWORD_SENTINEL}"))
        );
        for request in &state.inputs {
            let authorization = request
                .authorization
                .as_deref()
                .expect("WeeChat request should include Basic authorization");
            assert_eq!(authorization, expected_authorization);
            assert!(!authorization.contains(WEECHAT_PASSWORD_SENTINEL));
        }
    }

    rig.shutdown_and_wait().await;
    fixture_task.abort();
    env.cleanup().await;
}

#[tokio::test]
async fn real_weechat_component_keeps_approval_and_auth_in_the_originating_dm() {
    let _serial = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("weechat"));
    lunarwing::bridge::reset_engine_state().await;

    let (relay_url, fixture, fixture_task) = start_weechat_fixture().await;
    {
        let mut state = fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.dm_lines = VecDeque::from([weechat_dm_line(1, "run the approval fixture")]);
        state.group_lines.clear();
    }
    let root = repo_root();
    let wasm_path = required_component(
        root.join("lunarwing_weechat_wss/weechat_relay/target/wasm32-wasip2/release/weechat_relay_channel.wasm"),
        "Build it with cargo component build --release --target wasm32-wasip2.",
    );
    let pairing_dir = tempfile::tempdir().expect("WeeChat pairing directory should be created");
    let channel = load_real_channel(
        "weechat",
        &wasm_path,
        &root.join("lunarwing_weechat_wss/weechat_relay/weechat.capabilities.json"),
        serde_json::json!({
            "display_name": "WeeChat",
            "relay_url": relay_url,
            "relay_password": WEECHAT_PASSWORD_SENTINEL,
            "connection_mode": "http",
            "ws_adapter_url": "",
            "networks": ["libera"],
            "dm_policy": "open",
            "dm_policy_explicit": true,
            "group_policy": "open",
            "allow_from": [],
            "max_chunk_length": 420,
            "polling_enabled": false,
            "poll_interval_seconds": 3
        }),
        Arc::new(PairingStore::with_base_dir(
            pairing_dir.path().to_path_buf(),
        )),
        None,
    )
    .await;

    let provider = Arc::new(ControlEngineLlm::new());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool: Arc<dyn Tool> = Arc::new(ControlApprovalTool {
        executions: Arc::clone(&executions),
    });
    let rig = TestRigBuilder::new()
        .with_channel_name("capture")
        .with_channel(Box::new(SharedWasmChannel::new(Arc::clone(&channel))))
        .with_llm(llm)
        .with_auto_approve_tools(false)
        .with_extra_tools(vec![tool])
        .build()
        .await;

    channel
        .call_on_poll()
        .await
        .expect("WeeChat approval-trigger poll should execute");
    wait_for_weechat_command(&fixture, "Approval needed").await;
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dm_lines
        .push_back(weechat_dm_line(2, "yes"));
    channel
        .call_on_poll()
        .await
        .expect("WeeChat approval response poll should execute");
    wait_for_weechat_command(&fixture, APPROVED_RESPONSE).await;

    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dm_lines
        .push_back(weechat_dm_line(3, "authenticate this DM"));
    channel
        .call_on_poll()
        .await
        .expect("WeeChat auth-trigger poll should execute");
    wait_for_weechat_command(&fixture, "Authentication required").await;
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dm_lines
        .push_back(weechat_dm_line(4, CONTROL_CREDENTIAL_SENTINEL));
    channel
        .call_on_poll()
        .await
        .expect("WeeChat credential poll should execute");
    wait_for_weechat_command(&fixture, AUTHENTICATED_RESPONSE).await;

    {
        let state = fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(state.inputs.iter().all(|request| {
            request.payload["buffer_name"] == "irc.libera.Alice"
                && request.payload["command"]
                    .as_str()
                    .is_none_or(|command| !command.contains(CONTROL_CREDENTIAL_SENTINEL))
        }));
        let expected_authorization = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("plain:{WEECHAT_PASSWORD_SENTINEL}"))
        );
        assert!(state.inputs.iter().all(|request| {
            request.authorization.as_deref() == Some(expected_authorization.as_str())
        }));
    }
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation(
            "weechat",
            "default",
            "weechat:dm:v2:account:libera:trusteduser",
        )
        .await
        .expect("WeeChat control scope should resolve");
    let messages = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("WeeChat control history should load");
    assert!(
        messages
            .iter()
            .all(|message| !message.content.contains(CONTROL_CREDENTIAL_SENTINEL)),
        "the submitted credential must not enter compatibility history"
    );
    assert_engine_history_excludes("default", CONTROL_CREDENTIAL_SENTINEL).await;

    rig.shutdown_and_wait().await;
    fixture_task.abort();
    env.cleanup().await;
}

#[tokio::test]
async fn real_weechat_component_keeps_interrupt_in_the_originating_dm() {
    let _serial = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("weechat"));
    lunarwing::bridge::reset_engine_state().await;

    let (relay_url, fixture, fixture_task) = start_weechat_fixture().await;
    {
        let mut state = fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.dm_lines = VecDeque::from([serde_json::json!({
            "id": 1,
            "tags": [
                "irc_privmsg",
                "nick_Alice",
                "host_user@example.test",
                "account_TrustedUser",
                "casemapping_rfc1459"
            ],
            "prefix": "Alice",
            "message": "wait for a scoped interrupt"
        })]);
        state.group_lines.clear();
    }
    let root = repo_root();
    let wasm_path = required_component(
        root.join("lunarwing_weechat_wss/weechat_relay/target/wasm32-wasip2/release/weechat_relay_channel.wasm"),
        "Build it with cargo component build --release --target wasm32-wasip2.",
    );
    let pairing_dir = tempfile::tempdir().expect("WeeChat pairing directory should be created");
    let channel = load_real_channel(
        "weechat",
        &wasm_path,
        &root.join("lunarwing_weechat_wss/weechat_relay/weechat.capabilities.json"),
        serde_json::json!({
            "display_name": "WeeChat",
            "relay_url": relay_url,
            "relay_password": WEECHAT_PASSWORD_SENTINEL,
            "connection_mode": "http",
            "ws_adapter_url": "",
            "networks": ["libera"],
            "dm_policy": "open",
            "dm_policy_explicit": true,
            "group_policy": "open",
            "allow_from": [],
            "max_chunk_length": 420,
            "polling_enabled": false,
            "poll_interval_seconds": 3
        }),
        Arc::new(PairingStore::with_base_dir(
            pairing_dir.path().to_path_buf(),
        )),
        None,
    )
    .await;

    let dropped = Arc::new(AtomicBool::new(false));
    let pending = Arc::new(PendingEngineLlm::new(Arc::clone(&dropped)));
    let llm: Arc<dyn LlmProvider> = pending.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("capture")
        .with_channel(Box::new(SharedWasmChannel::new(Arc::clone(&channel))))
        .with_llm(llm)
        .build()
        .await;

    channel
        .call_on_poll()
        .await
        .expect("WeeChat pending-message poll should execute");
    tokio::time::timeout(Duration::from_secs(15), pending.started.notified())
        .await
        .expect("WeeChat message should reach the pending Engine V2 provider");
    fixture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .dm_lines
        .push_back(serde_json::json!({
            "id": 2,
            "tags": [
                "irc_privmsg",
                "nick_Alice",
                "host_user@example.test",
                "account_TrustedUser",
                "casemapping_rfc1459"
            ],
            "prefix": "Alice",
            "message": "/interrupt"
        }));
    channel
        .call_on_poll()
        .await
        .expect("WeeChat interrupt poll should execute");

    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let interrupted = fixture
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .inputs
                .iter()
                .any(|request| request.payload["command"] == "Interrupted.");
            if interrupted && dropped.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("WeeChat interrupt should cancel the originating Engine V2 thread");
    tokio::time::sleep(Duration::from_millis(100)).await;

    {
        let state = fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            state.inputs.len(),
            1,
            "interrupt must not allow a late final"
        );
        assert_eq!(
            state.inputs[0].payload,
            serde_json::json!({
                "buffer_name": "irc.libera.Alice",
                "command": "Interrupted."
            })
        );
        let expected_authorization = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("plain:{WEECHAT_PASSWORD_SENTINEL}"))
        );
        assert_eq!(
            state.inputs[0].authorization.as_deref(),
            Some(expected_authorization.as_str())
        );
    }
    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation(
            "weechat",
            "default",
            "weechat:dm:v2:account:libera:trusteduser",
        )
        .await
        .expect("WeeChat interrupt scope should resolve");
    let messages = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("WeeChat interrupt history should load");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        0,
        "a cancelled turn must not persist a late assistant response"
    );

    rig.shutdown_and_wait().await;
    fixture_task.abort();
    env.cleanup().await;
}
