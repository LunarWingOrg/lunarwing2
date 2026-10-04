//! Exact, authenticated controls for recovered authentication gates.

use secrecy::{ExposeSecret, SecretString};

use super::*;

/// Process-local control payload. Tokens never enter message text or metadata.
#[derive(Clone, Debug)]
pub(crate) struct AuthGateSubmission {
    pub request_id: uuid::Uuid,
    pub thread_id: lunarwing_engine::ThreadId,
    pub credential_name: String,
    pub token: SecretString,
}

async fn verified_auth_gate(
    state: &EngineState,
    message: &IncomingMessage,
    thread_id: lunarwing_engine::ThreadId,
    request_id: uuid::Uuid,
    credential_name: &str,
) -> Result<PendingGate, Error> {
    state
        .pending_gates
        .list_for_user(&message.user_id)
        .await
        .into_iter()
        .find(|gate| {
            gate.thread_id == thread_id
                && gate.request_id == request_id
                && gate_channel_matches(gate, &message.channel)
                && matches!(&gate.resume_kind,
                    lunarwing_engine::ResumeKind::Authentication { credential_name: name, .. }
                    if name == credential_name)
        })
        .ok_or_else(|| {
            engine_err(
                "stale",
                "Authentication request is stale or no longer available",
            )
        })
}

pub(crate) async fn submit(
    agent: &Agent,
    message: &IncomingMessage,
    submission: &AuthGateSubmission,
) -> Result<Option<String>, Error> {
    let lock = ENGINE_STATE
        .get()
        .ok_or_else(|| engine_err("auth", "Engine is not initialized"))?;
    let guard = lock.read().await;
    let state = guard
        .as_ref()
        .ok_or_else(|| engine_err("auth", "Engine is not initialized"))?;
    verified_auth_gate(
        state,
        message,
        submission.thread_id,
        submission.request_id,
        &submission.credential_name,
    )
    .await?;
    drop(guard);
    // resolve_gate atomically checks request ID, ownership, channel and expiry
    // again. A stale queued credential must never fall through to ordinary chat.
    resolve_gate(
        agent,
        message,
        submission.thread_id,
        submission.request_id,
        lunarwing_engine::GateResolution::CredentialProvided {
            token: submission.token.expose_secret().to_string(),
        },
    )
    .await
}

pub(crate) async fn cancel(
    message: &IncomingMessage,
    thread_id: lunarwing_engine::ThreadId,
    request_id: uuid::Uuid,
    credential_name: &str,
) -> Result<(), Error> {
    let lock = ENGINE_STATE
        .get()
        .ok_or_else(|| engine_err("auth", "Engine is not initialized"))?;
    let guard = lock.read().await;
    let state = guard
        .as_ref()
        .ok_or_else(|| engine_err("auth", "Engine is not initialized"))?;
    let pending =
        verified_auth_gate(state, message, thread_id, request_id, credential_name).await?;
    let pending = state
        .pending_gates
        .take_verified(&pending.key(), request_id, &message.channel)
        .await
        .map_err(|error| engine_err("cancel auth", error))?;
    cancel_pending_gate(state, &pending).await
}

pub(super) async fn cancel_pending_gate(
    state: &EngineState,
    pending: &PendingGate,
) -> Result<(), Error> {
    // Waiting threads are absent from the running map; finalize directly.
    finalize_silently_stopped_pending_thread(state, pending).await?;
    if let Some(ref sse) = state.sse {
        sse.broadcast_for_user(
            &pending.user_id,
            AppEvent::GateResolved {
                request_id: pending.request_id.to_string(),
                gate_name: pending.gate_name.clone(),
                tool_name: pending.action_name.clone(),
                resolution: "cancelled".into(),
                message: "Gate cancelled.".into(),
                thread_id: Some(pending.thread_id.to_string()),
            },
        );
    }
    Ok(())
}

#[cfg(all(test, feature = "libsql"))]
mod tests {
    use super::*;
    use crate::bridge::router::tests::{
        ENGINE_STATE_TEST_LOCK, TestStore, make_expected_test_state, sample_pending_gate,
        save_waiting_thread,
    };
    use crate::channels::web::{GatewayChannel, auth as web_auth, server};
    use crate::config::GatewayConfig;
    use crate::gate::store::PendingGateStore;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        middleware,
        routing::{get, post},
    };
    use futures::StreamExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    // Represents an already-loaded extension; configuration and secret storage
    // still go through the real extension manager and HTTP handler.
    struct LoadedTool;

    #[async_trait::async_trait]
    impl crate::tools::Tool for LoadedTool {
        fn name(&self) -> &str {
            "demo"
        }
        fn description(&self) -> &str {
            "auth fixture"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }
        fn is_wasm_tool(&self) -> bool {
            true
        }
        async fn execute(
            &self,
            _: Value,
            _: &crate::context::JobContext,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            Err(crate::tools::ToolError::ExecutionFailed(
                "unexpected execution".into(),
            ))
        }
    }

    struct Rig {
        app: Router,
        store: Arc<TestStore>,
        gates: Arc<PendingGateStore>,
        first: PendingGate,
        second: PendingGate,
        gateway: Arc<server::GatewayState>,
        messages: tokio::sync::mpsc::Receiver<IncomingMessage>,
        secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync>,
        disk: tempfile::TempDir,
    }

    impl Rig {
        async fn new() -> Self {
            let disk = tempfile::tempdir().unwrap();
            let secrets = Arc::new(crate::secrets::InMemorySecretsStore::new(Arc::new(
                crate::secrets::SecretsCrypto::new(SecretString::from(
                    "test-key-at-least-32-chars-long!!".to_string(),
                ))
                .unwrap(),
            )));
            let tools = Arc::new(crate::tools::ToolRegistry::new());
            tools.register(Arc::new(LoadedTool)).await;
            std::fs::write(disk.path().join("demo.wasm"), b"loaded test fixture").unwrap();
            std::fs::write(
                disk.path().join("demo.capabilities.json"),
                json!({
                    "setup": {"required_secrets": [{"name": "demo", "prompt": "token"}]}
                })
                .to_string(),
            )
            .unwrap();
            let extensions = Arc::new(crate::extensions::ExtensionManager::new(
                Arc::new(crate::tools::mcp::session::McpSessionManager::new()),
                Arc::new(crate::tools::mcp::process::McpProcessManager::new()),
                secrets.clone(),
                tools,
                None,
                None,
                disk.path().to_path_buf(),
                disk.path().join("channels"),
                None,
                "alice".into(),
                None,
                vec![],
            ));
            let channel = GatewayChannel::new(
                GatewayConfig {
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
            .with_extension_manager(extensions);
            let gateway = channel.state().clone();
            let (tx, messages) = tokio::sync::mpsc::channel(4);
            *gateway.msg_tx.write().await = Some(tx);
            let store = Arc::new(TestStore::new());
            let mut state = make_expected_test_state(store.clone());
            state.sse = Some(gateway.sse.clone());
            state.secrets_store = Some(secrets.clone());
            let gates = Arc::new(PendingGateStore::new(Some(Arc::new(
                crate::gate::persistence::FileGatePersistence::new(disk.path().join("gates.json")),
            ))));
            state.pending_gates = gates.clone();
            let first = Self::gate(&state, &store, "alice").await;
            let second = Self::gate(&state, &store, "alice").await;
            Self::gate(&state, &store, "bob").await;
            *ENGINE_STATE.get_or_init(|| RwLock::new(None)).write().await = Some(state);
            let auth: web_auth::CombinedAuthState =
                web_auth::MultiAuthState::single("test-token".into(), "alice".into()).into();
            let app = Router::new()
                .route(
                    "/api/chat/auth-token",
                    post(server::chat_auth_token_handler),
                )
                .route(
                    "/api/chat/auth-cancel",
                    post(server::chat_auth_cancel_handler),
                )
                .route(
                    "/api/chat/pending-gates",
                    get(server::chat_pending_gates_handler),
                )
                .route_layer(middleware::from_fn_with_state(
                    auth,
                    web_auth::auth_middleware,
                ))
                .with_state(gateway.clone());
            Self {
                app,
                store,
                gates,
                first,
                second,
                gateway,
                messages,
                secrets,
                disk,
            }
        }

        async fn gate(state: &EngineState, store: &TestStore, user: &str) -> PendingGate {
            let thread = save_waiting_thread(store, state.default_project_id, user).await;
            let mut gate = sample_pending_gate(
                user,
                thread.id,
                lunarwing_engine::ResumeKind::Authentication {
                    credential_name: "demo".into(),
                    instructions: "token".into(),
                    auth_url: None,
                },
            );
            gate.conversation_id = state
                .conversation_manager
                .get_or_create_conversation(&format!("gateway:{}", thread.id), user)
                .await
                .unwrap();
            gate.source_channel = "gateway".into();
            gate.resume_output = Some(json!({"authenticated": true}));
            state.pending_gates.insert(gate.clone()).await.unwrap();
            gate
        }

        fn body(&self) -> Value {
            json!({"extension_name": "demo", "request_id": self.first.request_id,
                "thread_id": self.first.thread_id.0, "token": "test-credential"})
        }

        async fn request(
            &self,
            path: &str,
            body: Value,
            authenticated: bool,
        ) -> (StatusCode, Value) {
            let mut request = Request::builder()
                .uri(path)
                .method(if path.ends_with("pending-gates") {
                    "GET"
                } else {
                    "POST"
                })
                .header("content-type", "application/json");
            if authenticated {
                request = request.header("Authorization", "Bearer test-token");
            }
            let response = self
                .app
                .clone()
                .oneshot(request.body(Body::from(body.to_string())).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 16384).await.unwrap();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }

        async fn assert_first_removed(&self) {
            assert!(self.gates.peek(&self.first.key()).await.is_none());
            assert!(self.gates.peek(&self.second.key()).await.is_some());
            assert_eq!(self.gates.list_for_user("bob").await.len(), 1);
            let restored = PendingGateStore::new(Some(Arc::new(
                crate::gate::persistence::FileGatePersistence::new(
                    self.disk.path().join("gates.json"),
                ),
            )));
            restored.restore_from_persistence().await.unwrap();
            assert!(restored.peek(&self.first.key()).await.is_none());
            let (status, snapshot) = self
                .request("/api/chat/pending-gates", Value::Null, true)
                .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(snapshot["gates"].as_array().unwrap().len(), 1);
            assert_eq!(
                snapshot["gates"][0]["request_id"],
                self.second.request_id.to_string()
            );
        }
    }

    #[tokio::test]
    async fn recovered_auth_cancel_stops_exact_thread_and_removes_durable_gate() {
        let _guard = ENGINE_STATE_TEST_LOCK.lock().await;
        let rig = Rig::new().await;
        let mut events = rig.gateway.sse.subscribe_raw(Some("alice".into())).unwrap();
        let (status, body) = rig.request("/api/chat/auth-cancel", rig.body(), true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true);
        rig.assert_first_removed().await;
        // Cancellation must not persist a successful completion (WI#302).
        assert_eq!(
            rig.store
                .load_thread(rig.first.thread_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            lunarwing_engine::ThreadState::Failed
        );
        assert_eq!(
            rig.store
                .load_thread(rig.second.thread_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            lunarwing_engine::ThreadState::Waiting
        );
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.next())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(event, crate::channels::web::types::SseEvent::GateResolved {
            request_id, resolution, ..
        } if request_id == rig.first.request_id.to_string() && resolution == "cancelled")
        );
        assert_eq!(
            rig.request("/api/chat/auth-cancel", rig.body(), true)
                .await
                .0,
            StatusCode::CONFLICT
        );
        *ENGINE_STATE.get().unwrap().write().await = None;
    }

    #[tokio::test]
    async fn recovered_auth_endpoints_reject_wrong_stale_expired_and_partial_identities() {
        let _guard = ENGINE_STATE_TEST_LOCK.lock().await;
        let rig = Rig::new().await;
        for endpoint in ["/api/chat/auth-cancel", "/api/chat/auth-token"] {
            assert_eq!(
                rig.request(endpoint, rig.body(), false).await.0,
                StatusCode::UNAUTHORIZED
            );
            let mut invalid = vec![];
            let mut body = rig.body();
            body["request_id"] = json!(uuid::Uuid::new_v4());
            invalid.push(body);
            let mut body = rig.body();
            body["thread_id"] = json!(rig.second.thread_id.0);
            invalid.push(body);
            let mut body = rig.body();
            body["extension_name"] = json!("different");
            invalid.push(body);
            let bob = rig.gates.list_for_user("bob").await.pop().unwrap();
            let mut body = rig.body();
            body["request_id"] = json!(bob.request_id);
            body["thread_id"] = json!(bob.thread_id.0);
            invalid.push(body);
            for body in invalid {
                assert_eq!(
                    rig.request(endpoint, body, true).await.0,
                    StatusCode::CONFLICT
                );
            }
            let mut body = rig.body();
            body.as_object_mut().unwrap().remove("thread_id");
            assert_eq!(
                rig.request(endpoint, body, true).await.0,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(rig.gates.list_all().await.len(), 3);
        assert!(!rig.secrets.exists("alice", "demo").await.unwrap());
        rig.gates.discard(&rig.first.key()).await.unwrap();
        let mut expired = rig.first.clone();
        expired.expires_at = chrono::Utc::now() - chrono::Duration::seconds(1);
        rig.gates.insert(expired).await.unwrap();
        for endpoint in ["/api/chat/auth-cancel", "/api/chat/auth-token"] {
            assert_eq!(
                rig.request(endpoint, rig.body(), true).await.0,
                StatusCode::CONFLICT
            );
        }
        *ENGINE_STATE.get().unwrap().write().await = None;
    }

    #[tokio::test]
    async fn recovered_auth_token_resumes_selected_thread_among_multiple_pending_gates() {
        let _guard = ENGINE_STATE_TEST_LOCK.lock().await;
        let mut rig = Rig::new().await;
        let (status, body) = rig.request("/api/chat/auth-token", rig.body(), true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true, "{body}");
        assert_eq!(body["activated"], true);
        let message = rig.messages.try_recv().unwrap();
        let submission = message.auth_gate_submission.as_ref().unwrap();
        assert_eq!(submission.request_id, rig.first.request_id);
        assert_eq!(submission.thread_id, rig.first.thread_id);
        assert!(message.content.is_empty());
        assert!(!format!("{message:?}").contains("test-credential"));
        let harness = crate::testing::TestHarnessBuilder::new().build().await;
        let agent = Agent::new(
            crate::config::AgentConfig::for_testing(),
            harness.deps,
            Arc::new(crate::channels::ChannelManager::new()),
            None,
            None,
            None,
            None,
            None,
        );
        let result = submit(&agent, &message, submission).await.unwrap();
        assert_eq!(result.as_deref(), Some("done"));
        rig.assert_first_removed().await;
        let thread = rig
            .store
            .load_thread(rig.first.thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(thread.state, lunarwing_engine::ThreadState::Done);
        assert!(
            !serde_json::to_string(&thread.messages)
                .unwrap()
                .contains("test-credential")
        );
        assert_eq!(
            rig.store
                .load_thread(rig.second.thread_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            lunarwing_engine::ThreadState::Waiting
        );
        // A duplicate queued credential cannot become a new user turn.
        assert!(submit(&agent, &message, submission).await.is_err());
        *ENGINE_STATE.get().unwrap().write().await = None;
    }
}
