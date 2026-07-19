mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use rust_decimal::Decimal;

use lunarwing::channels::wasm::{
    ChannelCapabilitiesFile, SharedWasmChannel, WasmChannel, WasmChannelRuntime,
    WasmChannelRuntimeConfig,
};
use lunarwing::channels::{Channel, OutgoingResponse, StatusUpdate};
use lunarwing::error::LlmError;
use lunarwing::llm::{
    CompletionRequest, CompletionResponse, FinishReason, LlmProvider, LlmStream, LlmStreamChunk,
    TokenUsage, ToolCompletionRequest, ToolCompletionResponse,
};
use lunarwing::pairing::PairingStore;
use lunarwing::secrets::{CreateSecretParams, InMemorySecretsStore, SecretsCrypto, SecretsStore};

use support::engine_v2_env::{ENGINE_V2_ENV_LOCK, EngineV2EnvGuard};
use support::test_rig::TestRigBuilder;

const TERMINAL_RESPONSE: &str = "real-wasm-engine-final";
const DARKIRC_BEARER_SENTINEL: &str = "darkirc-integration-bearer-sentinel";
const WEECHAT_PASSWORD_SENTINEL: &str = "weechat-integration-password-sentinel";

struct DeterministicEngineLlm;

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

    Arc::new(WasmChannel::new(
        runtime,
        prepared,
        capabilities,
        "default",
        config.to_string(),
        Arc::new(PairingStore::new()),
        None,
    ))
}

#[derive(Debug, Clone)]
struct DarkircRequest {
    authorization: Option<String>,
    payload: serde_json::Value,
}

#[derive(Default)]
struct DarkircFixtureState {
    poll_count: usize,
    ack_count: usize,
    sends: Vec<DarkircRequest>,
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

    let state = Arc::new(Mutex::new(DarkircFixtureState::default()));
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
                        );
                    }
                    let mut state = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.poll_count += 1;
                    let messages = if state.poll_count == 1 {
                        serde_json::json!([{
                            "from": "Alice",
                            "text": "finish through the real DarkIRC component",
                            "ts": "2026-07-19T00:00:00Z"
                        }])
                    } else {
                        serde_json::json!([])
                    };
                    (StatusCode::OK, Json(serde_json::json!({"messages": messages})))
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
    inputs: Vec<WeechatInput>,
}

async fn start_weechat_fixture() -> (
    String,
    Arc<Mutex<WeechatFixtureState>>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(Mutex::new(WeechatFixtureState::default()));
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
                    } else if state.dm_line_requests == 2 {
                        serde_json::json!([{
                            "id": 1,
                            "tags": [
                                "irc_privmsg",
                                "nick_Alice",
                                "host_user@example.test",
                                "account_TrustedUser",
                                "casemapping_rfc1459"
                            ],
                            "prefix": "Alice",
                            "message": "finish through the real WeeChat component"
                        }])
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
                        2 => serde_json::json!([{
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
                        }]),
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
                    state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .inputs
                        .push(WeechatInput {
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
        "dm_policy": "open",
        "allow_from": [],
        "polling_enabled": false,
        "poll_interval_ms": 3000
    });
    let unauthorized =
        load_real_channel("darkirc", &wasm_path, &capabilities_path, config.clone()).await;
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

    let channel = load_real_channel("darkirc", &wasm_path, &capabilities_path, config).await;

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
    let channel = Arc::new(
        Arc::try_unwrap(channel)
            .expect("channel should have one owner before credential injection")
            .with_secrets_store(secrets),
    );

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

    Channel::broadcast(
        channel.as_ref(),
        "Bob",
        OutgoingResponse::text("proactive-darkirc"),
    )
    .await
    .expect("explicit DarkIRC proactive delivery should succeed");

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
        assert_eq!(state.sends.len(), 5);
        assert_eq!(
            state.sends[0].payload,
            serde_json::json!({"to": "Alice", "text": TERMINAL_RESPONSE})
        );
        assert_eq!(
            state.sends[1].payload,
            serde_json::json!({"to": "Bob", "text": "proactive-darkirc"})
        );
        assert_eq!(state.sends[2].payload["to"], "Alice");
        assert!(
            state.sends[2].payload["text"]
                .as_str()
                .is_some_and(|text| text.contains("Waiting for external confirmation"))
        );
        assert_eq!(state.sends[3].payload["to"], "Alice");
        assert!(
            state.sends[3].payload["text"]
                .as_str()
                .is_some_and(|text| text.contains("Approval needed"))
        );
        assert_eq!(state.sends[4].payload["to"], "Alice");
        assert!(
            state.sends[4].payload["text"]
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
async fn real_weechat_component_routes_engine_v2_status_and_proactive_delivery() {
    let _serial = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("weechat"));
    lunarwing::bridge::reset_engine_state().await;

    let (relay_url, fixture, fixture_task) = start_weechat_fixture().await;
    let root = repo_root();
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
    let invalid = Channel::broadcast(
        channel.as_ref(),
        "Alice",
        OutgoingResponse::text("ambiguous"),
    )
    .await
    .expect_err("ambiguous WeeChat proactive target must fail");
    assert!(invalid.to_string().contains("irc.<network>"));

    {
        let state = fixture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            state.inputs.len(),
            4,
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
        for request in &state.inputs {
            let authorization = request
                .authorization
                .as_deref()
                .expect("WeeChat request should include Basic authorization");
            assert!(authorization.starts_with("Basic "));
            assert!(!authorization.contains(WEECHAT_PASSWORD_SENTINEL));
        }
    }

    rig.shutdown_and_wait().await;
    fixture_task.abort();
    env.cleanup().await;
}
