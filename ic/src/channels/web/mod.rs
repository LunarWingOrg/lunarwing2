//! Web gateway channel for browser-based access to LunarWing.
//!
//! Provides a single-page web UI with:
//! - Chat with the agent (via REST + SSE)
//! - Workspace/memory browsing
//! - Job management
//!
//! ```text
//! Browser ─── POST /api/chat/send ──► Agent Loop
//!         ◄── GET  /api/chat/events ── SSE stream
//!         ─── GET  /api/chat/ws ─────► WebSocket (bidirectional)
//!         ─── GET  /api/memory/* ────► Workspace
//!         ─── GET  /api/jobs/* ──────► Database
//!         ◄── GET  / ───────────────── Static HTML/CSS/JS
//! ```

pub mod auth;
pub mod emote;
pub(crate) mod handlers;
pub mod log_layer;
pub mod openai_compat;
pub mod responses_api;
pub mod server;
pub mod sse;
pub mod types;
pub(crate) mod util;
pub mod voice;
pub mod ws;

// Coordination admin API (Phase 3): localhost-only server started from main.
pub use handlers::coord::{
    coord_admin_port_from_env, coord_admin_router, coord_admin_router_with_knowledge,
    coord_admin_token_from_env, start_coord_admin_server, start_coord_admin_server_with_knowledge,
};

/// Test helpers for gateway integration tests.
///
/// Always compiled (not behind `#[cfg(test)]`) so that integration tests in
/// `tests/` -- which import this crate as a regular dependency -- can use
/// [`TestGatewayBuilder`](test_helpers::TestGatewayBuilder).
pub mod test_helpers;

#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::agent::SessionManager;
use crate::channels::{
    Channel, IncomingMessage, MessageLifecycle, MessageStream, OutgoingResponse, StatusUpdate,
};
use crate::config::GatewayConfig;
use crate::db::Database;
use crate::error::ChannelError;
use crate::extensions::ExtensionManager;
use crate::orchestrator::job_manager::ContainerJobManager;
use crate::skills::catalog::SkillCatalog;
use crate::skills::registry::SkillRegistry;
use crate::tools::ToolRegistry;
use crate::workspace::Workspace;

use self::log_layer::{LogBroadcaster, LogLevelHandle};

use self::auth::{CombinedAuthState, DbAuthenticator, MultiAuthState};
use self::server::GatewayState;
use self::sse::{Correlation, SseManager};
use self::types::{AppEvent, SseEvent, ToolDecisionDto};

/// Chat messages per user, `(requests, window_secs)`: `POST /api/chat/send`, shared by
/// every client of the user (web UI, TUI). Advertised by `GET /api/chat/capabilities`.
pub(crate) const CHAT_RATE_LIMIT: (u32, u64) = (30, 60);
/// Voice requests per user, `(requests, window_secs)`: transcribe and synthesize together.
pub(crate) const VOICE_RATE_LIMIT: (u32, u64) = (10, 60);

fn per_user_rate_limiter((requests, window_secs): (u32, u64)) -> server::PerUserRateLimiter {
    server::PerUserRateLimiter::new(u64::from(requests), window_secs)
}

/// Web gateway channel implementing the Channel trait.
pub struct GatewayChannel {
    config: GatewayConfig,
    state: Arc<GatewayState>,
    /// Combined auth state: env-var tokens + optional DB-backed tokens.
    auth: CombinedAuthState,
}

impl GatewayChannel {
    /// Create a new gateway channel.
    ///
    /// If no auth token is configured, generates a random one and prints it.
    /// Builds a single-user `MultiAuthState` from the config.
    pub fn new(config: GatewayConfig, owner_id: String) -> Self {
        let auth_token = config.auth_token.clone().unwrap_or_else(|| {
            use rand::RngCore;
            use rand::rngs::OsRng;
            let mut bytes = [0u8; 32];
            OsRng.fill_bytes(&mut bytes);
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        });
        // Generated here or loaded from DB settings, so on single-node
        // installs never in the environment the log scrubber starts from:
        // register it so every daemon log sink redacts it (WI#287 review S4).
        crate::tracing_fmt::register_log_secret(&auth_token);

        let auth = CombinedAuthState {
            env_auth: MultiAuthState::single(auth_token, owner_id.clone()),
            db_auth: None,
        };

        let state = Arc::new(GatewayState {
            msg_tx: tokio::sync::RwLock::new(None),
            sse: Arc::new(SseManager::new()),
            workspace: None,
            workspace_pool: None,
            session_manager: None,
            log_broadcaster: None,
            log_level_handle: None,
            extension_manager: None,
            tool_registry: None,
            store: None,
            job_manager: None,
            external_worker_manager: None,
            prompt_queue: None,
            scheduler: None,
            owner_id: owner_id.clone(),
            default_sender_id: owner_id,
            shutdown_tx: tokio::sync::RwLock::new(None),
            ws_tracker: Some(Arc::new(ws::WsConnectionTracker::new())),
            llm_provider: None,
            skill_registry: None,
            skill_catalog: None,
            chat_rate_limiter: per_user_rate_limiter(CHAT_RATE_LIMIT),
            oauth_rate_limiter: server::RateLimiter::new(10, 60),
            webhook_rate_limiter: server::RateLimiter::new(10, 60),
            registry_entries: Vec::new(),
            cost_guard: None,
            routine_engine: Arc::new(tokio::sync::RwLock::new(None)),
            startup_time: std::time::Instant::now(),
            active_config: server::ActiveConfigSnapshot::default(),
            secrets_store: None,
            db_auth: None,
            channel_manager: None,
            ws_ping_interval_secs: config.ws_ping_interval_secs,
            ws_idle_timeout_secs: config.ws_idle_timeout_secs,
            voice: None,
            voice_rate_limiter: per_user_rate_limiter(VOICE_RATE_LIMIT),
            context_manager: None,
        });

        Self {
            config,
            state,
            auth,
        }
    }

    /// Helper to rebuild state, copying existing fields and applying a mutation.
    fn rebuild_state(&mut self, mutate: impl FnOnce(&mut GatewayState)) {
        let mut new_state = GatewayState {
            msg_tx: tokio::sync::RwLock::new(None),
            // Preserve the existing broadcast channel so sender handles remain valid.
            sse: Arc::new(SseManager::from_sender(self.state.sse.sender())),
            workspace: self.state.workspace.clone(),
            workspace_pool: self.state.workspace_pool.clone(),
            session_manager: self.state.session_manager.clone(),
            log_broadcaster: self.state.log_broadcaster.clone(),
            log_level_handle: self.state.log_level_handle.clone(),
            extension_manager: self.state.extension_manager.clone(),
            tool_registry: self.state.tool_registry.clone(),
            store: self.state.store.clone(),
            job_manager: self.state.job_manager.clone(),
            external_worker_manager: self.state.external_worker_manager.clone(),
            prompt_queue: self.state.prompt_queue.clone(),
            scheduler: self.state.scheduler.clone(),
            owner_id: self.state.owner_id.clone(),
            default_sender_id: self.state.default_sender_id.clone(),
            shutdown_tx: tokio::sync::RwLock::new(None),
            ws_tracker: self.state.ws_tracker.clone(),
            llm_provider: self.state.llm_provider.clone(),
            skill_registry: self.state.skill_registry.clone(),
            skill_catalog: self.state.skill_catalog.clone(),
            chat_rate_limiter: per_user_rate_limiter(CHAT_RATE_LIMIT),
            oauth_rate_limiter: server::RateLimiter::new(10, 60),
            webhook_rate_limiter: server::RateLimiter::new(10, 60),
            registry_entries: self.state.registry_entries.clone(),
            cost_guard: self.state.cost_guard.clone(),
            routine_engine: Arc::clone(&self.state.routine_engine),
            startup_time: self.state.startup_time,
            active_config: self.state.active_config.clone(),
            secrets_store: self.state.secrets_store.clone(),
            db_auth: self.state.db_auth.clone(),
            channel_manager: self.state.channel_manager.clone(),
            ws_ping_interval_secs: self.state.ws_ping_interval_secs,
            ws_idle_timeout_secs: self.state.ws_idle_timeout_secs,
            voice: self.state.voice.clone(),
            voice_rate_limiter: per_user_rate_limiter(VOICE_RATE_LIMIT),
            context_manager: self.state.context_manager.clone(),
        };
        mutate(&mut new_state);
        self.state = Arc::new(new_state);
    }

    /// Inject the workspace reference for the memory API.
    pub fn with_workspace(mut self, workspace: Arc<Workspace>) -> Self {
        self.rebuild_state(|s| s.workspace = Some(workspace));
        self
    }

    /// Inject the session manager for thread/session info.
    pub fn with_session_manager(mut self, sm: Arc<SessionManager>) -> Self {
        self.rebuild_state(|s| s.session_manager = Some(sm));
        self
    }

    /// Inject the log broadcaster for the logs SSE endpoint.
    pub fn with_log_broadcaster(mut self, lb: Arc<LogBroadcaster>) -> Self {
        self.rebuild_state(|s| s.log_broadcaster = Some(lb));
        self
    }

    /// Inject the log level handle for runtime log level control.
    pub fn with_log_level_handle(mut self, h: Arc<LogLevelHandle>) -> Self {
        self.rebuild_state(|s| s.log_level_handle = Some(h));
        self
    }

    /// Inject the extension manager for the extensions API.
    pub fn with_extension_manager(mut self, em: Arc<ExtensionManager>) -> Self {
        self.rebuild_state(|s| s.extension_manager = Some(em));
        self
    }

    /// Inject the tool registry for the extensions API.
    pub fn with_tool_registry(mut self, tr: Arc<ToolRegistry>) -> Self {
        self.rebuild_state(|s| s.tool_registry = Some(tr));
        self
    }

    /// Inject the database store for sandbox job persistence.
    pub fn with_store(mut self, store: Arc<dyn Database>) -> Self {
        self.rebuild_state(|s| s.store = Some(store));
        self
    }

    /// Enable DB-backed token authentication alongside env-var tokens.
    pub fn with_db_auth(mut self, store: Arc<dyn Database>) -> Self {
        let authenticator = DbAuthenticator::new(store);
        // Share the same DbAuthenticator (and its cache) between the auth
        // middleware and GatewayState so handlers can invalidate the cache
        // on security-critical actions (suspend, role change, token revoke).
        self.rebuild_state(|s| s.db_auth = Some(Arc::new(authenticator.clone())));
        self.auth.db_auth = Some(authenticator);
        self
    }

    /// Inject the container job manager for sandbox operations.
    pub fn with_job_manager(mut self, jm: Arc<ContainerJobManager>) -> Self {
        self.rebuild_state(|s| s.job_manager = Some(jm));
        self
    }

    /// Inject the external worker manager for job cancellation.
    pub fn with_external_worker_manager(
        mut self,
        manager: Arc<crate::orchestrator::ExternalWorkerManager>,
    ) -> Self {
        self.rebuild_state(|state| state.external_worker_manager = Some(manager));
        self
    }

    /// Inject the job context manager so the gateway can surface live runtime
    /// state (`stuck`, `repair_attempts`) not stored on the sandbox record
    /// (WI#475).
    pub fn with_context_manager(mut self, cm: Arc<crate::context::ContextManager>) -> Self {
        self.rebuild_state(|state| state.context_manager = Some(cm));
        self
    }

    /// Inject the prompt queue for Claude Code follow-up prompts.
    pub fn with_prompt_queue(
        mut self,
        pq: Arc<
            tokio::sync::Mutex<
                std::collections::HashMap<
                    uuid::Uuid,
                    std::collections::VecDeque<crate::orchestrator::api::PendingPrompt>,
                >,
            >,
        >,
    ) -> Self {
        self.rebuild_state(|s| s.prompt_queue = Some(pq));
        self
    }

    /// Inject the scheduler for sending follow-up messages to agent jobs.
    pub fn with_scheduler(mut self, slot: crate::tools::builtin::SchedulerSlot) -> Self {
        self.rebuild_state(|s| s.scheduler = Some(slot));
        self
    }

    /// Inject the skill registry for skill management API.
    pub fn with_skill_registry(mut self, sr: Arc<std::sync::RwLock<SkillRegistry>>) -> Self {
        self.rebuild_state(|s| s.skill_registry = Some(sr));
        self
    }

    /// Inject the skill catalog for skill search API.
    pub fn with_skill_catalog(mut self, sc: Arc<SkillCatalog>) -> Self {
        self.rebuild_state(|s| s.skill_catalog = Some(sc));
        self
    }

    /// Inject the LLM provider for OpenAI-compatible API proxy.
    pub fn with_llm_provider(mut self, llm: Arc<dyn crate::llm::LlmProvider>) -> Self {
        self.rebuild_state(|s| s.llm_provider = Some(llm));
        self
    }

    /// Inject the desktop-facing voice API proxy (WI#131). The sidecar
    /// credential is held server-side; clients authenticate with their
    /// normal gateway bearer token.
    pub fn with_voice_api(mut self, voice: Arc<self::voice::VoiceApi>) -> Self {
        self.rebuild_state(|s| s.voice = Some(voice));
        self
    }

    /// Inject registry catalog entries for the available extensions API.
    pub fn with_registry_entries(mut self, entries: Vec<crate::extensions::RegistryEntry>) -> Self {
        self.rebuild_state(|s| s.registry_entries = entries);
        self
    }

    /// Inject the cost guard for token/cost tracking in the status popover.
    pub fn with_cost_guard(mut self, cg: Arc<crate::agent::cost_guard::CostGuard>) -> Self {
        self.rebuild_state(|s| s.cost_guard = Some(cg));
        self
    }

    /// Inject a shared routine engine slot used by other HTTP ingress paths.
    pub fn with_routine_engine_slot(mut self, slot: server::RoutineEngineSlot) -> Self {
        self.rebuild_state(|s| s.routine_engine = slot);
        self
    }

    /// Inject the active (resolved) configuration snapshot for the status endpoint.
    pub fn with_active_config(mut self, config: server::ActiveConfigSnapshot) -> Self {
        self.rebuild_state(|s| s.active_config = config);
        self
    }

    /// Inject the secrets store for admin secret provisioning.
    pub fn with_secrets_store(
        mut self,
        store: Arc<dyn crate::secrets::SecretsStore + Send + Sync>,
    ) -> Self {
        self.rebuild_state(|s| s.secrets_store = Some(store));
        self
    }

    /// Override the owner_id in the gateway state (used in multi-tenant mode
    /// to scope the gateway to a specific tenant's identity namespace).
    pub fn with_owner_scope(mut self, owner_id: &str) -> Self {
        let owner = owner_id.to_string();
        self.rebuild_state(|s| s.owner_id = owner);
        self
    }

    /// Inject the per-user workspace pool for multi-user mode.
    pub fn with_workspace_pool(mut self, pool: Arc<server::WorkspacePool>) -> Self {
        self.rebuild_state(|s| s.workspace_pool = Some(pool));
        self
    }

    /// Inject the channel manager for health monitoring.
    pub fn with_channel_manager(
        mut self,
        cm: Arc<crate::channels::manager::ChannelManager>,
    ) -> Self {
        self.rebuild_state(|s| s.channel_manager = Some(cm));
        self
    }

    /// Get the first auth token (for printing to console on startup).
    pub fn auth_token(&self) -> &str {
        self.auth.env_auth.first_token().unwrap_or("")
    }

    /// Get a reference to the shared gateway state (for the agent to push SSE events).
    pub fn state(&self) -> &Arc<GatewayState> {
        &self.state
    }
}

#[async_trait]
impl Channel for GatewayChannel {
    fn name(&self) -> &str {
        "gateway"
    }

    async fn start(&self) -> Result<MessageStream, ChannelError> {
        let (tx, rx) = mpsc::channel(256);
        *self.state.msg_tx.write().await = Some(tx);

        let addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| ChannelError::StartupFailed {
                name: "gateway".to_string(),
                reason: format!(
                    "Invalid address '{}:{}': {}",
                    self.config.host, self.config.port, e
                ),
            })?;

        server::start_server_with_mascot(
            addr,
            self.state.clone(),
            self.auth.clone(),
            self.config.mascot_enabled,
        )
        .await?;

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    async fn respond(
        &self,
        msg: &IncomingMessage,
        response: OutgoingResponse,
    ) -> Result<(), ChannelError> {
        let thread_id = match &msg.thread_id {
            Some(tid) => tid.clone(),
            None => {
                return Err(ChannelError::MissingRoutingTarget {
                    name: "gateway".to_string(),
                    reason: "respond() requires a thread_id on the incoming message".to_string(),
                });
            }
        };

        // Emoting-mascot hook: classify roleplay action phrases in the
        // outgoing reply so clients can mirror them in the mascot widget.
        let emotes = emote::emote_names(&response.content);
        // The reply of a message the chat handlers stamped carries its ids.
        self.state.sse.broadcast_correlated(
            Some(&msg.user_id),
            &msg.metadata,
            AppEvent::Response {
                content: response.content,
                thread_id,
                emotes,
            },
        );

        Ok(())
    }

    async fn send_status(
        &self,
        status: StatusUpdate,
        metadata: &serde_json::Value,
    ) -> Result<(), ChannelError> {
        let thread_id = metadata
            .get("thread_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let event = status_sse_event(status, thread_id);

        // Scope events to the user when user_id is available in metadata.
        // When user_id is missing (heartbeat, routines), events go to all
        // subscribers. In multi-tenant mode this leaks status across users.
        let user_id = metadata.get("user_id").and_then(|v| v.as_str());
        if user_id.is_none() {
            tracing::debug!("Status event missing user_id in metadata; broadcasting globally");
        }
        self.state
            .sse
            .broadcast_correlated(user_id, metadata, event);
        Ok(())
    }

    /// `turn_started` / `turn_finished` (TUI design 8.2.4). Only messages the chat
    /// handlers stamped with a `message_id` emit them; internal messages (job monitor,
    /// routines) and WebSocket sends carry none. Broadcasting never blocks, and the
    /// agent loop calls this after `respond()` on the same task, so a reply always
    /// reaches subscribers before its `turn_finished`.
    async fn message_lifecycle(&self, msg: &IncomingMessage, event: MessageLifecycle) {
        let Some(thread_id) = msg.thread_id.clone() else {
            return;
        };
        if Correlation::from_metadata(&msg.metadata).is_none() {
            return;
        }
        let event = match event {
            MessageLifecycle::Started => SseEvent::TurnStarted { thread_id },
            MessageLifecycle::Finished(outcome) => SseEvent::TurnFinished { thread_id, outcome },
        };
        self.state
            .sse
            .broadcast_correlated(Some(&msg.user_id), &msg.metadata, event);
    }

    async fn broadcast(
        &self,
        user_id: &str,
        response: OutgoingResponse,
    ) -> Result<(), ChannelError> {
        let thread_id = match response.thread_id {
            Some(tid) => tid,
            None => {
                return Err(ChannelError::MissingRoutingTarget {
                    name: "gateway".to_string(),
                    reason: "broadcast() requires a thread_id on the response".to_string(),
                });
            }
        };
        // Emoting-mascot hook: see `respond()`.
        let emotes = emote::emote_names(&response.content);
        self.state.sse.broadcast_for_user(
            user_id,
            AppEvent::Response {
                content: response.content,
                thread_id,
                emotes,
            },
        );
        Ok(())
    }

    async fn health_check(&self) -> Result<(), ChannelError> {
        if self.state.msg_tx.read().await.is_some() {
            Ok(())
        } else {
            Err(ChannelError::HealthCheckFailed {
                name: "gateway".to_string(),
                reason: "sender unavailable".to_string(),
            })
        }
    }

    async fn shutdown(&self) -> Result<(), ChannelError> {
        if let Some(tx) = self.state.shutdown_tx.write().await.take() {
            let _ = tx.send(());
        }
        *self.state.msg_tx.write().await = None;
        Ok(())
    }
}

/// Maps a channel status to its gateway SSE event (TUI design 8.3.1).
///
/// The same objects are the progress vocabulary of REPLv3 `Status` frames.
pub(crate) fn status_sse_event(status: StatusUpdate, thread_id: Option<String>) -> SseEvent {
    match status {
        StatusUpdate::Thinking(message) => SseEvent::Thinking { message, thread_id },
        StatusUpdate::ToolStarted { name } => SseEvent::ToolStarted { name, thread_id },
        StatusUpdate::ToolCompleted {
            name,
            success,
            error,
            parameters,
        } => SseEvent::ToolCompleted {
            name,
            success,
            error,
            parameters,
            thread_id,
        },
        StatusUpdate::ToolResult { name, preview } => SseEvent::ToolResult {
            name,
            preview,
            thread_id,
        },
        StatusUpdate::StreamChunk(content) => SseEvent::StreamChunk { content, thread_id },
        StatusUpdate::Status(message) => SseEvent::Status { message, thread_id },
        StatusUpdate::ExternalWaiting { gate_name } => SseEvent::Status {
            message: StatusUpdate::external_waiting_message(&gate_name),
            thread_id,
        },
        StatusUpdate::SkillActivated { skill_names } => SseEvent::Status {
            message: format!("Skills activated: {}", skill_names.join(", ")),
            thread_id,
        },
        StatusUpdate::ApprovalNeeded {
            request_id,
            tool_name,
            description,
            parameters,
            allow_always,
        } => SseEvent::ApprovalNeeded {
            request_id,
            tool_name,
            description,
            parameters: serde_json::to_string_pretty(&parameters)
                .unwrap_or_else(|_| parameters.to_string()),
            thread_id,
            allow_always,
        },
        StatusUpdate::ImageGenerated { data_url, path } => SseEvent::ImageGenerated {
            data_url,
            path,
            thread_id,
        },
        StatusUpdate::Suggestions { suggestions } => SseEvent::Suggestions {
            suggestions,
            thread_id,
        },
        StatusUpdate::ReasoningUpdate {
            narrative,
            decisions,
        } => SseEvent::ReasoningUpdate {
            narrative,
            decisions: decisions.into_iter().map(ToolDecisionDto::from).collect(),
            thread_id,
        },
        StatusUpdate::ReasoningStream(narrative) => SseEvent::ReasoningUpdate {
            narrative,
            decisions: Vec::new(),
            thread_id,
        },
        StatusUpdate::TurnCost {
            input_tokens,
            output_tokens,
            cost_usd,
        } => SseEvent::TurnCost {
            input_tokens,
            output_tokens,
            cost_usd,
            thread_id,
        },
        unthreaded @ (StatusUpdate::JobStarted { .. }
        | StatusUpdate::AuthRequired { .. }
        | StatusUpdate::AuthCompleted { .. }) => unthreaded_status_event(unthreaded),
    }
}

/// The job and credential statuses of [`status_sse_event`]: their events carry no
/// `thread_id`.
fn unthreaded_status_event(status: StatusUpdate) -> SseEvent {
    match status {
        StatusUpdate::JobStarted {
            job_id,
            title,
            browse_url,
        } => SseEvent::JobStarted {
            job_id,
            title,
            browse_url,
        },
        StatusUpdate::AuthRequired {
            extension_name,
            instructions,
            auth_url,
            setup_url,
        } => SseEvent::AuthRequired {
            extension_name,
            instructions,
            auth_url,
            setup_url,
        },
        StatusUpdate::AuthCompleted {
            extension_name,
            success,
            message,
        } => SseEvent::AuthCompleted {
            extension_name,
            success,
            message,
        },
        // `status_sse_event` maps every other status itself and never passes it here;
        // an empty `status` is ignored by clients, as for unmapped common events.
        _ => SseEvent::Status {
            message: String::new(),
            thread_id: None,
        },
    }
}
