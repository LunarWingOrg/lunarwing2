//! Axum HTTP server for the web gateway.
//!
//! Handles all API routes: chat, memory, jobs, health, and static file serving.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, WebSocketUpgrade},
    http::{StatusCode, header},
    middleware,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use lunarwing_common::client_caps::{
    FEATURE_ATTACHMENTS, FEATURE_CLIENT_TOKENS, FEATURE_STATUS, FEATURE_STREAM, FEATURE_TRANSCRIBE,
    FEATURES_CANONICAL, is_valid_client_id,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowHeaders, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use uuid::Uuid;

use crate::agent::SessionManager;
use crate::bootstrap::lunarwing_base_dir;
use crate::channels::IncomingMessage;
use crate::channels::image_mime::{
    GATEWAY_IMAGE_LIMITS, IMAGE_MIME_ALLOWLIST, ImageInput, ImageRejection, decode_images,
};
use crate::channels::web::auth::{AuthenticatedUser, UserIdentity, auth_middleware};
use crate::channels::web::handlers::cron_workers::{
    cron_worker_control_handler, cron_worker_status_handler,
};
use crate::channels::web::handlers::jobs::{
    job_files_list_handler, job_files_read_handler, jobs_cancel_handler, jobs_detail_handler,
    jobs_events_handler, jobs_list_handler, jobs_prompt_handler, jobs_restart_handler,
    jobs_summary_handler,
};
use crate::channels::web::handlers::memory::{
    memory_list_handler, memory_read_handler, memory_search_handler, memory_tree_handler,
    memory_write_handler,
};
use crate::channels::web::handlers::missions::{
    missions_detail_handler, missions_list_handler, missions_summary_handler,
};
use crate::channels::web::handlers::routines::{
    routines_delete_handler, routines_detail_handler, routines_list_handler,
    routines_summary_handler, routines_toggle_handler, routines_trigger_handler,
};
use crate::channels::web::handlers::skills::{
    skill_patch_approve_handler, skill_patch_reject_handler, skill_patches_list_handler,
    skill_proposal_approve_handler, skill_proposal_reject_handler, skill_proposals_list_handler,
    skill_publish_handler, skills_install_handler, skills_list_handler, skills_remove_handler,
    skills_search_handler,
};
use crate::channels::web::log_layer::LogBroadcaster;
use crate::channels::web::sse::SseManager;
use crate::channels::web::types::*;
use crate::channels::web::util::{
    build_turns_from_db_messages, reasoning_summary, truncate_preview,
};
use crate::channels::web::voice::{
    AUDIO_FORMATS, MAX_AUDIO_B64_LEN, VOICE_PROVIDER, voice_status_handler,
    voice_synthesize_handler, voice_transcribe_handler,
};
use crate::channels::web::{CHAT_RATE_LIMIT, VOICE_RATE_LIMIT};
use crate::db::Database;
use crate::extensions::ExtensionManager;
use crate::orchestrator::job_manager::ContainerJobManager;
use crate::tools::ToolRegistry;
use crate::workspace::Workspace;

/// Shared prompt queue: maps job IDs to pending follow-up prompts for Claude Code bridges.
pub type PromptQueue = Arc<
    tokio::sync::Mutex<
        std::collections::HashMap<
            uuid::Uuid,
            std::collections::VecDeque<crate::orchestrator::api::PendingPrompt>,
        >,
    >,
>;

/// Slot for the routine engine, filled at runtime after the agent starts.
pub type RoutineEngineSlot =
    Arc<tokio::sync::RwLock<Option<Arc<crate::agent::routine_engine::RoutineEngine>>>>;

/// Global request body limit (image uploads ride in `POST /api/chat/send`).
const MAX_REQUEST_BODY_BYTES: usize = 10 * 1024 * 1024;
/// Largest chat message the agent's input validator accepts (`lunarwing_safety`
/// `Validator` default, applied to every channel); advertised to clients.
const MAX_CHAT_CONTENT_BYTES: u64 = 100_000;
/// Version of the TUI chat contract served by `GET /api/chat/capabilities`.
const CHAT_API_VERSION: u32 = 1;

fn redact_oauth_state_for_logs(state: &str) -> String {
    let digest = Sha256::digest(state.as_bytes());
    let mut short_hash = String::with_capacity(12);
    for byte in &digest[..6] {
        use std::fmt::Write as _;
        let _ = write!(&mut short_hash, "{byte:02x}");
    }
    format!("sha256:{short_hash}:len={}", state.len())
}

/// Simple sliding-window rate limiter.
///
/// Tracks the number of requests in the current window. Resets when the window expires.
pub struct RateLimiter {
    /// Requests remaining in the current window.
    remaining: AtomicU64,
    /// Epoch second when the current window started.
    window_start: AtomicU64,
    /// Maximum requests per window.
    max_requests: u64,
    /// Window duration in seconds.
    window_secs: u64,
}

impl RateLimiter {
    pub fn new(max_requests: u64, window_secs: u64) -> Self {
        Self {
            remaining: AtomicU64::new(max_requests),
            window_start: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            ),
            max_requests,
            window_secs,
        }
    }

    /// Try to consume one request. Returns `true` if allowed, `false` if rate limited.
    ///
    /// Note: There is a benign TOCTOU race between checking `window_start` and
    /// resetting it — two concurrent threads may both see an expired window
    /// and reset it, granting a few extra requests at the window boundary.
    /// This is acceptable for chat rate limiting where approximate enforcement
    /// is sufficient, and avoids the cost of a Mutex.
    pub fn check(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let window = self.window_start.load(Ordering::Relaxed);
        if now.saturating_sub(window) >= self.window_secs {
            // Window expired, reset
            self.window_start.store(now, Ordering::Relaxed);
            self.remaining
                .store(self.max_requests - 1, Ordering::Relaxed);
            return true;
        }

        // Try to decrement remaining
        loop {
            let current = self.remaining.load(Ordering::Relaxed);
            if current == 0 {
                return false;
            }
            if self
                .remaining
                .compare_exchange_weak(current, current - 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }
}

/// Snapshot of the active (resolved) configuration exposed to the frontend.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ActiveConfigSnapshot {
    pub llm_backend: String,
    pub llm_model: String,
    pub enabled_channels: Vec<String>,
    pub skills_enabled: bool,
    pub skills_max_active: usize,
    pub skills_max_context_tokens: usize,
}

/// Per-user rate limiter that maintains a separate sliding window per user_id.
///
/// Prevents one user from exhausting the rate limit for all users in multi-tenant mode.
pub struct PerUserRateLimiter {
    limiters: std::sync::RwLock<std::collections::HashMap<String, RateLimiter>>,
    max_requests: u64,
    window_secs: u64,
}

impl PerUserRateLimiter {
    pub fn new(max_requests: u64, window_secs: u64) -> Self {
        Self {
            limiters: std::sync::RwLock::new(std::collections::HashMap::new()),
            max_requests,
            window_secs,
        }
    }

    /// Try to consume one request for the given user. Returns `true` if allowed.
    pub fn check(&self, user_id: &str) -> bool {
        // Fast path: check existing limiter under read lock.
        // On lock poisoning (another thread panicked while holding the lock),
        // allow the request rather than crashing the server.
        {
            let map = match self.limiters.read() {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("PerUserRateLimiter read lock poisoned; recovering");
                    e.into_inner()
                }
            };
            if let Some(limiter) = map.get(user_id) {
                return limiter.check();
            }
        }
        // Slow path: create limiter under write lock.
        let mut map = match self.limiters.write() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("PerUserRateLimiter write lock poisoned; recovering");
                e.into_inner()
            }
        };
        let limiter = map
            .entry(user_id.to_string())
            .or_insert_with(|| RateLimiter::new(self.max_requests, self.window_secs));
        limiter.check()
    }
}

/// Per-user workspace pool: lazily creates and caches workspaces keyed by user_id.
///
/// In single-user mode, exactly one workspace is cached. In multi-user mode,
/// each authenticated user gets their own workspace with appropriate scopes,
/// search config, memory layers, and embedding cache settings.
///
/// Also implements [`WorkspaceResolver`] so it can be shared with memory tools,
/// avoiding a separate `PerUserWorkspaceResolver` with duplicated logic.
pub struct WorkspacePool {
    db: Arc<dyn Database>,
    embeddings: Option<Arc<dyn crate::workspace::EmbeddingProvider>>,
    embedding_cache_config: crate::workspace::EmbeddingCacheConfig,
    search_config: crate::config::WorkspaceSearchConfig,
    workspace_config: crate::config::WorkspaceConfig,
    cache: tokio::sync::RwLock<std::collections::HashMap<String, Arc<Workspace>>>,
}

impl WorkspacePool {
    pub fn new(
        db: Arc<dyn Database>,
        embeddings: Option<Arc<dyn crate::workspace::EmbeddingProvider>>,
        embedding_cache_config: crate::workspace::EmbeddingCacheConfig,
        search_config: crate::config::WorkspaceSearchConfig,
        workspace_config: crate::config::WorkspaceConfig,
    ) -> Self {
        Self {
            db,
            embeddings,
            embedding_cache_config,
            search_config,
            workspace_config,
            cache: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Build a workspace for a user, applying search config, embeddings,
    /// global read scopes, and memory layers.
    fn build_workspace(&self, user_id: &str) -> Workspace {
        let mut ws = Workspace::new_with_db(user_id, Arc::clone(&self.db))
            .with_search_config(&self.search_config);

        if let Some(ref emb) = self.embeddings {
            ws = ws.with_embeddings_cached(Arc::clone(emb), self.embedding_cache_config.clone());
        }

        if !self.workspace_config.read_scopes.is_empty() {
            ws = ws.with_additional_read_scopes(self.workspace_config.read_scopes.clone());
        }

        ws = ws.with_memory_layers(self.workspace_config.memory_layers.clone());
        ws
    }

    /// Get or create a workspace for the given user identity.
    ///
    /// Applies search config, memory layers, embedding cache, and read scopes
    /// (both from global config and from the token's `workspace_read_scopes`).
    pub async fn get_or_create(&self, identity: &UserIdentity) -> Arc<Workspace> {
        // Fast path: check read lock
        {
            let cache = self.cache.read().await;
            if let Some(ws) = cache.get(&identity.user_id) {
                return Arc::clone(ws);
            }
        }

        // Slow path: create workspace under write lock
        let mut cache = self.cache.write().await;
        // Double-check after acquiring write lock
        if let Some(ws) = cache.get(&identity.user_id) {
            return Arc::clone(ws);
        }

        let mut ws = self.build_workspace(&identity.user_id);

        // Apply per-token read scopes from identity.
        if !identity.workspace_read_scopes.is_empty() {
            ws = ws.with_additional_read_scopes(identity.workspace_read_scopes.clone());
        }

        let ws = Arc::new(ws);
        cache.insert(identity.user_id.clone(), Arc::clone(&ws));
        ws
    }
}

#[async_trait::async_trait]
impl crate::tools::builtin::memory::WorkspaceResolver for WorkspacePool {
    async fn resolve(&self, user_id: &str) -> Arc<Workspace> {
        // Fast path: check read lock
        {
            let cache = self.cache.read().await;
            if let Some(ws) = cache.get(user_id) {
                return Arc::clone(ws);
            }
        }

        // Slow path: create workspace under write lock
        let mut cache = self.cache.write().await;
        if let Some(ws) = cache.get(user_id) {
            return Arc::clone(ws);
        }

        let ws = Arc::new(self.build_workspace(user_id));
        cache.insert(user_id.to_string(), Arc::clone(&ws));
        tracing::debug!(user_id = user_id, "Created per-user workspace");
        ws
    }
}

/// Shared state for all gateway handlers.
pub struct GatewayState {
    /// Channel to send messages to the agent loop.
    pub msg_tx: tokio::sync::RwLock<Option<mpsc::Sender<IncomingMessage>>>,
    /// SSE broadcast manager (Arc-wrapped so extension manager can hold a reference).
    pub sse: Arc<SseManager>,
    /// Workspace for memory API (single-user fallback).
    pub workspace: Option<Arc<Workspace>>,
    /// Per-user workspace pool for multi-user mode.
    pub workspace_pool: Option<Arc<WorkspacePool>>,
    /// Session manager for thread info.
    pub session_manager: Option<Arc<SessionManager>>,
    /// Log broadcaster for the logs SSE endpoint.
    pub log_broadcaster: Option<Arc<LogBroadcaster>>,
    /// Handle for changing the tracing log level at runtime.
    pub log_level_handle: Option<Arc<crate::channels::web::log_layer::LogLevelHandle>>,
    /// Extension manager for extension management API.
    pub extension_manager: Option<Arc<ExtensionManager>>,
    /// Tool registry for listing registered tools.
    pub tool_registry: Option<Arc<ToolRegistry>>,
    /// Database store for sandbox job persistence.
    pub store: Option<Arc<dyn Database>>,
    /// Container job manager for sandbox operations.
    pub job_manager: Option<Arc<ContainerJobManager>>,
    /// External worker manager for cancelling persistent worker jobs.
    pub external_worker_manager: Option<Arc<crate::orchestrator::ExternalWorkerManager>>,
    /// Prompt queue for Claude Code follow-up prompts.
    pub prompt_queue: Option<PromptQueue>,
    /// Durable owner scope for persistence and unauthenticated callback flows.
    pub owner_id: String,
    /// Default sender/routing identity for gateway-originated messages.
    pub default_sender_id: String,
    /// Shutdown signal sender.
    pub shutdown_tx: tokio::sync::RwLock<Option<oneshot::Sender<()>>>,
    /// WebSocket connection tracker.
    pub ws_tracker: Option<Arc<crate::channels::web::ws::WsConnectionTracker>>,
    /// LLM provider for OpenAI-compatible API proxy.
    pub llm_provider: Option<Arc<dyn crate::llm::LlmProvider>>,
    /// Skill registry for skill management API.
    pub skill_registry: Option<Arc<std::sync::RwLock<crate::skills::SkillRegistry>>>,
    /// Skill catalog for searching the ClawHub registry.
    pub skill_catalog: Option<Arc<crate::skills::catalog::SkillCatalog>>,
    /// Scheduler for sending follow-up messages to running agent jobs.
    pub scheduler: Option<crate::tools::builtin::SchedulerSlot>,
    /// Per-user rate limiter for chat endpoints (30 messages per 60 seconds per user).
    pub chat_rate_limiter: PerUserRateLimiter,
    /// Rate limiter for OAuth callback endpoints (10 requests per 60 seconds).
    pub oauth_rate_limiter: RateLimiter,
    /// Rate limiter for webhook trigger endpoints (10 requests per 60 seconds).
    pub webhook_rate_limiter: RateLimiter,
    /// Registry catalog entries for the available extensions API.
    /// Populated at startup from `registry/` manifests, independent of extension manager.
    pub registry_entries: Vec<crate::extensions::RegistryEntry>,
    /// Cost guard for token/cost tracking.
    pub cost_guard: Option<Arc<crate::agent::cost_guard::CostGuard>>,
    /// Routine engine slot for manual routine triggering (filled at runtime).
    pub routine_engine: RoutineEngineSlot,
    /// Server startup time for uptime calculation.
    pub startup_time: std::time::Instant,
    /// Snapshot of active (resolved) configuration for the frontend.
    pub active_config: ActiveConfigSnapshot,
    /// Secrets store for the secrets management API.
    pub secrets_store: Option<Arc<dyn crate::secrets::SecretsStore + Send + Sync>>,
    /// DB-backed token authenticator (optional).
    pub db_auth: Option<Arc<crate::channels::web::auth::DbAuthenticator>>,
    /// Channel manager for health checks.
    pub channel_manager: Option<Arc<crate::channels::manager::ChannelManager>>,
    /// WebSocket server-side ping interval in seconds.
    pub ws_ping_interval_secs: u64,
    /// WebSocket idle timeout in seconds.
    pub ws_idle_timeout_secs: u64,
    /// Desktop-facing voice API proxy (present when LunarVoice credential configured).
    pub voice: Option<Arc<crate::channels::web::voice::VoiceApi>>,
    /// Per-user rate limiter for voice endpoints (10 requests per 60 seconds).
    pub voice_rate_limiter: PerUserRateLimiter,
    /// Job context manager for live runtime state (`stuck`, `repair_attempts`)
    /// that is not persisted on the sandbox-job record (WI#475).
    pub context_manager: Option<Arc<crate::context::ContextManager>>,
}

/// Start the gateway HTTP server.
///
/// Returns the actual bound `SocketAddr` (useful when binding to port 0).
pub async fn start_server(
    addr: SocketAddr,
    state: Arc<GatewayState>,
    auth: crate::channels::web::auth::CombinedAuthState,
) -> Result<SocketAddr, crate::error::ChannelError> {
    start_server_with_mascot(addr, state, auth, false).await
}

pub(super) async fn start_server_with_mascot(
    addr: SocketAddr,
    state: Arc<GatewayState>,
    auth: crate::channels::web::auth::CombinedAuthState,
    mascot_enabled: bool,
) -> Result<SocketAddr, crate::error::ChannelError> {
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        crate::error::ChannelError::StartupFailed {
            name: "gateway".to_string(),
            reason: format!("Failed to bind to {}: {}", addr, e),
        }
    })?;
    let bound_addr =
        listener
            .local_addr()
            .map_err(|e| crate::error::ChannelError::StartupFailed {
                name: "gateway".to_string(),
                reason: format!("Failed to get local addr: {}", e),
            })?;

    // Public routes (no auth)
    let public = Router::new()
        .route("/api/health", get(health_handler))
        .route("/oauth/callback", get(oauth_callback_handler))
        .route(
            "/api/webhooks/{path}",
            post(crate::channels::web::handlers::webhooks::webhook_trigger_handler),
        )
        .route(
            "/api/missions/webhook/{path}",
            post(crate::channels::web::handlers::webhooks::mission_webhook_handler),
        );

    // Protected routes (require auth)
    let auth_state = auth;
    let protected = Router::new()
        // Chat
        .route("/api/chat/send", post(chat_send_handler))
        .route("/api/chat/capabilities", get(chat_capabilities_handler))
        .route("/api/chat/approval", post(chat_approval_handler))
        .route("/api/chat/pending-gates", get(chat_pending_gates_handler))
        .route("/api/chat/auth-token", post(chat_auth_token_handler))
        .route("/api/chat/auth-cancel", post(chat_auth_cancel_handler))
        .route("/api/chat/events", get(chat_events_handler))
        .route("/api/chat/ws", get(chat_ws_handler))
        .route("/api/chat/history", get(chat_history_handler))
        .route("/api/chat/threads", get(chat_threads_handler))
        .route("/api/chat/thread/new", post(chat_new_thread_handler))
        // Memory
        .route("/api/memory/tree", get(memory_tree_handler))
        .route("/api/memory/list", get(memory_list_handler))
        .route("/api/memory/read", get(memory_read_handler))
        .route("/api/memory/write", post(memory_write_handler))
        .route("/api/memory/search", post(memory_search_handler))
        .merge(crate::channels::web::handlers::prompts::routes())
        // Jobs
        .route("/api/jobs", get(jobs_list_handler))
        .route(
            "/api/workers/{worker}/cron",
            get(cron_worker_status_handler).post(cron_worker_control_handler),
        )
        .route("/api/jobs/summary", get(jobs_summary_handler))
        .route("/api/jobs/{id}", get(jobs_detail_handler))
        .route("/api/jobs/{id}/cancel", post(jobs_cancel_handler))
        .route("/api/jobs/{id}/restart", post(jobs_restart_handler))
        .route("/api/jobs/{id}/prompt", post(jobs_prompt_handler))
        .route("/api/jobs/{id}/events", get(jobs_events_handler))
        .route("/api/jobs/{id}/files/list", get(job_files_list_handler))
        .route("/api/jobs/{id}/files/read", get(job_files_read_handler))
        // Missions (read-only observability surface, work item #217)
        .route("/api/missions", get(missions_list_handler))
        .route("/api/missions/summary", get(missions_summary_handler))
        .route("/api/missions/{id}", get(missions_detail_handler))
        // Logs
        .route("/api/logs/events", get(logs_events_handler))
        .route("/api/logs/level", get(logs_level_get_handler))
        .route(
            "/api/logs/level",
            axum::routing::put(logs_level_set_handler),
        )
        .route("/api/logs/download", get(logs_download_handler))
        // Extensions
        .route("/api/extensions", get(extensions_list_handler))
        .route("/api/extensions/tools", get(extensions_tools_handler))
        .route("/api/extensions/registry", get(extensions_registry_handler))
        .route("/api/extensions/install", post(extensions_install_handler))
        .route(
            "/api/extensions/{name}/activate",
            post(extensions_activate_handler),
        )
        .route(
            "/api/extensions/{name}/deactivate",
            post(extensions_deactivate_handler),
        )
        .route(
            "/api/extensions/{name}/toggle",
            post(extensions_toggle_handler),
        )
        .route(
            "/api/extensions/{name}/update",
            post(extensions_update_handler),
        )
        .route(
            "/api/extensions/{name}/remove",
            post(extensions_remove_handler),
        )
        .route(
            "/api/extensions/{name}/setup",
            get(extensions_setup_handler).post(extensions_setup_submit_handler),
        )
        // Pairing
        .route("/api/pairing/{channel}", get(pairing_list_handler))
        .route(
            "/api/pairing/{channel}/approve",
            post(pairing_approve_handler),
        )
        // Routines
        .route("/api/routines", get(routines_list_handler))
        .route("/api/routines/summary", get(routines_summary_handler))
        .route("/api/routines/{id}", get(routines_detail_handler))
        .route("/api/routines/{id}/trigger", post(routines_trigger_handler))
        .route("/api/routines/{id}/toggle", post(routines_toggle_handler))
        .route(
            "/api/routines/{id}",
            axum::routing::delete(routines_delete_handler),
        )
        .route("/api/routines/{id}/runs", get(routines_runs_handler))
        // Skills
        .route("/api/skills", get(skills_list_handler))
        .route("/api/skills/search", post(skills_search_handler))
        .route("/api/skills/install", post(skills_install_handler))
        // B-1: self-improving skills — pending-patch approval surface.
        // Static `patches` segment precedes the `{name}` delete route.
        .route("/api/skills/patches", get(skill_patches_list_handler))
        .route(
            "/api/skills/patches/{doc_id}/approve",
            post(skill_patch_approve_handler),
        )
        .route(
            "/api/skills/patches/{doc_id}/reject",
            post(skill_patch_reject_handler),
        )
        // B-1/B-2/B-3: unified skill proposals surface (kind-discriminated).
        // The patch routes above remain as back-compat aliases.
        .route("/api/skills/proposals", get(skill_proposals_list_handler))
        .route(
            "/api/skills/proposals/{doc_id}/approve",
            post(skill_proposal_approve_handler),
        )
        .route(
            "/api/skills/proposals/{doc_id}/reject",
            post(skill_proposal_reject_handler),
        )
        // B-3: cross-agent skill sharing — publish to the registry.
        // Static `publish` segment precedes the `{name}` delete route.
        .route("/api/skills/publish/{doc_id}", post(skill_publish_handler))
        .route(
            "/api/skills/{name}",
            axum::routing::delete(skills_remove_handler),
        )
        // Settings
        .route("/api/settings", get(settings_list_handler))
        .route("/api/settings/export", get(settings_export_handler))
        .route("/api/settings/import", post(settings_import_handler))
        .route("/api/settings/{key}", get(settings_get_handler))
        .route(
            "/api/settings/{key}",
            axum::routing::put(settings_set_handler),
        )
        .route(
            "/api/settings/{key}",
            axum::routing::delete(settings_delete_handler),
        )
        // Gateway control plane
        .route("/api/gateway/status", get(gateway_status_handler))
        // Desktop-facing voice API (WI#131): loopback LunarVoice proxy.
        .route("/api/voice/status", get(voice_status_handler))
        .route(
            "/api/voice/transcribe",
            post(voice_transcribe_handler).layer(DefaultBodyLimit::max(
                crate::channels::web::voice::TRANSCRIBE_BODY_LIMIT,
            )),
        )
        .route(
            "/api/voice/synthesize",
            post(voice_synthesize_handler).layer(DefaultBodyLimit::max(
                crate::channels::web::voice::SYNTHESIZE_BODY_LIMIT,
            )),
        )
        // OpenAI-compatible API
        .route(
            "/v1/chat/completions",
            post(super::openai_compat::chat_completions_handler),
        )
        .route("/v1/models", get(super::openai_compat::models_handler))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ));

    // Static file routes (no auth, served from embedded strings)
    let statics = Router::new()
        .route("/", get(move || index_handler(mascot_enabled)))
        .route("/style.css", get(css_handler))
        .route("/app.js", get(js_handler))
        .route("/prompts.js", get(prompts_js_handler))
        .route("/theme-init.js", get(theme_init_handler))
        .route("/favicon.ico", get(favicon_handler))
        .route("/favicon.svg", get(favicon_svg_handler))
        .route("/logo.svg", get(logo_svg_handler))
        .route(
            "/fonts/hanken-grotesk-latin-wght-normal.woff2",
            get(font_hanken_normal_handler),
        )
        .route(
            "/fonts/hanken-grotesk-latin-wght-italic.woff2",
            get(font_hanken_italic_handler),
        )
        .route(
            "/fonts/fira-code-latin-wght-normal.woff2",
            get(font_fira_code_handler),
        )
        .route("/i18n/index.js", get(i18n_index_handler))
        .route("/i18n/en.js", get(i18n_en_handler))
        .route("/i18n/zh-CN.js", get(i18n_zh_handler))
        .route("/i18n-app.js", get(i18n_app_handler))
        .route("/vendor/purify.min.js", get(vendor_purify_handler))
        .route("/vendor/marked.umd.min.js", get(vendor_marked_handler));
    let statics = if mascot_enabled {
        statics
            .route("/mascot.js", get(mascot_js_handler))
            .route("/mascot/{name}", get(mascot_sprite_handler))
    } else {
        statics
    };
    let statics = statics.layer(CompressionLayer::new().gzip(true));

    // Project file serving (behind auth to prevent unauthorized file access).
    let projects = Router::new()
        .route("/projects/{project_id}", get(project_redirect_handler))
        .route("/projects/{project_id}/", get(project_index_handler))
        .route("/projects/{project_id}/{*path}", get(project_file_handler))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ));

    // CORS: restrict to same-origin by default. Only localhost/127.0.0.1
    // origins are allowed, since the gateway is a local-first service.
    let cors = CorsLayer::new()
        .allow_origin([
            format!("http://{}:{}", addr.ip(), addr.port())
                .parse()
                .expect("valid origin"),
            format!("http://localhost:{}", addr.port())
                .parse()
                .expect("valid origin"),
        ])
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
        ])
        .allow_headers(AllowHeaders::list([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
        ]))
        .allow_credentials(true);

    let app = Router::new()
        .merge(public)
        .merge(statics)
        .merge(projects)
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(cors)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            header::HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            header::HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("content-security-policy"),
            header::HeaderValue::from_static(
                "default-src 'self'; \
                 script-src 'self'; \
                 style-src 'self' 'unsafe-inline'; \
                 font-src 'self'; \
                 connect-src 'self'; \
                 img-src 'self' data:; \
                 object-src 'none'; \
                 frame-ancestors 'none'; \
                 base-uri 'self'; \
                 form-action 'self'",
            ),
        ))
        .with_state(state.clone());

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    *state.shutdown_tx.write().await = Some(shutdown_tx);

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
                tracing::debug!("Web gateway shutting down");
            })
            .await
        {
            tracing::error!("Web gateway server error: {}", e);
        }
    });

    // Background cleanup: remove leaked tracker entries for connections that
    // exited without unregistering (e.g. due to a panic in the handler).
    if let Some(ref tracker) = state.ws_tracker {
        let tracker = Arc::clone(tracker);
        let idle_timeout = std::time::Duration::from_secs(state.ws_idle_timeout_secs);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let removed = tracker.cleanup_stale(idle_timeout);
                if removed > 0 {
                    tracing::info!(
                        removed,
                        remaining = tracker.connection_count(),
                        "Cleaned up stale WebSocket tracker entries"
                    );
                }
            }
        });
    }

    Ok(bound_addr)
}

// --- Static file handlers ---

const INDEX_HTML: &str = include_str!("static/index.html");
const MASCOT_SCRIPT_AND_BODY_END: &str = "  <script src=\"/mascot.js\" defer></script>\n</body>";
static MASCOT_INDEX_HTML: LazyLock<String> =
    LazyLock::new(|| INDEX_HTML.replacen("</body>", MASCOT_SCRIPT_AND_BODY_END, 1));

fn index_html(mascot_enabled: bool) -> &'static str {
    if mascot_enabled {
        MASCOT_INDEX_HTML.as_str()
    } else {
        INDEX_HTML
    }
}

async fn index_handler(mascot_enabled: bool) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        index_html(mascot_enabled),
    )
}

async fn css_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("static/style.css"),
    )
}

async fn js_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("static/app.js"),
    )
}

async fn prompts_js_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("static/prompts.js"),
    )
}

async fn theme_init_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("static/theme-init.js"),
    )
}

async fn favicon_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/x-icon"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        include_bytes!("static/favicon.ico").as_slice(),
    )
}

async fn favicon_svg_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        include_str!("static/favicon.svg"),
    )
}

async fn logo_svg_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        include_str!("static/logo.svg"),
    )
}

// Self-hosted fonts (embedded in the binary). woff2 is immutable content, so a
// long cache lifetime is safe.
async fn font_hanken_normal_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        include_bytes!("static/fonts/hanken-grotesk-latin-wght-normal.woff2").as_slice(),
    )
}

async fn font_hanken_italic_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        include_bytes!("static/fonts/hanken-grotesk-latin-wght-italic.woff2").as_slice(),
    )
}

async fn font_fira_code_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        include_bytes!("static/fonts/fira-code-latin-wght-normal.woff2").as_slice(),
    )
}

async fn i18n_index_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("static/i18n/index.js"),
    )
}

async fn i18n_en_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("static/i18n/en.js"),
    )
}

async fn i18n_zh_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("static/i18n/zh-CN.js"),
    )
}

async fn i18n_app_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("static/i18n-app.js"),
    )
}

// Emoting-mascot widget (item #19 in the 2.1.0 goals): the widget JS and
// animated sprite pack are only routed when MASCOT_ENABLED is true.
async fn mascot_js_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("static/mascot.js"),
    )
}

/// Serve one sprite from the mascot pack. The allowlist mirrors
/// [`crate::channels::web::emote::Emote::ALL_SPRITES`]; unknown names get a
/// 404 instead of arbitrary embedded-content access. Long cache lifetime
/// because the files are pinned content of this binary.
async fn mascot_sprite_handler(Path(name): Path<String>) -> impl IntoResponse {
    let bytes: Option<&'static [u8]> = match name.as_str() {
        "lunar_fiery.gif" => Some(include_bytes!("static/mascot/lunar_fiery.gif").as_slice()),
        "lunar_greet.gif" => Some(include_bytes!("static/mascot/lunar_greet.gif").as_slice()),
        "lunar_jump.gif" => Some(include_bytes!("static/mascot/lunar_jump.gif").as_slice()),
        "lunar_love.gif" => Some(include_bytes!("static/mascot/lunar_love.gif").as_slice()),
        "lunar_rage.gif" => Some(include_bytes!("static/mascot/lunar_rage.gif").as_slice()),
        "lunar_sleep.gif" => Some(include_bytes!("static/mascot/lunar_sleep.gif").as_slice()),
        "lunar_sup.gif" => Some(include_bytes!("static/mascot/lunar_sup.gif").as_slice()),
        "lunar_walk.gif" => Some(include_bytes!("static/mascot/lunar_walk.gif").as_slice()),
        _ => None,
    };
    match bytes {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, "image/gif"),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// Vendored frontend libraries (embedded in the binary). Content is pinned to a
// specific upstream release (see static/vendor/README.md), so a long cache
// lifetime is safe.
async fn vendor_purify_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        include_str!("static/vendor/purify.min.js"),
    )
}

async fn vendor_marked_handler() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        include_str!("static/vendor/marked.umd.min.js"),
    )
}

// --- Health ---

async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy",
        channel: "gateway",
    })
}

/// Return an OAuth error landing page response.
fn oauth_error_page(label: &str) -> axum::response::Response {
    let html = crate::cli::oauth_defaults::landing_html(label, false);
    axum::response::Html(html).into_response()
}

/// OAuth callback handler for the web gateway.
///
/// This is a PUBLIC route (no Bearer token required) because OAuth providers
/// redirect the user's browser here. The `state` query parameter correlates
/// the callback with a pending OAuth flow registered by `start_wasm_oauth()`.
///
/// Used on hosted instances where `LUNARWING_OAUTH_CALLBACK_URL` (or legacy
/// `IRONCLAW_OAUTH_CALLBACK_URL`) points to the gateway (e.g.,
/// `https://agent.example.com/oauth/callback`).
/// Local/desktop mode continues to use the TCP listener on port 9876.
async fn oauth_callback_handler(
    State(state): State<Arc<GatewayState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    oauth_callback_impl(
        state.extension_manager.as_ref(),
        state.session_manager.as_ref(),
        &state.owner_id,
        params,
    )
    .await
}

async fn oauth_callback_impl(
    extension_manager: Option<&Arc<ExtensionManager>>,
    session_manager: Option<&Arc<SessionManager>>,
    owner_id: &str,
    params: std::collections::HashMap<String, String>,
) -> axum::response::Response {
    use crate::cli::oauth_defaults;

    // Check for error from OAuth provider (e.g., user denied consent)
    if let Some(error) = params.get("error") {
        let description = params
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| error.clone());
        return oauth_error_page(&description);
    }

    let state_param = match params.get("state") {
        Some(s) if !s.is_empty() => s.clone(),
        _ => {
            return oauth_error_page("LunarWing");
        }
    };

    let code = match params.get("code") {
        Some(c) if !c.is_empty() => c.clone(),
        _ => {
            return oauth_error_page("LunarWing");
        }
    };

    // Look up the pending flow by CSRF state (atomic remove prevents replay)
    let ext_mgr = match extension_manager {
        Some(mgr) => mgr,
        None => {
            return oauth_error_page("LunarWing");
        }
    };

    let decoded_state = match oauth_defaults::decode_hosted_oauth_state(&state_param) {
        Ok(decoded) => decoded,
        Err(error) => {
            let redacted_state = redact_oauth_state_for_logs(&state_param);
            tracing::warn!(
                state = %redacted_state,
                error = %error,
                "OAuth callback received with malformed state"
            );
            clear_session_auth_mode(session_manager, owner_id).await;
            return oauth_error_page("LunarWing");
        }
    };
    let lookup_key = decoded_state.flow_id.clone();

    let flow = ext_mgr
        .pending_oauth_flows()
        .write()
        .await
        .remove(&lookup_key);

    let flow = match flow {
        Some(f) => f,
        None => {
            let redacted_state = redact_oauth_state_for_logs(&state_param);
            let redacted_lookup_key = redact_oauth_state_for_logs(&lookup_key);
            tracing::warn!(
                state = %redacted_state,
                lookup_key = %redacted_lookup_key,
                "OAuth callback received with unknown or expired state"
            );
            return oauth_error_page("LunarWing");
        }
    };

    // Check flow expiry (5 minutes, matching TCP listener timeout)
    if flow.created_at.elapsed() > oauth_defaults::OAUTH_FLOW_EXPIRY {
        tracing::warn!(
            extension = %flow.extension_name,
            "OAuth flow expired"
        );
        // Notify UI so auth card can show error instead of staying stuck
        if let Some(ref sse) = flow.sse_manager {
            sse.broadcast_for_user(
                &flow.user_id,
                SseEvent::AuthCompleted {
                    extension_name: flow.extension_name.clone(),
                    success: false,
                    message: "OAuth flow expired. Please try again.".to_string(),
                },
            );
        }
        clear_session_auth_mode(session_manager, &flow.user_id).await;
        return oauth_error_page(&flow.display_name);
    }

    // Exchange the authorization code for tokens.
    // Use the platform exchange proxy when configured, otherwise call the
    // provider's token URL directly.
    let exchange_proxy_url = oauth_defaults::exchange_proxy_url();

    let result: Result<(), String> = async {
        let token_response = if let Some(proxy_url) = &exchange_proxy_url {
            let gateway_token = flow.gateway_token.as_deref().unwrap_or_default();
            oauth_defaults::exchange_via_proxy(oauth_defaults::ProxyTokenExchangeRequest {
                proxy_url,
                gateway_token,
                token_url: &flow.token_url,
                client_id: &flow.client_id,
                client_secret: flow.client_secret.as_deref(),
                code: &code,
                redirect_uri: &flow.redirect_uri,
                code_verifier: flow.code_verifier.as_deref(),
                access_token_field: &flow.access_token_field,
                extra_token_params: &flow.token_exchange_extra_params,
            })
            .await
            .map_err(|e| e.to_string())?
        } else {
            oauth_defaults::exchange_oauth_code_with_params(
                &flow.token_url,
                &flow.client_id,
                flow.client_secret.as_deref(),
                &code,
                &flow.redirect_uri,
                flow.code_verifier.as_deref(),
                &flow.access_token_field,
                &flow.token_exchange_extra_params,
            )
            .await
            .map_err(|e| e.to_string())?
        };

        // Validate the token before storing (catches wrong account, etc.)
        if let Some(ref validation) = flow.validation_endpoint {
            oauth_defaults::validate_oauth_token(&token_response.access_token, validation)
                .await
                .map_err(|e| e.to_string())?;
        }

        // Store tokens encrypted in the secrets store
        oauth_defaults::store_oauth_tokens(
            flow.secrets.as_ref(),
            &flow.user_id,
            &flow.secret_name,
            flow.provider.as_deref(),
            &token_response.access_token,
            token_response.refresh_token.as_deref(),
            token_response.expires_in,
            &flow.scopes,
        )
        .await
        .map_err(|e| e.to_string())?;

        // Persist the client_id for flows that need it after the session ends
        // (for example DCR-based MCP refresh).
        if let Some(ref client_id_secret) = flow.client_id_secret_name {
            let params = crate::secrets::CreateSecretParams::new(client_id_secret, &flow.client_id)
                .with_provider(flow.provider.as_ref().cloned().unwrap_or_default());
            flow.secrets
                .create(&flow.user_id, params)
                .await
                .map_err(|e| e.to_string())?;
        }

        Ok(())
    }
    .await;

    let (success, message) = match &result {
        Ok(()) => (
            true,
            format!("{} authenticated successfully", flow.display_name),
        ),
        Err(e) => (
            false,
            format!("{} authentication failed: {}", flow.display_name, e),
        ),
    };

    match &result {
        Ok(()) => {
            tracing::info!(
                extension = %flow.extension_name,
                "OAuth completed successfully via gateway callback"
            );
        }
        Err(e) => {
            tracing::warn!(
                extension = %flow.extension_name,
                error = %e,
                "OAuth failed via gateway callback"
            );
        }
    }

    // Clear auth mode regardless of outcome so the next user message goes
    // through to the LLM instead of being intercepted as a token.
    clear_session_auth_mode(session_manager, &flow.user_id).await;

    // After successful OAuth, auto-activate the extension so it moves
    // from "Installed (Authenticate)" → "Active" without a second click.
    // OAuth success is independent of activation — tokens are already stored.
    // Report auth as successful and attempt activation as a bonus step.
    let (final_message, activation_succeeded) = if success {
        match ext_mgr
            .activate_after_auth(&flow.extension_name, &flow.user_id)
            .await
        {
            Ok(Some(result)) => (result.message, true),
            Ok(None) => (
                format!(
                    "{} authenticated successfully and remains deactivated.",
                    flow.display_name
                ),
                false,
            ),
            Err(e) => {
                tracing::warn!(
                    extension = %flow.extension_name,
                    error = %e,
                    "Auto-activation after OAuth failed"
                );
                (
                    format!(
                        "{} authenticated successfully. Activation failed: {}. Try activating manually.",
                        flow.display_name, e
                    ),
                    false,
                )
            }
        }
    } else {
        (message, false)
    };

    if activation_succeeded {
        let resume_result =
            match crate::bridge::resolve_engine_auth_callback(&flow.user_id, &flow.secret_name)
                .await
            {
                Ok(false) if flow.extension_name != flow.secret_name => {
                    crate::bridge::resolve_engine_auth_callback(&flow.user_id, &flow.extension_name)
                        .await
                }
                result => result,
            };
        if let Err(error) = resume_result {
            tracing::warn!(
                extension = %flow.extension_name,
                error = %error,
                "Failed to resume Engine V2 thread after OAuth callback"
            );
        }
    }

    // Broadcast SSE event to notify the web UI
    if let Some(ref sse) = flow.sse_manager {
        sse.broadcast_for_user(
            &flow.user_id,
            SseEvent::AuthCompleted {
                extension_name: flow.extension_name,
                success,
                message: final_message.clone(),
            },
        );
    }

    let html = oauth_defaults::landing_html(&flow.display_name, success);
    axum::response::Html(html).into_response()
}

#[cfg(feature = "integration")]
pub async fn oauth_callback_for_test(
    extension_manager: &Arc<ExtensionManager>,
    params: std::collections::HashMap<String, String>,
) -> axum::response::Response {
    oauth_callback_impl(Some(extension_manager), None, "integration-test", params).await
}

// --- Chat handlers ---

/// Validates and decodes the images of a chat message (TUI design 6.2).
///
/// Any invalid image fails the whole message: 413 for the size limits, 422 for the
/// count, the type and the encoding, with a plain-text reason naming the image. The
/// attachment type is sniffed from the bytes; ids are `web-image-<uuid>`.
pub(crate) fn gateway_image_attachments(
    images: &[ImageData],
) -> Result<Vec<crate::channels::IncomingAttachment>, (StatusCode, String)> {
    let inputs: Vec<ImageInput<'_>> = images
        .iter()
        .map(|image| ImageInput {
            declared_mime: &image.media_type,
            data_b64: &image.data,
            filename: None,
        })
        .collect();
    decode_images(&inputs, GATEWAY_IMAGE_LIMITS, "web-image").map_err(|rejection| {
        // Positions and limits only: client data never reaches the logs.
        tracing::debug!(%rejection, "Rejected chat image upload");
        image_rejection_response(&rejection)
    })
}

/// `image <i> rejected: <reason>`, with 413 for the size limits and 422 otherwise.
fn image_rejection_response(rejection: &ImageRejection) -> (StatusCode, String) {
    let reason = match rejection {
        ImageRejection::TooMany { count, max } => format!(
            "image {} rejected: too many images ({count}, at most {max})",
            max.saturating_add(1)
        ),
        ImageRejection::TooLarge { index, max } => {
            format!("image {index} rejected: larger than {max} bytes")
        }
        ImageRejection::TotalTooLarge { max } => {
            format!("images rejected: the images exceed {max} bytes in total")
        }
        ImageRejection::BadBase64 { index } => format!("image {index} rejected: invalid base64"),
        ImageRejection::Unsupported { index } => format!(
            "image {index} rejected: unsupported image type (allowed: png, jpeg, gif, webp)"
        ),
        ImageRejection::Empty { index } => format!("image {index} rejected: no image data"),
    };
    let status = if rejection.is_size_limit() {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    (status, reason)
}

/// `client_turn_id` must satisfy `is_valid_client_id` when present (TUI design 4.1);
/// an invalid id is rejected, never rewritten.
fn validate_client_turn_id(id: Option<&str>) -> Result<Option<&str>, (StatusCode, String)> {
    match id {
        Some(id) if !is_valid_client_id(id) => Err((
            StatusCode::BAD_REQUEST,
            "invalid client_turn_id".to_string(),
        )),
        id => Ok(id),
    }
}

/// Metadata of a message the chat handlers queue for the agent (TUI design 8.3.1).
///
/// `user_id` scopes the message's SSE events to its user; `message_id` (the message's
/// own id) and the optional `client_turn_id` are merged into every event it causes,
/// and their presence enables the `turn_started` / `turn_finished` events.
pub(crate) fn gateway_message_metadata(
    user_id: &str,
    thread_id: &str,
    message_id: Uuid,
    client_turn_id: Option<&str>,
) -> serde_json::Value {
    let mut meta = serde_json::json!({
        "user_id": user_id,
        "thread_id": thread_id,
        "message_id": message_id.to_string(),
    });
    if let Some(id) = client_turn_id {
        meta["client_turn_id"] = serde_json::json!(id);
    }
    meta
}

fn require_chat_thread_id(thread_id: Option<&str>) -> Result<&str, (StatusCode, String)> {
    thread_id.filter(|value| !value.trim().is_empty()).ok_or((
        StatusCode::BAD_REQUEST,
        "thread_id is required for /api/chat/send".to_string(),
    ))
}

async fn chat_send_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: axum::http::HeaderMap,
    Json(req): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<SendMessageResponse>), (StatusCode, String)> {
    tracing::trace!(
        "[chat_send_handler] Received message: content_len={}, thread_id={:?}",
        req.content.len(),
        req.thread_id
    );

    let thread_id = require_chat_thread_id(req.thread_id.as_deref())?;
    let client_turn_id = validate_client_turn_id(req.client_turn_id.as_deref())?;

    if !state.chat_rate_limiter.check(&user.user_id) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Rate limit exceeded. Try again shortly.".to_string(),
        ));
    }

    // Decoded only after the rate limit: a refused request costs no decoding.
    let attachments = gateway_image_attachments(&req.images)?;

    let mut msg = IncomingMessage::new("gateway", &user.user_id, &req.content);
    if state.owner_id != state.default_sender_id && user.user_id == state.owner_id {
        msg = msg.with_sender_id(&state.default_sender_id);
    }
    // Prefer timezone from JSON body, fall back to X-Timezone header
    let tz = req
        .timezone
        .as_deref()
        .or_else(|| headers.get("X-Timezone").and_then(|v| v.to_str().ok()));
    if let Some(tz) = tz {
        msg = msg.with_timezone(tz);
    }

    // user_id scopes the message's SSE events; the ids correlate them (G1).
    let meta = gateway_message_metadata(&user.user_id, thread_id, msg.id, client_turn_id);
    msg = msg.with_thread(thread_id).with_metadata(meta);

    if !attachments.is_empty() {
        msg = msg.with_attachments(attachments);
    }

    let msg_id = msg.id;
    tracing::trace!(
        "[chat_send_handler] Created message id={}, content_len={}, images={}",
        msg_id,
        req.content.len(),
        req.images.len()
    );

    // Clone sender to avoid holding RwLock read guard across send().await
    let tx = {
        let tx_guard = state.msg_tx.read().await;
        tx_guard
            .as_ref()
            .ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "Channel not started".to_string(),
            ))?
            .clone()
    };

    tracing::debug!("[chat_send_handler] Sending message through channel");
    tx.send(msg).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Channel closed".to_string(),
        )
    })?;

    tracing::debug!("[chat_send_handler] Message sent successfully, returning 202 ACCEPTED");

    Ok((
        StatusCode::ACCEPTED,
        Json(SendMessageResponse {
            message_id: msg_id,
            status: "accepted",
        }),
    ))
}

/// `GET /api/chat/capabilities` (TUI design 4.2.2): the chat contract this gateway
/// serves. Protected and not rate-limited. Without client tokens (M2b) every request
/// carries the master token.
async fn chat_capabilities_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(_user): AuthenticatedUser,
) -> Json<ChatCapabilitiesResponse> {
    Json(chat_capabilities(
        state.voice.is_some(),
        crate::bridge::is_engine_v2_enabled(),
    ))
}

/// The capabilities document for a gateway with or without the voice API and Engine V2.
fn chat_capabilities(voice: bool, engine_v2: bool) -> ChatCapabilitiesResponse {
    let features = FEATURES_CANONICAL
        .into_iter()
        .filter(|feature| match *feature {
            FEATURE_STATUS | FEATURE_ATTACHMENTS => true,
            FEATURE_TRANSCRIBE => voice,
            FEATURE_STREAM => engine_v2,
            // M2b: true once the daemon loads `state/gateway-client-tokens.json`;
            // until then `mt-admin tui-access` refuses to mint client tokens.
            FEATURE_CLIENT_TOKENS => false,
            _ => false,
        })
        .collect();
    let limits = lunarwing_common::WireLimits {
        max_content_bytes: MAX_CHAT_CONTENT_BYTES,
        max_attachments: u32::try_from(GATEWAY_IMAGE_LIMITS.max_count).unwrap_or(u32::MAX),
        max_attachment_bytes: GATEWAY_IMAGE_LIMITS.max_bytes as u64,
        max_total_attachment_bytes: GATEWAY_IMAGE_LIMITS.max_total_bytes as u64,
        attachment_mime_types: IMAGE_MIME_ALLOWLIST.into_iter().map(String::from).collect(),
        max_transcribe_b64_bytes: if voice { MAX_AUDIO_B64_LEN as u64 } else { 0 },
        transcribe_formats: if voice {
            AUDIO_FORMATS.into_iter().map(String::from).collect()
        } else {
            Vec::new()
        },
        messages_per_minute: per_minute(CHAT_RATE_LIMIT),
        transcribes_per_minute: per_minute(VOICE_RATE_LIMIT),
        max_request_bytes: Some(MAX_REQUEST_BODY_BYTES as u64),
        max_frame_bytes: None,
        max_in_flight: None,
        idle_timeout_secs: None,
    };
    ChatCapabilitiesResponse {
        api_version: CHAT_API_VERSION,
        server_version: env!("CARGO_PKG_VERSION"),
        engine: if engine_v2 { "v2" } else { "v1" },
        features,
        limits,
        voice: VoiceCapabilities {
            provider: voice.then_some(VOICE_PROVIDER),
            stt: voice,
            tts: voice,
        },
        credential: "master",
    }
}

/// Requests per minute of a `(requests, window_secs)` limit, rounded down.
fn per_minute((requests, window_secs): (u32, u64)) -> u32 {
    let per_minute = u64::from(requests).saturating_mul(60) / window_secs.max(1);
    u32::try_from(per_minute).unwrap_or(u32::MAX)
}

pub(crate) async fn chat_pending_gates_handler(
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let gates = crate::bridge::list_engine_pending_gates(&user.user_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Failed to recover pending engine gates");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Failed to list pending gates".into(),
            )
        })?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(super::types::PendingGatesResponse { gates }),
    ))
}

pub(crate) async fn chat_approval_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(req): Json<ApprovalRequest>,
) -> Result<(StatusCode, Json<SendMessageResponse>), (StatusCode, String)> {
    let (approved, always) = match req.action.as_str() {
        "approve" => (true, false),
        "always" => (true, true),
        "deny" => (false, false),
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Unknown action: {}", other),
            ));
        }
    };

    let request_id = Uuid::parse_str(&req.request_id).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Invalid request_id (expected UUID)".to_string(),
        )
    })?;
    // The owning turn's client id: the resumed turn's events carry it.
    let client_turn_id = validate_client_turn_id(req.client_turn_id.as_deref())?;

    // WI#472: resolve the owning thread server-side. Without this an approval
    // sent without `thread_id` matched no gate and still returned 202, leaving
    // the gate (and for `deny`, the action) untouched.
    let thread_id =
        resolve_approval_thread(&state, &user.user_id, request_id, req.thread_id.as_deref())
            .await?;

    // Build a structured ExecApproval submission as JSON, sent through the
    // existing message pipeline so the agent loop picks it up.
    let approval = crate::agent::submission::Submission::ExecApproval {
        request_id,
        approved,
        always,
    };
    let content = serde_json::to_string(&approval).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to serialize approval: {}", e),
        )
    })?;

    let mut msg = IncomingMessage::new("gateway", &user.user_id, content);
    if state.owner_id != state.default_sender_id && user.user_id == state.owner_id {
        msg = msg.with_sender_id(&state.default_sender_id);
    }

    // Same metadata as a send, so the continuation is user-scoped and correlated.
    let meta = gateway_message_metadata(&user.user_id, &thread_id, msg.id, client_turn_id);
    msg = msg.with_thread(&thread_id).with_metadata(meta);

    let msg_id = msg.id;

    // Clone sender to avoid holding RwLock read guard across send().await
    let tx = {
        let tx_guard = state.msg_tx.read().await;
        tx_guard
            .as_ref()
            .ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "Channel not started".to_string(),
            ))?
            .clone()
    };

    tx.send(msg).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Channel closed".to_string(),
        )
    })?;

    Ok((
        StatusCode::ACCEPTED,
        Json(SendMessageResponse {
            message_id: msg_id,
            status: "accepted",
        }),
    ))
}

/// Resolve the engine/legacy thread that owns `request_id` for `user_id`.
///
/// Engine V2 pending gates are consulted first (they outlive the in-memory
/// session for work that originated on another channel), then the caller's own
/// legacy session. Both lookups are user-scoped, so an approval can never
/// resolve another user's gate. Returns 404 when no live gate matches — the
/// caller must not report success for an approval that would be a no-op — and
/// 400 when the request names a `thread_id` that does not own the gate.
async fn resolve_approval_thread(
    state: &GatewayState,
    user_id: &str,
    request_id: Uuid,
    requested_thread: Option<&str>,
) -> Result<String, (StatusCode, String)> {
    let requested = requested_thread.map(str::trim).filter(|id| !id.is_empty());
    if requested_thread.is_some() && requested.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "thread_id must not be blank when supplied".to_string(),
        ));
    }

    let engine_gate =
        crate::bridge::find_engine_approval_gate_for_request(user_id, request_id)
            .await
            .map_err(|error| {
                tracing::warn!(%error, %user_id, %request_id, "Failed to inspect pending approval gate");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Failed to inspect pending approvals".to_string(),
                )
            })?;
    if let Some(gate) = engine_gate {
        return owned_thread(gate.thread_id, requested);
    }

    if let Some(session_manager) = state.session_manager.as_ref()
        && let Some(thread_id) = session_manager
            .find_awaiting_approval_thread(user_id, request_id)
            .await
    {
        return owned_thread(thread_id.to_string(), requested);
    }

    Err((
        StatusCode::NOT_FOUND,
        "No pending approval matches this request_id".to_string(),
    ))
}

/// Accept the resolved owner, or reject a caller-supplied thread that differs.
fn owned_thread(owner: String, requested: Option<&str>) -> Result<String, (StatusCode, String)> {
    match requested {
        Some(requested) if requested != owner => Err((
            StatusCode::BAD_REQUEST,
            "thread_id does not own this approval request".to_string(),
        )),
        _ => Ok(owner),
    }
}

/// Submit an auth token to the extension manager and resume a matching engine gate.
///
/// For Engine V2, the token enters the gate-control path on the exact pending
/// thread. That path consumes it before normal chat history or LLM handling.
pub(crate) async fn chat_auth_token_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(req): Json<AuthTokenRequest>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let client_turn_id = validate_client_turn_id(req.client_turn_id.as_deref())?;
    let pending = requested_engine_auth_gate(
        &user.user_id,
        &req.extension_name,
        req.request_id,
        req.thread_id,
    )
    .await?;
    let ext_mgr = state.extension_manager.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Extension manager not available".to_string(),
    ))?;

    match ext_mgr
        .configure_token(&req.extension_name, &req.token, &user.user_id)
        .await
    {
        Ok(result) => {
            let mut resp = if result.verification.is_some() || result.activated {
                ActionResponse::ok(result.message.clone())
            } else {
                ActionResponse::fail(result.message.clone())
            };
            resp.activated = Some(result.activated);
            resp.auth_url = result.auth_url.clone();
            resp.verification = result.verification.clone();
            resp.instructions = result.verification.as_ref().map(|v| v.instructions.clone());

            let engine_resume_queued = if result.activated {
                queue_engine_auth_resume(
                    &state,
                    &user.user_id,
                    &req.extension_name,
                    &req.token,
                    pending.as_ref(),
                    client_turn_id,
                )
                .await?
            } else {
                false
            };
            announce_configured_token(
                &state,
                &user.user_id,
                &req.extension_name,
                result,
                engine_resume_queued,
            )
            .await;

            Ok(Json(resp))
        }
        Err(e) => {
            let msg = e.to_string();
            // Re-emit auth_required for retry on validation errors
            if matches!(e, crate::extensions::ExtensionError::ValidationFailed(_)) {
                state.sse.broadcast_for_user(
                    &user.user_id,
                    SseEvent::AuthRequired {
                        extension_name: req.extension_name.clone(),
                        instructions: Some(msg.clone()),
                        auth_url: None,
                        setup_url: None,
                    },
                );
            }
            Ok(Json(ActionResponse::fail(msg)))
        }
    }
}

/// Tells the user's clients how a submitted credential was handled.
///
/// A queued Engine V2 resume reports the outcome through the resumed turn instead.
async fn announce_configured_token(
    state: &GatewayState,
    user_id: &str,
    extension_name: &str,
    result: crate::extensions::ConfigureResult,
    engine_resume_queued: bool,
) {
    if result.verification.is_some() {
        state.sse.broadcast_for_user(
            user_id,
            SseEvent::AuthRequired {
                extension_name: extension_name.to_string(),
                instructions: Some(result.message),
                auth_url: None,
                setup_url: None,
            },
        );
    } else if result.activated {
        if !engine_resume_queued {
            clear_auth_mode(state, user_id).await;
            state.sse.broadcast_for_user(
                user_id,
                SseEvent::AuthCompleted {
                    extension_name: extension_name.to_string(),
                    success: true,
                    message: result.message,
                },
            );
        }
    } else {
        state.sse.broadcast_for_user(
            user_id,
            SseEvent::AuthCompleted {
                extension_name: extension_name.to_string(),
                success: false,
                message: result.message,
            },
        );
    }
}

fn matching_engine_auth_thread<'a>(
    pending: Option<&'a crate::gate::pending::PendingGateView>,
    extension_name: &str,
) -> Option<&'a str> {
    let pending = pending?;
    match &pending.resume_kind {
        lunarwing_engine::ResumeKind::Authentication {
            credential_name, ..
        } if credential_name == extension_name => Some(pending.thread_id.as_str()),
        lunarwing_engine::ResumeKind::Approval { .. }
        | lunarwing_engine::ResumeKind::Authentication { .. }
        | lunarwing_engine::ResumeKind::External { .. } => None,
    }
}

async fn requested_engine_auth_gate(
    user_id: &str,
    extension_name: &str,
    request_id: Option<Uuid>,
    thread_id: Option<Uuid>,
) -> Result<Option<crate::gate::pending::PendingGateView>, (StatusCode, String)> {
    if request_id.is_some() != thread_id.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "request_id and thread_id must be supplied together".into(),
        ));
    }
    let thread_hint = thread_id.map(|id| id.to_string());
    let pending = crate::bridge::get_engine_pending_gate(user_id, thread_hint.as_deref())
        .await
        .map_err(|error| {
            tracing::warn!(
                user_id = %user_id,
                extension = %extension_name,
                error = %error,
                "Failed to inspect pending Engine V2 auth gate"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to inspect pending authentication state".to_string(),
            )
        })?;
    let pending = pending.filter(|gate| {
        matching_engine_auth_thread(Some(gate), extension_name).is_some()
            && request_id.is_none_or(|id| gate.request_id == id.to_string())
            && thread_id.is_none_or(|id| gate.thread_id == id.to_string())
    });
    if request_id.is_some() && pending.is_none() {
        return Err((
            StatusCode::CONFLICT,
            "Authentication request is stale or no longer available".into(),
        ));
    }
    Ok(pending)
}

/// Queues the Engine V2 resume of the gate waiting for `extension_name`.
///
/// The resume message carries gateway metadata with its own `message_id` and the
/// owning turn's `client_turn_id`, so the resumed turn's events and lifecycle belong
/// to that turn (TUI design 8.3.1, F1).
async fn queue_engine_auth_resume(
    state: &GatewayState,
    user_id: &str,
    extension_name: &str,
    token: &str,
    pending: Option<&crate::gate::pending::PendingGateView>,
    client_turn_id: Option<&str>,
) -> Result<bool, (StatusCode, String)> {
    let Some(pending) = pending else {
        return Ok(false);
    };
    let thread_id = &pending.thread_id;
    let parse_id = |id: &str| {
        Uuid::parse_str(id).map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Invalid authentication gate identity".into(),
            )
        })
    };
    let mut message = IncomingMessage::new("gateway", user_id, "").with_thread(thread_id);
    let meta = gateway_message_metadata(user_id, thread_id, message.id, client_turn_id);
    message = message.with_metadata(meta);
    message.auth_gate_submission = Some(crate::bridge::router::auth::AuthGateSubmission {
        request_id: parse_id(&pending.request_id)?,
        thread_id: lunarwing_engine::ThreadId(parse_id(thread_id)?),
        credential_name: extension_name.to_string(),
        token: secrecy::SecretString::from(token.to_string()),
    });
    if state.owner_id != state.default_sender_id && user_id == state.owner_id {
        message = message.with_sender_id(&state.default_sender_id);
    }

    let tx = {
        let tx_guard = state.msg_tx.read().await;
        tx_guard
            .as_ref()
            .ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "Channel not started".to_string(),
            ))?
            .clone()
    };
    tx.send(message).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Channel closed".to_string(),
        )
    })?;

    Ok(true)
}

/// Cancel an in-progress auth flow.
pub(crate) async fn chat_auth_cancel_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(req): Json<AuthCancelRequest>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    // Validated like the other chat requests; the cancellation emits no lifecycle
    // events (on Engine V2 the gate announces `gate_resolved{cancelled}`).
    validate_client_turn_id(req.client_turn_id.as_deref())?;
    let pending = requested_engine_auth_gate(
        &user.user_id,
        &req.extension_name,
        req.request_id,
        req.thread_id,
    )
    .await?;
    if let Some(pending) = pending {
        let parse_id = |id: &str| {
            Uuid::parse_str(id).map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Invalid authentication gate identity".into(),
                )
            })
        };
        crate::bridge::router::auth::cancel(
            &IncomingMessage::new("gateway", &user.user_id, ""),
            lunarwing_engine::ThreadId(parse_id(&pending.thread_id)?),
            parse_id(&pending.request_id)?,
            &req.extension_name,
        )
        .await
        .map_err(|error| (StatusCode::CONFLICT, error.to_string()))?;
        return Ok(Json(ActionResponse::ok("Auth cancelled")));
    }
    clear_auth_mode(&state, &user.user_id).await;
    Ok(Json(ActionResponse::ok("Auth cancelled")))
}

/// Clear pending auth mode on the active thread.
pub async fn clear_auth_mode(state: &GatewayState, user_id: &str) {
    clear_session_auth_mode(state.session_manager.as_ref(), user_id).await;
}

async fn clear_session_auth_mode(session_manager: Option<&Arc<SessionManager>>, user_id: &str) {
    if let Some(sm) = session_manager {
        let session = sm.get_or_create_session(user_id).await;
        let mut sess = session.lock().await;
        if let Some(thread_id) = sess.active_thread
            && let Some(thread) = sess.threads.get_mut(&thread_id)
        {
            thread.pending_auth = None;
        }
    }
}

async fn chat_events_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let sse = state.sse.subscribe(Some(user.user_id)).ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Too many connections".to_string(),
    ))?;
    Ok((
        [("X-Accel-Buffering", "no"), ("Cache-Control", "no-cache")],
        sse,
    ))
}

/// Check whether an Origin header value points to a local address.
///
/// Extracts the host from the origin (handling both IPv4/hostname and IPv6
/// literal formats) and compares it against known local addresses. Used to
/// prevent cross-site WebSocket hijacking while allowing localhost access.
fn is_local_origin(origin: &str) -> bool {
    let host = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .and_then(|rest| {
            if rest.starts_with('[') {
                // IPv6 literal: extract "[::1]" up to and including ']'
                rest.find(']').map(|i| &rest[..=i])
            } else {
                // IPv4 or hostname: take up to the first ':' (port) or '/' (path)
                rest.split(':').next()?.split('/').next()
            }
        })
        .unwrap_or("");

    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

async fn chat_ws_handler(
    AuthenticatedUser(user): AuthenticatedUser,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
    State(state): State<Arc<GatewayState>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // Validate Origin header to prevent cross-site WebSocket hijacking.
    // Require the header outright; browsers always send it for WS upgrades,
    // so a missing Origin means a non-browser client trying to bypass the check.
    let origin = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            (
                StatusCode::FORBIDDEN,
                "WebSocket Origin header required".to_string(),
            )
        })?;

    let is_local = is_local_origin(origin);
    if !is_local {
        return Err((
            StatusCode::FORBIDDEN,
            "WebSocket origin not allowed".to_string(),
        ));
    }
    Ok(ws.on_upgrade(move |socket| {
        crate::channels::web::ws::handle_ws_connection(socket, state, user)
    }))
}

#[derive(Deserialize)]
struct HistoryQuery {
    thread_id: Option<String>,
    limit: Option<usize>,
    before: Option<String>,
}

/// Engine V2 approval gate for this thread, shaped for the history envelope
/// (WI#520).
///
/// Engine V2 parks approvals in the shared gate store instead of the legacy
/// in-memory session turn, so durable history carried no approval state and a
/// client polling only `/api/chat/history` could never learn the `request_id`
/// while a gate was live. The lookup is user-scoped and thread-scoped and a
/// failure here degrades to `None` (logged): history must still render.
async fn engine_pending_approval(user_id: &str, thread_id: Uuid) -> Option<PendingApprovalInfo> {
    if !crate::bridge::is_engine_v2_enabled() {
        return None;
    }
    match crate::bridge::get_engine_pending_approval(user_id, thread_id).await {
        Ok(pending) => pending.as_ref().map(PendingApprovalInfo::from),
        Err(error) => {
            tracing::warn!(
                %user_id,
                %thread_id,
                %error,
                "Failed to read the engine approval gate for chat history"
            );
            None
        }
    }
}

async fn chat_history_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>, (StatusCode, String)> {
    let session_manager = state.session_manager.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Session manager not available".to_string(),
    ))?;

    let session = session_manager.get_or_create_session(&user.user_id).await;
    let sess = session.lock().await;

    let limit = query.limit.unwrap_or(50);
    let before_cursor = query
        .before
        .as_deref()
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|_| {
                    (
                        StatusCode::BAD_REQUEST,
                        "Invalid 'before' timestamp".to_string(),
                    )
                })
        })
        .transpose()?;

    // Find the thread
    let thread_id = if let Some(ref tid) = query.thread_id {
        Uuid::parse_str(tid)
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid thread_id".to_string()))?
    } else {
        sess.active_thread
            .ok_or((StatusCode::NOT_FOUND, "No active thread".to_string()))?
    };

    // Verify the thread belongs to the authenticated user before returning any data.
    // In-memory threads are already scoped by user via session_manager, but DB
    // lookups could expose another user's conversation if the UUID is guessed.
    if query.thread_id.is_some()
        && let Some(ref store) = state.store
    {
        let owned = store
            .conversation_belongs_to_user(thread_id, &user.user_id)
            .await
            .map_err(|e| {
                tracing::error!(thread_id = %thread_id, error = %e, "DB error during thread ownership check");
                (StatusCode::INTERNAL_SERVER_ERROR, "Database error".to_string())
            })?;
        if !owned && !sess.threads.contains_key(&thread_id) {
            return Err((StatusCode::NOT_FOUND, "Thread not found".to_string()));
        }
    }

    // For paginated requests (before cursor set), always go to DB
    if before_cursor.is_some()
        && let Some(ref store) = state.store
    {
        let (messages, has_more) = store
            .list_conversation_messages_paginated(thread_id, before_cursor, limit as i64)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let oldest_timestamp = messages.first().map(|m| m.created_at.to_rfc3339());
        let turns = build_turns_from_db_messages(&messages);
        let pending_approval = engine_pending_approval(&user.user_id, thread_id).await;
        return Ok(Json(HistoryResponse {
            thread_id,
            turns,
            has_more,
            oldest_timestamp,
            pending_approval,
        }));
    }

    // Engine V2 dual-writes history rather than updating legacy session turns.
    let prefer_durable_history = state.store.is_some() && crate::bridge::is_engine_v2_enabled();
    if let Some(thread) = sess.threads.get(&thread_id)
        && !prefer_durable_history
        && (!thread.turns.is_empty() || thread.pending_approval.is_some())
    {
        let turns: Vec<TurnInfo> = thread
            .turns
            .iter()
            .map(|t| {
                let tool_calls = t
                    .tool_calls
                    .iter()
                    .map(|tc| ToolCallInfo {
                        name: tc.name.clone(),
                        has_result: tc.result.is_some(),
                        has_error: tc.error.is_some(),
                        result_preview: tc.result.as_ref().map(|r| {
                            let s = match r {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            truncate_preview(&s, 500)
                        }),
                        error: tc.error.clone(),
                    })
                    .collect::<Vec<_>>();
                let reasoning = reasoning_summary(&tool_calls);
                TurnInfo {
                    turn_number: t.turn_number,
                    user_input: t.user_input.clone(),
                    response: t.response.clone(),
                    state: format!("{:?}", t.state),
                    started_at: t.started_at.to_rfc3339(),
                    completed_at: t.completed_at.map(|dt| dt.to_rfc3339()),
                    tool_calls,
                    reasoning,
                    recovery: None,
                }
            })
            .collect();

        let pending_approval = thread
            .pending_approval
            .as_ref()
            .map(PendingApprovalInfo::from);

        return Ok(Json(HistoryResponse {
            thread_id,
            turns,
            has_more: false,
            oldest_timestamp: None,
            pending_approval,
        }));
    }

    // Fall back to DB for historical threads not in memory (paginated)
    if let Some(ref store) = state.store {
        let (messages, has_more) = store
            .list_conversation_messages_paginated(thread_id, None, limit as i64)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if !messages.is_empty() {
            let oldest_timestamp = messages.first().map(|m| m.created_at.to_rfc3339());
            let turns = build_turns_from_db_messages(&messages);
            let pending_approval = engine_pending_approval(&user.user_id, thread_id).await;
            return Ok(Json(HistoryResponse {
                thread_id,
                turns,
                has_more,
                oldest_timestamp,
                pending_approval,
            }));
        }
    }

    // Empty thread (just created, no messages yet).
    let pending_approval = engine_pending_approval(&user.user_id, thread_id).await;
    Ok(Json(HistoryResponse {
        thread_id,
        turns: Vec::new(),
        has_more: false,
        oldest_timestamp: None,
        pending_approval,
    }))
}

async fn chat_threads_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<ThreadListResponse>, (StatusCode, String)> {
    let session_manager = state.session_manager.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Session manager not available".to_string(),
    ))?;

    let session = session_manager.get_or_create_session(&user.user_id).await;
    let sess = session.lock().await;

    // Try DB first for persistent thread list
    if let Some(ref store) = state.store {
        // Auto-create assistant thread if it doesn't exist
        let assistant_id = store
            .get_or_create_assistant_conversation(&user.user_id, "gateway")
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        match store
            .list_conversations_all_channels(&user.user_id, 50)
            .await
        {
            Ok(summaries) => {
                let mut assistant_thread = None;
                let mut threads = Vec::new();

                for s in &summaries {
                    let info = ThreadInfo {
                        id: s.id,
                        state: "Idle".to_string(),
                        turn_count: s.message_count.max(0) as usize,
                        created_at: s.started_at.to_rfc3339(),
                        updated_at: s.last_activity.to_rfc3339(),
                        title: s.title.clone(),
                        thread_type: s.thread_type.clone(),
                        channel: Some(s.channel.clone()),
                    };

                    if s.id == assistant_id {
                        assistant_thread = Some(info);
                    } else {
                        threads.push(info);
                    }
                }

                // If assistant wasn't in the list (0 messages), synthesize it
                if assistant_thread.is_none() {
                    assistant_thread = Some(ThreadInfo {
                        id: assistant_id,
                        state: "Idle".to_string(),
                        turn_count: 0,
                        created_at: chrono::Utc::now().to_rfc3339(),
                        updated_at: chrono::Utc::now().to_rfc3339(),
                        title: None,
                        thread_type: Some("assistant".to_string()),
                        channel: Some("gateway".to_string()),
                    });
                }

                return Ok(Json(ThreadListResponse {
                    assistant_thread,
                    threads,
                    active_thread: sess.active_thread,
                }));
            }
            Err(e) => {
                tracing::error!(user_id = %user.user_id, error = %e, "DB error listing threads; falling back to in-memory");
            }
        }
    }

    // Fallback: in-memory only (no assistant thread without DB)
    let mut sorted_threads: Vec<_> = sess.threads.values().collect();
    sorted_threads.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
    let threads: Vec<ThreadInfo> = sorted_threads
        .into_iter()
        .map(|t| ThreadInfo {
            id: t.id,
            state: format!("{:?}", t.state),
            turn_count: t.turns.len(),
            created_at: t.created_at.to_rfc3339(),
            updated_at: t.updated_at.to_rfc3339(),
            title: None,
            thread_type: None,
            channel: Some("gateway".to_string()),
        })
        .collect();

    Ok(Json(ThreadListResponse {
        assistant_thread: None,
        threads,
        active_thread: sess.active_thread,
    }))
}

async fn chat_new_thread_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<ThreadInfo>, (StatusCode, String)> {
    let session_manager = state.session_manager.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Session manager not available".to_string(),
    ))?;

    let session = session_manager.get_or_create_session(&user.user_id).await;
    let (thread_id, info) = {
        let mut sess = session.lock().await;
        let thread = sess.create_thread();
        let id = thread.id;
        let info = ThreadInfo {
            id: thread.id,
            state: format!("{:?}", thread.state),
            turn_count: thread.turns.len(),
            created_at: thread.created_at.to_rfc3339(),
            updated_at: thread.updated_at.to_rfc3339(),
            title: None,
            thread_type: Some("thread".to_string()),
            channel: Some("gateway".to_string()),
        };
        (id, info)
    };

    // Persist the empty conversation row with thread_type metadata synchronously
    // so that the subsequent loadThreads() call from the frontend sees it.
    if let Some(ref store) = state.store {
        match store
            .ensure_conversation(thread_id, "gateway", &user.user_id, None, None)
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                user = %user.user_id,
                thread_id = %thread_id,
                "Skipped persisting new thread due to ownership/channel conflict"
            ),
            Err(e) => tracing::warn!("Failed to persist new thread: {}", e),
        }
        let metadata_val = serde_json::json!("thread");
        if let Err(e) = store
            .update_conversation_metadata_field(thread_id, "thread_type", &metadata_val)
            .await
        {
            tracing::warn!("Failed to set thread_type metadata: {}", e);
        }
    }

    Ok(Json(info))
}

// Job handlers moved to handlers/jobs.rs
// --- Logs handlers ---

async fn logs_events_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(_user): AuthenticatedUser,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let broadcaster = state.log_broadcaster.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Log broadcaster not available".to_string(),
    ))?;

    // Replay recent history so late-joining browsers see startup logs.
    // Subscribe BEFORE snapshotting to avoid a gap between history and live.
    let rx = broadcaster.subscribe();
    let history = broadcaster.recent_entries();

    let history_stream = futures::stream::iter(history).map(|entry| {
        let data = serde_json::to_string(&entry).unwrap_or_default();
        Ok::<_, Infallible>(Event::default().event("log").data(data))
    });

    let live_stream = tokio_stream::wrappers::BroadcastStream::new(rx)
        .filter_map(|result| result.ok())
        .map(|entry| {
            let data = serde_json::to_string(&entry).unwrap_or_default();
            Ok::<_, Infallible>(Event::default().event("log").data(data))
        });

    let stream = history_stream.chain(live_stream);

    Ok((
        [("X-Accel-Buffering", "no"), ("Cache-Control", "no-cache")],
        Sse::new(stream).keep_alive(
            KeepAlive::new()
                .interval(std::time::Duration::from_secs(15))
                .text(""),
        ),
    ))
}

async fn logs_level_get_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(_user): AuthenticatedUser,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let handle = state.log_level_handle.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Log level control not available".to_string(),
    ))?;
    Ok(Json(serde_json::json!({ "level": handle.current_level() })))
}

async fn logs_level_set_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let handle = state.log_level_handle.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Log level control not available".to_string(),
    ))?;

    let level = body
        .get("level")
        .and_then(|v| v.as_str())
        .ok_or((StatusCode::BAD_REQUEST, "missing 'level' field".to_string()))?;

    handle
        .set_level(level)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    tracing::info!(user_id = %user.user_id, "Log level changed to '{}'", handle.current_level());
    Ok(Json(serde_json::json!({ "level": handle.current_level() })))
}

async fn logs_download_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(_user): AuthenticatedUser,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let broadcaster = state.log_broadcaster.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Log broadcaster not available".to_string(),
    ))?;
    let entries = broadcaster.recent_entries();

    let body = entries
        .iter()
        .map(|e| serde_json::to_string(e).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");

    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"lunarwing-logs.jsonl\"",
            ),
        ],
        body,
    ))
}

// --- Extension handlers ---

async fn extensions_list_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<ExtensionListResponse>, (StatusCode, String)> {
    let ext_mgr = state.extension_manager.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Extension manager not available (secrets store required)".to_string(),
    ))?;

    let installed = ext_mgr
        .list(None, false, &user.user_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let pairing_store = crate::pairing::PairingStore::new();
    let mut owner_bound_channels = std::collections::HashSet::new();
    for ext in &installed {
        if ext.kind == crate::extensions::ExtensionKind::WasmChannel
            && ext_mgr.has_wasm_channel_owner_binding(&ext.name).await
        {
            owner_bound_channels.insert(ext.name.clone());
        }
    }
    let extensions = installed
        .into_iter()
        .map(|ext| {
            let activation_status = if ext.kind == crate::extensions::ExtensionKind::WasmChannel {
                let has_paired = pairing_store
                    .read_allow_from(&ext.name)
                    .map(|list| !list.is_empty())
                    .unwrap_or(false);
                crate::channels::web::types::classify_wasm_channel_activation(
                    &ext,
                    has_paired,
                    owner_bound_channels.contains(&ext.name),
                )
            } else {
                None
            };
            ExtensionInfo {
                name: ext.name,
                display_name: ext.display_name,
                kind: ext.kind.to_string(),
                description: ext.description,
                url: ext.url,
                transport: ext.transport,
                command: ext.command,
                placement: ext.placement,
                authenticated: ext.authenticated,
                active: ext.active,
                recovering: ext.recovering,
                enabled: ext.enabled,
                tools: ext.tools,
                needs_setup: ext.needs_setup,
                has_auth: ext.has_auth,
                activation_status,
                activation_error: ext.activation_error,
                version: ext.version,
            }
        })
        .collect();

    Ok(Json(ExtensionListResponse { extensions }))
}

async fn extensions_tools_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<ToolListResponse>, (StatusCode, String)> {
    let registry = state.tool_registry.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Tool registry not available".to_string(),
    ))?;

    let definitions = registry.tool_definitions_for_user(&user.user_id).await;
    let tools = definitions
        .into_iter()
        .map(|td| ToolInfo {
            name: td.name,
            description: td.description,
        })
        .collect();

    Ok(Json(ToolListResponse { tools }))
}

async fn extensions_install_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(req): Json<InstallExtensionRequest>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let mcp_config = match req.mcp_config() {
        Ok(config) => config,
        Err(error) => return Ok(Json(ActionResponse::fail(error))),
    };
    // When extension manager isn't available, check registry entries for a helpful message
    let Some(ext_mgr) = state.extension_manager.as_ref() else {
        // Look up the entry in the catalog to give a specific error
        if let Some(entry) = state.registry_entries.iter().find(|e| e.name == req.name) {
            let msg = match &entry.source {
                crate::extensions::ExtensionSource::WasmBuildable { .. } => {
                    format!(
                        "'{}' requires building from source. \
                         Run `lunarwing registry install {}` from the CLI.",
                        req.name, req.name
                    )
                }
                _ => format!(
                    "Extension manager not available (secrets store required). \
                     Configure DATABASE_URL or a secrets backend to enable installation of '{}'.",
                    req.name
                ),
            };
            return Ok(Json(ActionResponse::fail(msg)));
        }
        return Ok(Json(ActionResponse::fail(
            "Extension manager not available (secrets store required)".to_string(),
        )));
    };

    let kind_hint = req
        .kind
        .as_deref()
        .and_then(|k| match k {
            "mcp_server" => Some(crate::extensions::ExtensionKind::McpServer),
            "wasm_tool" => Some(crate::extensions::ExtensionKind::WasmTool),
            "wasm_channel" => Some(crate::extensions::ExtensionKind::WasmChannel),
            _ => None,
        })
        .or_else(|| {
            req.transport
                .as_ref()
                .map(|_| crate::extensions::ExtensionKind::McpServer)
        });

    let install_result = match mcp_config {
        Some(config) => ext_mgr.install_mcp_config(config, &user.user_id).await,
        None => {
            ext_mgr
                .install(&req.name, req.url.as_deref(), kind_hint, &user.user_id)
                .await
        }
    };

    match install_result {
        Ok(result) => {
            let mut resp = ActionResponse::ok(result.message);

            // Auto-activate WASM tools after install (install = active).
            if result.kind == crate::extensions::ExtensionKind::WasmTool {
                if let Err(e) = ext_mgr.activate(&req.name, &user.user_id).await {
                    tracing::debug!(
                        extension = %req.name,
                        error = %e,
                        "Auto-activation after install failed"
                    );
                }

                // Check auth after activation. This may initiate OAuth both for scope
                // expansion and for first-time auth when credentials are already
                // configured (e.g., built-in providers). We only surface an auth_url
                // when the extension reports it is awaiting authorization.
                match ext_mgr.auth(&req.name, &user.user_id).await {
                    Ok(auth_result) if auth_result.auth_url().is_some() => {
                        // Scope expansion or initial OAuth: user needs to authorize
                        resp.auth_url = auth_result.auth_url().map(String::from);
                    }
                    _ => {}
                }
            }

            Ok(Json(resp))
        }
        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
    }
}

async fn extensions_activate_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(name): Path<String>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let ext_mgr = state.extension_manager.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Extension manager not available (secrets store required)".to_string(),
    ))?;
    let is_job_worker = ext_mgr
        .get_mcp_server_pub(&name, &user.user_id)
        .await
        .is_ok_and(|config| config.placement == crate::tools::mcp::McpPlacement::JobWorker);

    match ext_mgr.activate(&name, &user.user_id).await {
        Ok(result) => {
            // Activation loaded the WASM module. Check if the tool needs
            // OAuth scope expansion (e.g., adding google-docs when gmail
            // already has a token but missing the documents scope).
            // Initial OAuth setup is triggered via configure.
            let mut resp = ActionResponse::ok(result.message);
            if !is_job_worker
                && let Ok(auth_result) = ext_mgr.auth(&name, &user.user_id).await
                && auth_result.auth_url().is_some()
            {
                resp.auth_url = auth_result.auth_url().map(String::from);
            }
            Ok(Json(resp))
        }
        Err(activate_err) => {
            let needs_auth = matches!(
                &activate_err,
                crate::extensions::ExtensionError::AuthRequired
            );

            if !needs_auth {
                return Ok(Json(ActionResponse::fail(activate_err.to_string())));
            }
            if is_job_worker {
                return Ok(Json(ActionResponse::fail(activate_err.to_string())));
            }

            // Activation failed due to auth; try authenticating first.
            match ext_mgr.auth(&name, &user.user_id).await {
                Ok(auth_result) if auth_result.is_authenticated() => {
                    // Auth succeeded, retry activation.
                    match ext_mgr.activate(&name, &user.user_id).await {
                        Ok(result) => Ok(Json(ActionResponse::ok(result.message))),
                        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
                    }
                }
                Ok(auth_result) => {
                    // Auth in progress (OAuth URL or awaiting manual token).
                    let mut resp = ActionResponse::fail(
                        auth_result
                            .instructions()
                            .map(String::from)
                            .unwrap_or_else(|| format!("'{}' requires authentication.", name)),
                    );
                    resp.auth_url = auth_result.auth_url().map(String::from);
                    resp.awaiting_token = Some(auth_result.is_awaiting_token());
                    resp.instructions = auth_result.instructions().map(String::from);
                    Ok(Json(resp))
                }
                Err(auth_err) => Ok(Json(ActionResponse::fail(format!(
                    "Authentication failed: {}",
                    auth_err
                )))),
            }
        }
    }
}

async fn extensions_deactivate_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(name): Path<String>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let ext_mgr = state.extension_manager.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Extension manager not available (secrets store required)".to_string(),
    ))?;

    match ext_mgr.deactivate(&name, &user.user_id).await {
        Ok(message) => Ok(Json(ActionResponse::ok(message))),
        Err(error) => Ok(Json(ActionResponse::fail(error.to_string()))),
    }
}

async fn extensions_toggle_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(name): Path<String>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let ext_mgr = state.extension_manager.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Extension manager not available (secrets store required)".to_string(),
    ))?;

    match ext_mgr.toggle_mcp(&name, &user.user_id).await {
        Ok(message) => Ok(Json(ActionResponse::ok(message))),
        Err(error) => Ok(Json(ActionResponse::fail(error.to_string()))),
    }
}

// --- Project file serving handlers ---

/// Redirect `/projects/{id}` to `/projects/{id}/` so relative paths in
/// the served HTML resolve within the project namespace.
async fn project_redirect_handler(
    State(state): State<Arc<GatewayState>>,
    super::auth::AuthenticatedUser(user): super::auth::AuthenticatedUser,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    if !verify_project_ownership(&state, &project_id, &user.user_id).await {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    axum::response::Redirect::permanent(&format!("/projects/{project_id}/")).into_response()
}

/// Serve `index.html` when hitting `/projects/{project_id}/`.
async fn project_index_handler(
    State(state): State<Arc<GatewayState>>,
    super::auth::AuthenticatedUser(user): super::auth::AuthenticatedUser,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    if !verify_project_ownership(&state, &project_id, &user.user_id).await {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    serve_project_file(&project_id, "index.html").await
}

/// Serve any file under `/projects/{project_id}/{path}`.
async fn project_file_handler(
    State(state): State<Arc<GatewayState>>,
    super::auth::AuthenticatedUser(user): super::auth::AuthenticatedUser,
    Path((project_id, path)): Path<(String, String)>,
) -> impl IntoResponse {
    if !verify_project_ownership(&state, &project_id, &user.user_id).await {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    serve_project_file(&project_id, &path).await
}

async fn extensions_update_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(name): Path<String>,
    Json(req): Json<InstallExtensionRequest>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let ext_mgr = state.extension_manager.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Extension manager not available (secrets store required)".to_string(),
    ))?;

    let existing = ext_mgr
        .get_mcp_server_pub(&name, &user.user_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    let new_config = match req.mcp_update_config(&name, &existing) {
        Ok(config) => config,
        Err(error) => return Ok(Json(ActionResponse::fail(error))),
    };

    match ext_mgr.update_mcp(&name, new_config, &user.user_id).await {
        Ok(message) => Ok(Json(ActionResponse::ok(message))),
        Err(error) => Ok(Json(ActionResponse::fail(error.to_string()))),
    }
}

/// Check that a project directory belongs to a job owned by the given user.
/// Returns false if the store is unavailable or the project is not found.
async fn verify_project_ownership(state: &GatewayState, project_id: &str, user_id: &str) -> bool {
    let Some(ref store) = state.store else {
        return false;
    };
    // The project_id is a sandbox job UUID used as the directory name.
    let Ok(job_id) = project_id.parse::<uuid::Uuid>() else {
        return false;
    };
    match store.get_sandbox_job(job_id).await {
        Ok(Some(job)) => job.user_id == user_id,
        _ => false,
    }
}

/// Shared logic: resolve the file inside `~/.lunarwing/projects/{project_id}/`,
/// guard against path traversal, and stream the content with the right MIME type.
async fn serve_project_file(project_id: &str, path: &str) -> axum::response::Response {
    // Reject project_id values that could escape the projects directory.
    if project_id.contains('/')
        || project_id.contains('\\')
        || project_id.contains("..")
        || project_id.is_empty()
    {
        return (StatusCode::BAD_REQUEST, "Invalid project ID").into_response();
    }

    let base = lunarwing_base_dir().join("projects").join(project_id);

    let file_path = base.join(path);

    // Path traversal guard
    let canonical = match file_path.canonicalize() {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "Not found").into_response(),
    };
    let base_canonical = match base.canonicalize() {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "Not found").into_response(),
    };
    if !canonical.starts_with(&base_canonical) {
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }

    match tokio::fs::read(&canonical).await {
        Ok(contents) => {
            let mime = mime_guess::from_path(&canonical)
                .first_or_octet_stream()
                .to_string();
            ([(header::CONTENT_TYPE, mime)], contents).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

async fn extensions_remove_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(name): Path<String>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let ext_mgr = state.extension_manager.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Extension manager not available (secrets store required)".to_string(),
    ))?;

    match ext_mgr.remove(&name, &user.user_id).await {
        Ok(message) => Ok(Json(ActionResponse::ok(message))),
        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
    }
}

async fn extensions_registry_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(params): Query<RegistrySearchQuery>,
) -> Json<RegistrySearchResponse> {
    let query = params.query.unwrap_or_default();
    let query_lower = query.to_lowercase();
    let tokens: Vec<&str> = query_lower.split_whitespace().collect();

    // Filter registry entries by query (or return all if empty)
    let matching: Vec<&crate::extensions::RegistryEntry> = if tokens.is_empty() {
        state.registry_entries.iter().collect()
    } else {
        state
            .registry_entries
            .iter()
            .filter(|e| {
                let name = e.name.to_lowercase();
                let display = e.display_name.to_lowercase();
                let desc = e.description.to_lowercase();
                tokens.iter().any(|t| {
                    name.contains(t)
                        || display.contains(t)
                        || desc.contains(t)
                        || e.keywords.iter().any(|k| k.to_lowercase().contains(t))
                })
            })
            .collect()
    };

    // Cross-reference with installed extensions by (name, kind) to avoid
    // false positives when the same name exists as different kinds.
    let installed: std::collections::HashSet<(String, String)> =
        if let Some(ext_mgr) = state.extension_manager.as_ref() {
            ext_mgr
                .list(None, false, &user.user_id)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|ext| (ext.name, ext.kind.to_string()))
                .collect()
        } else {
            std::collections::HashSet::new()
        };

    let entries = matching
        .into_iter()
        .map(|e| {
            let kind_str = e.kind.to_string();
            RegistryEntryInfo {
                name: e.name.clone(),
                display_name: e.display_name.clone(),
                installed: installed.contains(&(e.name.clone(), kind_str.clone())),
                kind: kind_str,
                description: e.description.clone(),
                keywords: e.keywords.clone(),
                version: e.version.clone(),
            }
        })
        .collect();

    Json(RegistrySearchResponse { entries })
}

async fn extensions_setup_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(name): Path<String>,
) -> Result<Json<ExtensionSetupResponse>, (StatusCode, String)> {
    let ext_mgr = state.extension_manager.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Extension manager not available (secrets store required)".to_string(),
    ))?;

    let setup = ext_mgr
        .get_setup_schema(&name, &user.user_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let kind = ext_mgr
        .list(None, false, &user.user_id)
        .await
        .ok()
        .and_then(|list| list.into_iter().find(|e| e.name == name))
        .map(|e| e.kind.to_string())
        .unwrap_or_default();

    Ok(Json(ExtensionSetupResponse {
        name,
        kind,
        secrets: setup.secrets,
        fields: setup.fields,
    }))
}

async fn extensions_setup_submit_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(name): Path<String>,
    Json(req): Json<ExtensionSetupRequest>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let ext_mgr = state.extension_manager.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Extension manager not available (secrets store required)".to_string(),
    ))?;

    // Clear auth mode regardless of outcome so the next user message goes
    // through to the LLM instead of being intercepted as a token.
    clear_auth_mode(&state, &user.user_id).await;

    match ext_mgr
        .configure(&name, &req.secrets, &req.fields, &user.user_id)
        .await
    {
        Ok(result) => {
            let mut resp = if result.verification.is_some() || result.activated {
                ActionResponse::ok(result.message)
            } else {
                ActionResponse::fail(result.message)
            };
            resp.activated = Some(result.activated);
            if result.restart_required || !result.activated {
                resp.needs_restart = Some(true);
            }
            resp.auth_url = result.auth_url.clone();
            resp.verification = result.verification.clone();
            resp.instructions = result.verification.as_ref().map(|v| v.instructions.clone());
            if result.verification.is_none() {
                // Broadcast auth_completed so the chat UI can dismiss any in-progress
                // auth card or setup modal that was triggered by tool_auth/tool_activate.
                state.sse.broadcast_for_user(
                    &user.user_id,
                    SseEvent::AuthCompleted {
                        extension_name: name.clone(),
                        success: result.activated,
                        message: resp.message.clone(),
                    },
                );
            }
            Ok(Json(resp))
        }
        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
    }
}

// --- Pairing handlers ---

async fn pairing_list_handler(
    Path(channel): Path<String>,
) -> Result<Json<PairingListResponse>, (StatusCode, String)> {
    let store = crate::pairing::PairingStore::new();
    let requests = store
        .list_pending(&channel)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let infos = requests
        .into_iter()
        .map(|r| PairingRequestInfo {
            code: r.code,
            sender_id: r.id,
            meta: r.meta,
            created_at: r.created_at,
        })
        .collect();

    Ok(Json(PairingListResponse {
        channel,
        requests: infos,
    }))
}

async fn pairing_approve_handler(
    Path(channel): Path<String>,
    Json(req): Json<PairingApproveRequest>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let store = crate::pairing::PairingStore::new();
    match store.approve(&channel, &req.code) {
        Ok(Some(approved)) => Ok(Json(ActionResponse::ok(format!(
            "Pairing approved for sender '{}'",
            approved.id
        )))),
        Ok(None) => Ok(Json(ActionResponse::fail(
            "Invalid or expired pairing code".to_string(),
        ))),
        Err(crate::pairing::PairingStoreError::ApproveRateLimited) => Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed approve attempts; try again later".to_string(),
        )),
        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
    }
}

async fn routines_runs_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Database not available".to_string(),
    ))?;

    let routine_id = Uuid::parse_str(&id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid routine ID".to_string()))?;

    // Verify ownership before listing runs.
    let routine = store
        .get_routine(routine_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Routine not found".to_string()))?;

    if routine.user_id != user.user_id {
        return Err((StatusCode::NOT_FOUND, "Routine not found".to_string()));
    }

    let runs = store
        .list_routine_runs(routine_id, 50)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let run_infos: Vec<RoutineRunInfo> = runs
        .iter()
        .map(|run| RoutineRunInfo {
            id: run.id,
            trigger_type: run.trigger_type.clone(),
            started_at: run.started_at.to_rfc3339(),
            completed_at: run.completed_at.map(|dt| dt.to_rfc3339()),
            status: run.status.to_string(),
            result_summary: run.result_summary.clone(),
            tokens_used: run.tokens_used,
            job_id: run.job_id,
        })
        .collect();

    Ok(Json(serde_json::json!({
        "routine_id": routine_id,
        "runs": run_infos,
    })))
}

// --- Settings handlers ---

async fn settings_list_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<SettingsListResponse>, StatusCode> {
    let store = state
        .store
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let rows = store.list_settings(&user.user_id).await.map_err(|e| {
        tracing::error!("Failed to list settings: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let settings = rows
        .into_iter()
        .map(|r| SettingResponse {
            key: r.key,
            value: r.value,
            updated_at: r.updated_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(SettingsListResponse { settings }))
}

async fn settings_get_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(key): Path<String>,
) -> Result<Json<SettingResponse>, StatusCode> {
    let store = state
        .store
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let row = store
        .get_setting_full(&user.user_id, &key)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get setting '{}': {}", key, e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(SettingResponse {
        key: row.key,
        value: row.value,
        updated_at: row.updated_at.to_rfc3339(),
    }))
}

async fn settings_set_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(key): Path<String>,
    Json(body): Json<SettingWriteRequest>,
) -> Result<StatusCode, StatusCode> {
    crate::settings::validate_setting_write(&key, &body.value).map_err(|error| {
        tracing::warn!(setting = %key, %error, "Rejected settings API write");
        StatusCode::BAD_REQUEST
    })?;

    let store = state
        .store
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    store
        .set_setting(&user.user_id, &key, &body.value)
        .await
        .map_err(|e| {
            tracing::error!("Failed to set setting '{}': {}", key, e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(StatusCode::NO_CONTENT)
}

async fn settings_delete_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(key): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let store = state
        .store
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    store
        .delete_setting(&user.user_id, &key)
        .await
        .map_err(|e| {
            tracing::error!("Failed to delete setting '{}': {}", key, e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(StatusCode::NO_CONTENT)
}

async fn settings_export_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<SettingsExportResponse>, StatusCode> {
    let store = state
        .store
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let settings = store.get_all_settings(&user.user_id).await.map_err(|e| {
        tracing::error!("Failed to export settings: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(SettingsExportResponse { settings }))
}

async fn settings_import_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(body): Json<SettingsImportRequest>,
) -> Result<StatusCode, StatusCode> {
    crate::settings::validate_settings_write(&body.settings).map_err(|error| {
        tracing::warn!(%error, "Rejected settings API import");
        StatusCode::BAD_REQUEST
    })?;

    let store = state
        .store
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    store
        .set_all_settings(&user.user_id, &body.settings)
        .await
        .map_err(|e| {
            tracing::error!("Failed to import settings: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(StatusCode::NO_CONTENT)
}

// --- Gateway control plane handlers ---

async fn gateway_status_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(_user): AuthenticatedUser,
) -> Json<GatewayStatusResponse> {
    let sse_connections = state.sse.connection_count();
    let ws_connections = state
        .ws_tracker
        .as_ref()
        .map(|t| t.connection_count())
        .unwrap_or(0);

    let uptime_secs = state.startup_time.elapsed().as_secs();

    let (daily_cost, actions_this_hour, model_usage) = if let Some(ref cg) = state.cost_guard {
        let cost = cg.daily_spend().await;
        let actions = cg.actions_this_hour().await;
        let usage = cg.model_usage().await;
        let models: Vec<ModelUsageEntry> = usage
            .into_iter()
            .map(|(model, tokens)| ModelUsageEntry {
                model,
                input_tokens: tokens.input_tokens,
                output_tokens: tokens.output_tokens,
                cost: format!("{:.6}", tokens.cost),
            })
            .collect();
        (Some(format!("{:.4}", cost)), Some(actions), Some(models))
    } else {
        (None, None, None)
    };

    let restart_enabled = crate::config::helpers::env_or_override("LUNARWING_IN_DOCKER")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false);

    let channel_health = if let Some(ref cm) = state.channel_manager {
        let results = cm.health_check_all().await;
        Some(
            results
                .into_iter()
                .map(|(name, result)| {
                    let entry = match result {
                        Ok(()) => ChannelHealthEntry {
                            healthy: true,
                            error: None,
                        },
                        Err(e) => ChannelHealthEntry {
                            healthy: false,
                            error: Some(e.to_string()),
                        },
                    };
                    (name, entry)
                })
                .collect(),
        )
    } else {
        None
    };
    let channel_config_errors = if let Some(ref cm) = state.channel_manager {
        Some(cm.config_errors().await)
    } else {
        None
    };

    Json(GatewayStatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        sse_connections,
        ws_connections,
        total_connections: sse_connections + ws_connections,
        uptime_secs,
        restart_enabled,
        daily_cost,
        actions_this_hour,
        model_usage,
        llm_backend: state.active_config.llm_backend.clone(),
        llm_model: state.active_config.llm_model.clone(),
        enabled_channels: state.active_config.enabled_channels.clone(),
        skills_enabled: state.active_config.skills_enabled,
        skills_max_active: state.active_config.skills_max_active,
        skills_max_context_tokens: state.active_config.skills_max_context_tokens,
        channel_health,
        channel_config_errors,
        workload_isolation: crate::tools::process::workload_isolation_mode().await,
    })
}

#[derive(serde::Serialize)]
struct ModelUsageEntry {
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cost: String,
}

#[derive(serde::Serialize)]
struct ChannelHealthEntry {
    healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(serde::Serialize)]
struct GatewayStatusResponse {
    version: String,
    sse_connections: u64,
    ws_connections: u64,
    total_connections: u64,
    uptime_secs: u64,
    restart_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    daily_cost: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actions_this_hour: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_usage: Option<Vec<ModelUsageEntry>>,
    llm_backend: String,
    llm_model: String,
    enabled_channels: Vec<String>,
    skills_enabled: bool,
    skills_max_active: usize,
    skills_max_context_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel_health: Option<std::collections::HashMap<String, ChannelHealthEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel_config_errors: Option<std::collections::HashMap<String, String>>,
    workload_isolation: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::web::types::{
        ExtensionActivationStatus, classify_wasm_channel_activation,
    };
    use crate::cli::oauth_defaults;
    use crate::extensions::{ExtensionKind, InstalledExtension};
    use crate::testing::credentials::TEST_GATEWAY_CRYPTO_KEY;

    #[test]
    fn mascot_script_is_only_injected_when_enabled() {
        let script = r#"<script src="/mascot.js" defer></script>"#;

        assert!(!index_html(false).contains(script));
        assert_eq!(index_html(true).matches(script).count(), 1);
    }

    #[test]
    fn chat_send_requires_thread_id() {
        for thread_id in [None, Some(""), Some("   ")] {
            let error = require_chat_thread_id(thread_id)
                .expect_err("missing or blank thread id must be rejected");
            assert_eq!(error.0, StatusCode::BAD_REQUEST);
            assert!(error.1.contains("thread_id is required"));
        }
    }

    #[test]
    fn chat_send_accepts_nonempty_thread_id() {
        let thread_id = Uuid::new_v4().to_string();
        assert_eq!(
            require_chat_thread_id(Some(&thread_id)),
            Ok(thread_id.as_str())
        );
    }

    fn pending_gate_view(
        resume_kind: lunarwing_engine::ResumeKind,
    ) -> crate::gate::pending::PendingGateView {
        crate::gate::pending::PendingGateView {
            request_id: Uuid::new_v4().to_string(),
            thread_id: Uuid::new_v4().to_string(),
            gate_name: resume_kind.kind_name().to_string(),
            tool_name: "test-tool".to_string(),
            description: "test gate".to_string(),
            parameters: "{}".to_string(),
            resume_kind,
        }
    }

    /// WI#472: a caller-supplied `thread_id` must name the gate's owner; any
    /// other thread is refused rather than silently accepted, and the resolved
    /// owner is used when the caller omits it.
    #[test]
    fn approval_thread_must_match_the_resolved_gate_owner() {
        let owner = Uuid::new_v4().to_string();
        let other = Uuid::new_v4().to_string();

        assert_eq!(
            owned_thread(owner.clone(), None),
            Ok(owner.clone()),
            "omitted thread_id resolves to the gate owner"
        );
        assert_eq!(
            owned_thread(owner.clone(), Some(owner.as_str())),
            Ok(owner.clone())
        );

        let error = owned_thread(owner, Some(other.as_str()))
            .expect_err("a foreign thread_id must not be accepted");
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(error.1.contains("does not own"));
    }

    #[test]
    fn matching_engine_auth_thread_requires_matching_credential() {
        let pending = pending_gate_view(lunarwing_engine::ResumeKind::Authentication {
            credential_name: "engine-v2-auth".to_string(),
            instructions: "Provide a token".to_string(),
            auth_url: None,
        });

        assert_eq!(
            matching_engine_auth_thread(Some(&pending), "engine-v2-auth"),
            Some(pending.thread_id.as_str())
        );
        assert_eq!(
            matching_engine_auth_thread(Some(&pending), "different-extension"),
            None
        );
    }

    #[test]
    fn matching_engine_auth_thread_rejects_non_auth_gate() {
        let pending = pending_gate_view(lunarwing_engine::ResumeKind::Approval {
            allow_always: false,
        });

        assert_eq!(
            matching_engine_auth_thread(Some(&pending), "engine-v2-auth"),
            None
        );
    }

    #[test]
    fn test_build_turns_from_db_messages_complete() {
        let now = chrono::Utc::now();
        let messages = vec![
            crate::history::ConversationMessage {
                id: Uuid::new_v4(),
                role: "user".to_string(),
                content: "Hello".to_string(),
                created_at: now,
            },
            crate::history::ConversationMessage {
                id: Uuid::new_v4(),
                role: "assistant".to_string(),
                content: "Hi there!".to_string(),
                created_at: now + chrono::TimeDelta::seconds(1),
            },
            crate::history::ConversationMessage {
                id: Uuid::new_v4(),
                role: "user".to_string(),
                content: "How are you?".to_string(),
                created_at: now + chrono::TimeDelta::seconds(2),
            },
            crate::history::ConversationMessage {
                id: Uuid::new_v4(),
                role: "assistant".to_string(),
                content: "Doing well!".to_string(),
                created_at: now + chrono::TimeDelta::seconds(3),
            },
        ];

        let turns = build_turns_from_db_messages(&messages);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user_input, "Hello");
        assert_eq!(turns[0].response.as_deref(), Some("Hi there!"));
        assert_eq!(turns[0].state, "Completed");
        assert_eq!(turns[1].user_input, "How are you?");
        assert_eq!(turns[1].response.as_deref(), Some("Doing well!"));
    }

    #[test]
    fn test_build_turns_from_db_messages_incomplete_last() {
        let now = chrono::Utc::now();
        let messages = vec![
            crate::history::ConversationMessage {
                id: Uuid::new_v4(),
                role: "user".to_string(),
                content: "Hello".to_string(),
                created_at: now,
            },
            crate::history::ConversationMessage {
                id: Uuid::new_v4(),
                role: "assistant".to_string(),
                content: "Hi!".to_string(),
                created_at: now + chrono::TimeDelta::seconds(1),
            },
            crate::history::ConversationMessage {
                id: Uuid::new_v4(),
                role: "user".to_string(),
                content: "Lost message".to_string(),
                created_at: now + chrono::TimeDelta::seconds(2),
            },
        ];

        let turns = build_turns_from_db_messages(&messages);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].user_input, "Lost message");
        assert!(turns[1].response.is_none());
        assert_eq!(turns[1].state, "Failed");
    }

    #[test]
    fn test_build_turns_from_db_messages_empty() {
        let turns = build_turns_from_db_messages(&[]);
        assert!(turns.is_empty());
    }

    #[test]
    fn test_wasm_channel_activation_status_owner_bound_counts_as_active() -> Result<(), String> {
        let ext = InstalledExtension {
            name: "xmpp".to_string(),
            kind: ExtensionKind::WasmChannel,
            display_name: Some("XMPP".to_string()),
            description: None,
            url: None,
            transport: None,
            command: None,
            placement: None,
            authenticated: true,
            active: true,
            recovering: false,
            enabled: None,
            tools: Vec::new(),
            needs_setup: true,
            has_auth: false,
            installed: true,
            activation_error: None,
            version: None,
        };

        let owner_bound = classify_wasm_channel_activation(&ext, false, true);
        if owner_bound != Some(ExtensionActivationStatus::Active) {
            return Err(format!(
                "owner-bound channel should be active, got {:?}",
                owner_bound
            ));
        }

        let unbound = classify_wasm_channel_activation(&ext, false, false);
        if unbound != Some(ExtensionActivationStatus::Pairing) {
            return Err(format!(
                "unbound channel should be pairing, got {:?}",
                unbound
            ));
        }

        Ok(())
    }

    // --- OAuth callback handler tests ---

    /// Build a minimal `GatewayState` for testing the OAuth callback handler.
    fn test_gateway_state(ext_mgr: Option<Arc<ExtensionManager>>) -> Arc<GatewayState> {
        Arc::new(GatewayState {
            msg_tx: tokio::sync::RwLock::new(None),
            sse: Arc::new(SseManager::new()),
            workspace: None,
            workspace_pool: None,
            session_manager: None,
            log_broadcaster: None,
            log_level_handle: None,
            extension_manager: ext_mgr,
            tool_registry: None,
            store: None,
            job_manager: None,
            external_worker_manager: None,
            prompt_queue: None,
            owner_id: "test".to_string(),
            default_sender_id: "test".to_string(),
            shutdown_tx: tokio::sync::RwLock::new(None),
            ws_tracker: None,
            llm_provider: None,
            skill_registry: None,
            skill_catalog: None,
            scheduler: None,
            chat_rate_limiter: PerUserRateLimiter::new(30, 60),
            oauth_rate_limiter: RateLimiter::new(10, 60),
            webhook_rate_limiter: RateLimiter::new(10, 60),
            registry_entries: vec![],
            cost_guard: None,
            routine_engine: Arc::new(tokio::sync::RwLock::new(None)),
            startup_time: std::time::Instant::now(),
            active_config: ActiveConfigSnapshot::default(),
            secrets_store: None,
            db_auth: None,
            channel_manager: None,
            ws_ping_interval_secs: 30,
            ws_idle_timeout_secs: 120,
            voice: None,
            voice_rate_limiter: PerUserRateLimiter::new(10, 60),
            context_manager: None,
        })
    }

    async fn settings_request(
        method: axum::http::Method,
        uri: &str,
        body: serde_json::Value,
    ) -> StatusCode {
        use axum::body::Body;
        use tower::ServiceExt;

        let state = test_gateway_state(None);
        let app = Router::new()
            .route("/api/settings/import", post(settings_import_handler))
            .route(
                "/api/settings/{key}",
                axum::routing::put(settings_set_handler),
            )
            .with_state(state);
        let mut request = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");
        request.extensions_mut().insert(UserIdentity {
            user_id: "test".to_string(),
            workspace_read_scopes: Vec::new(),
        });

        ServiceExt::<axum::http::Request<Body>>::oneshot(app, request)
            .await
            .expect("response")
            .status()
    }

    #[tokio::test]
    async fn settings_api_rejects_removed_tunnel_values_before_database_write() {
        let provider_status = settings_request(
            axum::http::Method::PUT,
            "/api/settings/tunnel.provider",
            serde_json::json!({"value": "cloudflare"}),
        )
        .await;
        assert_eq!(provider_status, StatusCode::BAD_REQUEST);

        let key_status = settings_request(
            axum::http::Method::PUT,
            "/api/settings/tunnel.ngrok_token",
            serde_json::json!({"value": "sensitive-value"}),
        )
        .await;
        assert_eq!(key_status, StatusCode::BAD_REQUEST);

        let object_status = settings_request(
            axum::http::Method::PUT,
            "/api/settings/tunnel",
            serde_json::json!({
                "value": {"provider": "netbird", "cf_token": "sensitive-value"}
            }),
        )
        .await;
        assert_eq!(object_status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn settings_api_rejects_mixed_legacy_import_before_database_write() {
        let status = settings_request(
            axum::http::Method::POST,
            "/api/settings/import",
            serde_json::json!({
                "settings": {
                    "agent.name": "updated",
                    "tunnel.ts_hostname": "old-host"
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn networking_ui_lists_only_supported_managed_tunnel_providers() {
        let app_js = include_str!("static/app.js");
        assert!(app_js.contains("options: ['none', 'netbird', 'custom']"));
        assert!(
            !app_js.contains("options: ['none', 'cloudflare', 'ngrok', 'tailscale', 'custom']")
        );
        for key in [
            "tunnel.netbird_custom_domain",
            "tunnel.netbird_name_prefix",
            "tunnel.custom_command",
            "tunnel.custom_health_url",
            "tunnel.custom_url_pattern",
            "tunnel.public_url",
        ] {
            assert!(app_js.contains(key), "missing networking setting: {key}");
        }
    }

    #[test]
    fn chat_ui_separates_live_work_reasoning_and_final_response() {
        let app_js = include_str!("static/app.js");
        assert!(app_js.contains("addEventListener('reasoning_update'"));
        assert!(app_js.contains("activity-live-work-output"));
        assert!(app_js.contains("createReasoningSummaryElement(turn.reasoning, false)"));
        assert!(app_js.contains("finishStreamingWork(data.thread_id)"));
        assert!(!app_js.contains("finalizeStreamingMessage(data.thread_id, data.content)"));
    }

    /// Build a test router with just the OAuth callback route.
    fn test_oauth_router(state: Arc<GatewayState>) -> Router {
        Router::new()
            .route("/oauth/callback", get(oauth_callback_handler))
            .with_state(state)
    }

    #[tokio::test]
    async fn test_extensions_setup_submit_returns_failure_when_not_activated() {
        use axum::body::Body;
        use tower::ServiceExt;

        let secrets = test_secrets_store();
        let (ext_mgr, _wasm_tools_dir, wasm_channels_dir) = test_ext_mgr(secrets);

        let channel_name = "test-failing-channel";
        std::fs::write(
            wasm_channels_dir
                .path()
                .join(format!("{channel_name}.wasm")),
            b"\0asm fake",
        )
        .expect("write fake wasm");
        let caps = serde_json::json!({
            "type": "channel",
            "name": channel_name,
            "setup": {
                "required_secrets": [
                    {"name": "BOT_TOKEN", "prompt": "Enter bot token"}
                ]
            }
        });
        std::fs::write(
            wasm_channels_dir
                .path()
                .join(format!("{channel_name}.capabilities.json")),
            serde_json::to_string(&caps).expect("serialize caps"),
        )
        .expect("write capabilities");

        let state = test_gateway_state(Some(ext_mgr));
        let app = Router::new()
            .route(
                "/api/extensions/{name}/setup",
                post(extensions_setup_submit_handler),
            )
            .with_state(state);

        let req_body = serde_json::json!({
            "secrets": {
                "BOT_TOKEN": "dummy-token"
            }
        });
        let mut req = axum::http::Request::builder()
            .method("POST")
            .uri(format!("/api/extensions/{channel_name}/setup"))
            .header("content-type", "application/json")
            .body(Body::from(req_body.to_string()))
            .expect("request");
        // Inject AuthenticatedUser so the handler's extractor succeeds
        // without needing the full auth middleware layer.
        req.extensions_mut().insert(UserIdentity {
            user_id: "test".to_string(),
            workspace_read_scopes: Vec::new(),
        });

        let resp = ServiceExt::<axum::http::Request<Body>>::oneshot(app, req)
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json response");
        assert_eq!(parsed["success"], serde_json::Value::Bool(false));
        assert_eq!(parsed["activated"], serde_json::Value::Bool(false));
        assert!(
            parsed["message"]
                .as_str()
                .unwrap_or_default()
                .contains("Activation failed"),
            "expected activation failure in message: {:?}",
            parsed
        );
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn test_extensions_deactivate_preserves_mcp_config() {
        use axum::body::Body;
        use tower::ServiceExt;

        let (store, db_dir) = crate::testing::test_db().await;
        let ext_mgr = Arc::new(ExtensionManager::new(
            Arc::new(crate::tools::mcp::session::McpSessionManager::new()),
            Arc::new(crate::tools::mcp::process::McpProcessManager::new()),
            test_secrets_store(),
            Arc::new(ToolRegistry::new()),
            None,
            None,
            db_dir.path().join("tools"),
            db_dir.path().join("channels"),
            None,
            "test".to_string(),
            Some(Arc::clone(&store)),
            Vec::new(),
        ));
        ext_mgr
            .install_mcp_config(
                crate::tools::mcp::McpServerConfig::new_stdio(
                    "local-files",
                    "cat",
                    Vec::new(),
                    std::collections::HashMap::new(),
                ),
                "test",
            )
            .await
            .expect("install test MCP");

        let app = Router::new()
            .route(
                "/api/extensions/{name}/deactivate",
                post(extensions_deactivate_handler),
            )
            .with_state(test_gateway_state(Some(ext_mgr)));
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/extensions/local-files/deactivate")
            .body(Body::empty())
            .expect("request");
        request.extensions_mut().insert(UserIdentity {
            user_id: "test".to_string(),
            workspace_read_scopes: Vec::new(),
        });

        let response = ServiceExt::<axum::http::Request<Body>>::oneshot(app, request)
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 64)
            .await
            .expect("body");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("JSON response");
        assert_eq!(payload["success"], true);

        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("reload MCP config");
        assert!(!stored.get("local-files").expect("config preserved").enabled);
    }

    fn expired_flow_created_at() -> Option<std::time::Instant> {
        std::time::Instant::now()
            .checked_sub(oauth_defaults::OAUTH_FLOW_EXPIRY + std::time::Duration::from_secs(1))
    }

    #[tokio::test]
    async fn test_csp_header_present_on_responses() {
        use crate::channels::web::auth::MultiAuthState;
        use std::net::SocketAddr;

        let state = test_gateway_state(None);

        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let auth = MultiAuthState::single("test-token".to_string(), "test".to_string());
        let bound = start_server_with_mascot(addr, state.clone(), auth.into(), false)
            .await
            .expect("server should start");

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{}/api/health", bound))
            .send()
            .await
            .expect("health request should succeed");

        assert_eq!(resp.status(), 200);

        let csp = resp
            .headers()
            .get("content-security-policy")
            .expect("CSP header must be present");

        let csp_str = csp.to_str().expect("CSP header should be valid UTF-8");
        assert!(
            csp_str.contains("default-src 'self'"),
            "CSP must contain default-src"
        );
        assert!(
            csp_str.contains("script-src 'self';"),
            "CSP script-src must be 'self' only (marked/DOMPurify are vendored)"
        );
        assert!(
            !csp_str.contains("cdn.jsdelivr.net") && !csp_str.contains("cdnjs.cloudflare.com"),
            "CSP must not reference script CDNs (libraries are vendored)"
        );
        assert!(
            csp_str.contains("object-src 'none'"),
            "CSP must contain object-src 'none'"
        );
        assert!(
            csp_str.contains("frame-ancestors 'none'"),
            "CSP must contain frame-ancestors 'none'"
        );
        // Fonts are self-hosted; the CSP must NOT reach out to Google's font
        // CDNs and must allow same-origin fonts.
        assert!(
            csp_str.contains("font-src 'self'"),
            "CSP must serve fonts from 'self' (self-hosted, no external CDN)"
        );
        assert!(
            !csp_str.contains("fonts.googleapis.com") && !csp_str.contains("fonts.gstatic.com"),
            "CSP must not reference Google Fonts CDNs"
        );

        if let Some(tx) = state.shutdown_tx.write().await.take() {
            let _ = tx.send(());
        }
    }

    #[tokio::test]
    async fn mascot_routes_follow_enabled_flag() {
        use crate::channels::web::auth::MultiAuthState;

        let client = reqwest::Client::new();
        let script = r#"<script src="/mascot.js" defer></script>"#;
        for mascot_enabled in [false, true] {
            let state = test_gateway_state(None);
            let addr: SocketAddr = "127.0.0.1:0".parse().expect("test address");
            let auth = MultiAuthState::single("test-token".to_string(), "test".to_string());
            let bound = start_server_with_mascot(addr, state.clone(), auth.into(), mascot_enabled)
                .await
                .expect("server should start");

            let index = client
                .get(format!("http://{bound}/"))
                .send()
                .await
                .expect("index request")
                .text()
                .await
                .expect("index body");
            assert_eq!(index.contains(script), mascot_enabled);

            let mascot_status = client
                .get(format!("http://{bound}/mascot.js"))
                .send()
                .await
                .expect("mascot request")
                .status();
            assert_eq!(
                mascot_status.as_u16(),
                if mascot_enabled { 200 } else { 404 }
            );

            if let Some(tx) = state.shutdown_tx.write().await.take() {
                let _ = tx.send(());
            }
        }
    }

    #[tokio::test]
    async fn test_oauth_callback_missing_params() {
        use axum::body::Body;
        use tower::ServiceExt;

        let state = test_gateway_state(None);
        let app = test_oauth_router(state);

        let req = axum::http::Request::builder()
            .uri("/oauth/callback")
            .body(Body::empty())
            .expect("request");

        let resp = ServiceExt::<axum::http::Request<Body>>::oneshot(app, req)
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .expect("body");
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Authorization Failed"));
    }

    #[tokio::test]
    async fn test_oauth_callback_error_from_provider() {
        use axum::body::Body;
        use tower::ServiceExt;

        let state = test_gateway_state(None);
        let app = test_oauth_router(state);

        let req = axum::http::Request::builder()
            .uri("/oauth/callback?error=access_denied&error_description=access_denied")
            .body(Body::empty())
            .expect("request");

        let resp = ServiceExt::<axum::http::Request<Body>>::oneshot(app, req)
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .expect("body");
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Authorization Failed"));
    }

    #[tokio::test]
    async fn test_oauth_callback_unknown_state() {
        use axum::body::Body;
        use tower::ServiceExt;

        // Build an ExtensionManager so the handler can look up flows
        let secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(crate::secrets::InMemorySecretsStore::new(Arc::new(
                crate::secrets::SecretsCrypto::new(secrecy::SecretString::from(
                    TEST_GATEWAY_CRYPTO_KEY.to_string(),
                ))
                .expect("crypto"),
            )));
        let (ext_mgr, _wasm_tools_dir, _wasm_channels_dir) = test_ext_mgr(secrets);

        let state = test_gateway_state(Some(ext_mgr));
        let app = test_oauth_router(state);

        let req = axum::http::Request::builder()
            .uri("/oauth/callback?code=test_code&state=unknown_state_value")
            .body(Body::empty())
            .expect("request");

        let resp = ServiceExt::<axum::http::Request<Body>>::oneshot(app, req)
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .expect("body");
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Authorization Failed"));
    }

    #[tokio::test]
    async fn test_oauth_callback_expired_flow() {
        use axum::body::Body;
        use tower::ServiceExt;

        let secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(crate::secrets::InMemorySecretsStore::new(Arc::new(
                crate::secrets::SecretsCrypto::new(secrecy::SecretString::from(
                    TEST_GATEWAY_CRYPTO_KEY.to_string(),
                ))
                .expect("crypto"),
            )));
        let (ext_mgr, _wasm_tools_dir, _wasm_channels_dir) = test_ext_mgr(secrets.clone());
        let Some(created_at) = expired_flow_created_at() else {
            eprintln!("Skipping expired OAuth flow test: monotonic uptime below expiry window");
            return;
        };

        // Insert an expired flow.
        let flow = crate::cli::oauth_defaults::PendingOAuthFlow {
            extension_name: "test_tool".to_string(),
            display_name: "Test Tool".to_string(),
            token_url: "https://example.com/token".to_string(),
            client_id: "client123".to_string(),
            client_secret: None,
            redirect_uri: "https://example.com/oauth/callback".to_string(),
            code_verifier: None,
            access_token_field: "access_token".to_string(),
            secret_name: "test_token".to_string(),
            provider: None,
            validation_endpoint: None,
            scopes: vec![],
            user_id: "test".to_string(),
            secrets,
            sse_manager: None,
            gateway_token: None,
            token_exchange_extra_params: std::collections::HashMap::new(),
            client_id_secret_name: None,
            client_secret_secret_name: None,
            client_secret_expires_at: None,
            auto_activate_extension: false,
            created_at,
        };

        ext_mgr
            .pending_oauth_flows()
            .write()
            .await
            .insert("expired_state".to_string(), flow);

        let state = test_gateway_state(Some(ext_mgr));
        let app = test_oauth_router(state);

        let req = axum::http::Request::builder()
            .uri("/oauth/callback?code=test_code&state=expired_state")
            .body(Body::empty())
            .expect("request");

        let resp = ServiceExt::<axum::http::Request<Body>>::oneshot(app, req)
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .expect("body");
        let html = String::from_utf8_lossy(&body);
        // Expired flow → error landing page
        assert!(html.contains("Authorization Failed"));
    }

    #[tokio::test]
    async fn test_oauth_callback_expired_flow_broadcasts_auth_completed_failure() {
        use axum::body::Body;
        use tower::ServiceExt;

        let secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(crate::secrets::InMemorySecretsStore::new(Arc::new(
                crate::secrets::SecretsCrypto::new(secrecy::SecretString::from(
                    TEST_GATEWAY_CRYPTO_KEY.to_string(),
                ))
                .expect("crypto"),
            )));
        let (ext_mgr, _wasm_tools_dir, _wasm_channels_dir) = test_ext_mgr(secrets.clone());

        let sse_mgr = Arc::new(SseManager::new());
        let mut receiver = sse_mgr.sender().subscribe();
        let Some(created_at) = expired_flow_created_at() else {
            eprintln!("Skipping expired OAuth flow SSE test: monotonic uptime below expiry window");
            return;
        };
        let flow = crate::cli::oauth_defaults::PendingOAuthFlow {
            extension_name: "test_tool".to_string(),
            display_name: "Test Tool".to_string(),
            token_url: "https://example.com/token".to_string(),
            client_id: "client123".to_string(),
            client_secret: None,
            redirect_uri: "https://example.com/oauth/callback".to_string(),
            code_verifier: None,
            access_token_field: "access_token".to_string(),
            secret_name: "test_token".to_string(),
            provider: None,
            validation_endpoint: None,
            scopes: vec![],
            user_id: "test".to_string(),
            secrets,
            sse_manager: Some(sse_mgr),
            gateway_token: None,
            token_exchange_extra_params: std::collections::HashMap::new(),
            client_id_secret_name: None,
            client_secret_secret_name: None,
            client_secret_expires_at: None,
            auto_activate_extension: false,
            created_at,
        };

        ext_mgr
            .pending_oauth_flows()
            .write()
            .await
            .insert("expired_state".to_string(), flow);

        let state = test_gateway_state(Some(ext_mgr));
        let app = test_oauth_router(state);

        let req = axum::http::Request::builder()
            .uri("/oauth/callback?code=test_code&state=expired_state")
            .body(Body::empty())
            .expect("request");

        let resp = ServiceExt::<axum::http::Request<Body>>::oneshot(app, req)
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        match receiver.recv().await.expect("auth_completed event").event {
            crate::channels::web::types::SseEvent::AuthCompleted {
                extension_name,
                success,
                message,
            } => {
                assert_eq!(extension_name, "test_tool");
                assert!(!success, "expired OAuth flow should broadcast failure");
                assert_eq!(message, "OAuth flow expired. Please try again.");
            }
            event => panic!("expected AuthCompleted event, got {event:?}"),
        }
    }

    #[tokio::test]
    async fn test_oauth_callback_no_extension_manager() {
        use axum::body::Body;
        use tower::ServiceExt;

        // No extension manager set → graceful error
        let state = test_gateway_state(None);
        let app = test_oauth_router(state);

        let req = axum::http::Request::builder()
            .uri("/oauth/callback?code=test_code&state=some_state")
            .body(Body::empty())
            .expect("request");

        let resp = ServiceExt::<axum::http::Request<Body>>::oneshot(app, req)
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .expect("body");
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Authorization Failed"));
    }

    #[tokio::test]
    async fn test_oauth_callback_strips_instance_prefix() {
        use axum::body::Body;
        use tower::ServiceExt;

        let secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(crate::secrets::InMemorySecretsStore::new(Arc::new(
                crate::secrets::SecretsCrypto::new(secrecy::SecretString::from(
                    TEST_GATEWAY_CRYPTO_KEY.to_string(),
                ))
                .expect("crypto"),
            )));
        let (ext_mgr, _wasm_tools_dir, _wasm_channels_dir) = test_ext_mgr(secrets.clone());

        // Insert a flow keyed by raw nonce "test_nonce" (without instance prefix).
        // Use an expired flow so the handler exits before attempting a real HTTP
        // token exchange — we only need to verify that the instance prefix was
        // stripped and the flow was found by the raw nonce.
        let Some(created_at) = expired_flow_created_at() else {
            eprintln!("Skipping OAuth state-prefix test: monotonic uptime below expiry window");
            return;
        };
        let flow = crate::cli::oauth_defaults::PendingOAuthFlow {
            extension_name: "test_tool".to_string(),
            display_name: "Test Tool".to_string(),
            token_url: "https://example.com/token".to_string(),
            client_id: "client123".to_string(),
            client_secret: None,
            redirect_uri: "https://example.com/oauth/callback".to_string(),
            code_verifier: None,
            access_token_field: "access_token".to_string(),
            secret_name: "test_token".to_string(),
            provider: None,
            validation_endpoint: None,
            scopes: vec![],
            user_id: "test".to_string(),
            secrets,
            sse_manager: None,
            gateway_token: None,
            token_exchange_extra_params: std::collections::HashMap::new(),
            client_id_secret_name: None,
            client_secret_secret_name: None,
            client_secret_expires_at: None,
            auto_activate_extension: false,
            // Expired — handler will reject after lookup (no network I/O)
            created_at,
        };

        ext_mgr
            .pending_oauth_flows()
            .write()
            .await
            .insert("test_nonce".to_string(), flow);

        let state = test_gateway_state(Some(ext_mgr.clone()));
        let app = test_oauth_router(state);

        // Send callback with instance prefix: "myinstance:test_nonce"
        // The handler should strip "myinstance:" and find the flow keyed by "test_nonce"
        let req = axum::http::Request::builder()
            .uri("/oauth/callback?code=fake_code&state=myinstance:test_nonce")
            .body(Body::empty())
            .expect("request");

        let resp = ServiceExt::<axum::http::Request<Body>>::oneshot(app, req)
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .expect("body");
        let html = String::from_utf8_lossy(&body);

        // The flow was found (stripped prefix matched) but is expired, so the
        // handler returns an error landing page. The flow being consumed from
        // the registry (checked below) proves the prefix was stripped correctly.
        assert!(
            html.contains("Authorization Failed"),
            "Expected error page, html was: {}",
            &html[..html.len().min(500)]
        );

        // Verify the flow was consumed (removed from registry)
        assert!(
            ext_mgr
                .pending_oauth_flows()
                .read()
                .await
                .get("test_nonce")
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_oauth_callback_accepts_versioned_hosted_state() {
        use axum::body::Body;
        use tower::ServiceExt;

        let secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(crate::secrets::InMemorySecretsStore::new(Arc::new(
                crate::secrets::SecretsCrypto::new(secrecy::SecretString::from(
                    TEST_GATEWAY_CRYPTO_KEY.to_string(),
                ))
                .expect("crypto"),
            )));
        let (ext_mgr, _wasm_tools_dir, _wasm_channels_dir) = test_ext_mgr(secrets.clone());

        let Some(created_at) = expired_flow_created_at() else {
            eprintln!("Skipping versioned OAuth state test: monotonic uptime below expiry window");
            return;
        };
        let flow = crate::cli::oauth_defaults::PendingOAuthFlow {
            extension_name: "test_tool".to_string(),
            display_name: "Test Tool".to_string(),
            token_url: "https://example.com/token".to_string(),
            client_id: "client123".to_string(),
            client_secret: None,
            redirect_uri: "https://example.com/oauth/callback".to_string(),
            code_verifier: None,
            access_token_field: "access_token".to_string(),
            secret_name: "test_token".to_string(),
            provider: None,
            validation_endpoint: None,
            scopes: vec![],
            user_id: "test".to_string(),
            secrets,
            sse_manager: None,
            gateway_token: None,
            token_exchange_extra_params: std::collections::HashMap::new(),
            client_id_secret_name: None,
            client_secret_secret_name: None,
            client_secret_expires_at: None,
            auto_activate_extension: false,
            created_at,
        };

        ext_mgr
            .pending_oauth_flows()
            .write()
            .await
            .insert("test_nonce".to_string(), flow);

        let state = test_gateway_state(Some(ext_mgr.clone()));
        let app = test_oauth_router(state);
        let versioned_state =
            crate::cli::oauth_defaults::encode_hosted_oauth_state("test_nonce", Some("myinstance"));

        let req = axum::http::Request::builder()
            .uri(format!(
                "/oauth/callback?code=fake_code&state={}",
                urlencoding::encode(&versioned_state)
            ))
            .body(Body::empty())
            .expect("request");

        let resp = ServiceExt::<axum::http::Request<Body>>::oneshot(app, req)
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .expect("body");
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Authorization Failed"));
        assert!(
            ext_mgr
                .pending_oauth_flows()
                .read()
                .await
                .get("test_nonce")
                .is_none()
        );
    }

    fn test_secrets_store() -> Arc<dyn crate::secrets::SecretsStore + Send + Sync> {
        Arc::new(crate::secrets::InMemorySecretsStore::new(Arc::new(
            crate::secrets::SecretsCrypto::new(secrecy::SecretString::from(
                "test-key-at-least-32-chars-long!!".to_string(),
            ))
            .expect("crypto"),
        )))
    }

    fn test_ext_mgr(
        secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync>,
    ) -> (Arc<ExtensionManager>, tempfile::TempDir, tempfile::TempDir) {
        let tool_registry = Arc::new(ToolRegistry::new());
        let mcp_sm = Arc::new(crate::tools::mcp::session::McpSessionManager::new());
        let mcp_pm = Arc::new(crate::tools::mcp::process::McpProcessManager::new());
        let wasm_tools_dir = tempfile::tempdir().expect("temp wasm tools dir");
        let wasm_channels_dir = tempfile::tempdir().expect("temp wasm channels dir");
        let ext_mgr = Arc::new(ExtensionManager::new(
            mcp_sm,
            mcp_pm,
            secrets,
            tool_registry,
            None,
            None,
            wasm_tools_dir.path().to_path_buf(),
            wasm_channels_dir.path().to_path_buf(),
            None,
            "test".to_string(),
            None,
            vec![],
        ));
        (ext_mgr, wasm_tools_dir, wasm_channels_dir)
    }

    #[test]
    fn test_is_local_origin_localhost() {
        assert!(is_local_origin("http://localhost:3001"));
        assert!(is_local_origin("http://localhost"));
        assert!(is_local_origin("https://localhost:3001"));
    }

    #[test]
    fn test_is_local_origin_ipv4() {
        assert!(is_local_origin("http://127.0.0.1:3001"));
        assert!(is_local_origin("http://127.0.0.1"));
    }

    #[test]
    fn test_is_local_origin_ipv6() {
        assert!(is_local_origin("http://[::1]:3001"));
        assert!(is_local_origin("http://[::1]"));
    }

    #[test]
    fn test_is_local_origin_rejects_remote() {
        assert!(!is_local_origin("http://evil.com"));
        assert!(!is_local_origin("http://localhost.evil.com"));
        assert!(!is_local_origin("http://192.168.1.1:3001"));
    }

    #[test]
    fn test_is_local_origin_rejects_garbage() {
        assert!(!is_local_origin("not-a-url"));
        assert!(!is_local_origin(""));
    }

    // ---- TUI gateway contract (design 4.2, 8.3.1-8.3.4) ----

    use crate::channels::web::sse::Correlation;
    use crate::channels::web::test_helpers::TestGatewayBuilder;

    const CHAT_TOKEN: &str = "chat-contract-test-token";
    const THREAD: &str = "5f0e4a52-6a3e-4a8f-9a1e-2b9d6c1e7a10";
    const TURN: &str = "0193f2a1-7c3e-4d4a-b1e2-6f0c9a8b7d6e";
    /// A 1x1 PNG.
    const TINY_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    async fn chat_gateway() -> (SocketAddr, mpsc::Receiver<IncomingMessage>) {
        let (tx, rx) = mpsc::channel(8);
        let (addr, _state) = TestGatewayBuilder::new()
            .msg_tx(tx)
            .start(CHAT_TOKEN)
            .await
            .expect("gateway starts");
        (addr, rx)
    }

    async fn post_chat(
        addr: SocketAddr,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, String) {
        let response = reqwest::Client::new()
            .post(format!("http://{addr}{path}"))
            .bearer_auth(CHAT_TOKEN)
            .json(&body)
            .send()
            .await
            .expect("request reaches the gateway");
        let status = response.status();
        (status, response.text().await.expect("response body"))
    }

    async fn next_message(rx: &mut mpsc::Receiver<IncomingMessage>) -> IncomingMessage {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("a message is queued in time")
            .expect("message channel open")
    }

    fn image(media_type: &str, data: &str) -> ImageData {
        ImageData {
            media_type: media_type.to_string(),
            data: data.to_string(),
        }
    }

    fn test_user() -> UserIdentity {
        UserIdentity {
            user_id: "test-user".to_string(),
            workspace_read_scopes: Vec::new(),
        }
    }

    /// A gateway whose state has a session manager (for V1 approval lookups).
    fn session_gateway(sessions: Arc<SessionManager>) -> crate::channels::web::GatewayChannel {
        let config = crate::config::GatewayConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            auth_token: Some(CHAT_TOKEN.to_string()),
            mascot_enabled: false,
            user_id: "test-user".to_string(),
            workspace_read_scopes: Vec::new(),
            memory_layers: Vec::new(),
            user_tokens: None,
            ws_ping_interval_secs: 30,
            ws_idle_timeout_secs: 120,
        };
        crate::channels::web::GatewayChannel::new(config, "test-user".to_string())
            .with_session_manager(sessions)
    }

    #[test]
    fn validate_client_turn_id_accepts_absent_and_valid_ids_only() {
        assert_eq!(validate_client_turn_id(None), Ok(None));
        assert_eq!(validate_client_turn_id(Some(TURN)), Ok(Some(TURN)));
        assert_eq!(validate_client_turn_id(Some("c-0007")), Ok(Some("c-0007")));
        let too_long = "a".repeat(65);
        for bad in ["", "a.b", "a:b", "a b", "caf\u{e9}", too_long.as_str()] {
            assert_eq!(
                validate_client_turn_id(Some(bad)),
                Err((
                    StatusCode::BAD_REQUEST,
                    "invalid client_turn_id".to_string()
                )),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn gateway_message_metadata_shape() {
        let id = Uuid::new_v4();
        assert_eq!(
            gateway_message_metadata("alice", THREAD, id, Some(TURN)),
            serde_json::json!({
                "user_id": "alice",
                "thread_id": THREAD,
                "message_id": id.to_string(),
                "client_turn_id": TURN,
            })
        );
        assert_eq!(
            gateway_message_metadata("alice", THREAD, id, None),
            serde_json::json!({"user_id": "alice", "thread_id": THREAD, "message_id": id.to_string()})
        );
    }

    #[tokio::test]
    async fn chat_send_stamps_message_id_and_client_turn_id_in_metadata() {
        let (addr, mut rx) = chat_gateway().await;
        let (status, body) = post_chat(
            addr,
            "/api/chat/send",
            serde_json::json!({"content": "hi", "thread_id": THREAD, "client_turn_id": TURN}),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let accepted: serde_json::Value = serde_json::from_str(&body).expect("JSON body");

        let msg = next_message(&mut rx).await;
        assert_eq!(
            accepted["message_id"],
            serde_json::json!(msg.id.to_string())
        );
        assert_eq!(msg.thread_id.as_deref(), Some(THREAD));
        assert_eq!(
            msg.metadata,
            gateway_message_metadata("test-user", THREAD, msg.id, Some(TURN))
        );
        assert_eq!(
            Correlation::from_metadata(&msg.metadata),
            Some(Correlation {
                message_id: msg.id.to_string(),
                client_turn_id: Some(TURN.to_string()),
            })
        );

        // Without a client id the server id alone correlates the message.
        let (status, _) = post_chat(
            addr,
            "/api/chat/send",
            serde_json::json!({"content": "hi", "thread_id": THREAD}),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let msg = next_message(&mut rx).await;
        assert_eq!(
            msg.metadata,
            gateway_message_metadata("test-user", THREAD, msg.id, None)
        );
    }

    #[tokio::test]
    async fn chat_send_rejects_invalid_client_turn_id() {
        let (addr, mut rx) = chat_gateway().await;
        let too_long = "a".repeat(65);
        for bad in ["", "a.b", "a b", "../x", too_long.as_str()] {
            let (status, body) = post_chat(
                addr,
                "/api/chat/send",
                serde_json::json!({"content": "hi", "thread_id": THREAD, "client_turn_id": bad}),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
            assert_eq!(body, "invalid client_turn_id");
        }
        // A non-string id is a JSON shape error, rejected by the extractor.
        let (status, _) = post_chat(
            addr,
            "/api/chat/send",
            serde_json::json!({"content": "hi", "thread_id": THREAD, "client_turn_id": 7}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(rx.try_recv().is_err(), "rejected sends are not queued");
    }

    #[tokio::test]
    async fn approval_stamps_owner_metadata_and_original_client_turn_id() {
        use crate::agent::session::{PendingApproval, ThreadState};

        let sessions = Arc::new(SessionManager::new());
        let (session, thread_id) = sessions.resolve_thread("test-user", "gateway", None).await;
        let request_id = Uuid::new_v4();
        {
            let mut sess = session.lock().await;
            let thread = sess.threads.get_mut(&thread_id).expect("resolved thread");
            thread.state = ThreadState::AwaitingApproval;
            thread.pending_approval = Some(PendingApproval {
                request_id,
                tool_name: "shell".into(),
                parameters: serde_json::json!({}),
                display_parameters: serde_json::json!({}),
                description: "run".into(),
                tool_call_id: "call_1".into(),
                context_messages: Vec::new(),
                deferred_tool_calls: Vec::new(),
                user_timezone: None,
                allow_always: true,
            });
        }
        let gateway = session_gateway(sessions);
        let state = Arc::clone(gateway.state());
        let (tx, mut rx) = mpsc::channel(4);
        *state.msg_tx.write().await = Some(tx);

        let request = |client_turn_id: Option<&str>| ApprovalRequest {
            request_id: request_id.to_string(),
            action: "approve".to_string(),
            thread_id: None,
            client_turn_id: client_turn_id.map(str::to_string),
        };
        let error = chat_approval_handler(
            State(Arc::clone(&state)),
            AuthenticatedUser(test_user()),
            Json(request(Some("a.b"))),
        )
        .await
        .expect_err("an invalid client id is refused");
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(rx.try_recv().is_err());

        let (status, Json(accepted)) = chat_approval_handler(
            State(Arc::clone(&state)),
            AuthenticatedUser(test_user()),
            Json(request(Some(TURN))),
        )
        .await
        .expect("approval accepted");
        assert_eq!(status, StatusCode::ACCEPTED);
        let msg = next_message(&mut rx).await;
        assert_eq!(accepted.message_id, msg.id);
        let thread = thread_id.to_string();
        assert_eq!(msg.thread_id.as_deref(), Some(thread.as_str()));
        // The approval gets its own message id but the owning turn's client id.
        assert_eq!(
            msg.metadata,
            gateway_message_metadata("test-user", &thread, msg.id, Some(TURN))
        );
    }

    #[tokio::test]
    async fn auth_token_resume_stamps_client_turn_id() {
        let (tx, mut rx) = mpsc::channel(4);
        let state = TestGatewayBuilder::new().msg_tx(tx).build();
        let pending = pending_gate_view(lunarwing_engine::ResumeKind::Authentication {
            credential_name: "notion".to_string(),
            instructions: "Paste a token".to_string(),
            auth_url: None,
        });

        let queued = queue_engine_auth_resume(
            &state,
            "test-user",
            "notion",
            "fixture-credential",
            Some(&pending),
            Some(TURN),
        )
        .await
        .expect("resume queued");
        assert!(queued);
        let msg = next_message(&mut rx).await;
        assert_eq!(msg.thread_id.as_deref(), Some(pending.thread_id.as_str()));
        assert_eq!(
            msg.metadata,
            gateway_message_metadata("test-user", &pending.thread_id, msg.id, Some(TURN))
        );
        let submission = msg.auth_gate_submission.as_ref().expect("gate submission");
        assert_eq!(submission.credential_name, "notion");
        assert_eq!(submission.request_id.to_string(), pending.request_id);

        // Without an owning turn the resume still carries its own message id.
        queue_engine_auth_resume(
            &state,
            "test-user",
            "notion",
            "fixture-credential",
            Some(&pending),
            None,
        )
        .await
        .expect("resume queued");
        let msg = next_message(&mut rx).await;
        assert_eq!(
            msg.metadata,
            gateway_message_metadata("test-user", &pending.thread_id, msg.id, None)
        );

        // No pending engine gate: nothing to resume.
        let queued = queue_engine_auth_resume(
            &state,
            "test-user",
            "notion",
            "fixture-credential",
            None,
            Some(TURN),
        )
        .await
        .expect("no resume needed");
        assert!(!queued);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn auth_cancel_rejects_invalid_client_turn_id() {
        let (addr, _rx) = chat_gateway().await;
        for path in ["/api/chat/auth-cancel", "/api/chat/auth-token"] {
            let (status, body) = post_chat(
                addr,
                path,
                serde_json::json!({"extension_name": "notion", "token": "x", "client_turn_id": "a.b"}),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(body, "invalid client_turn_id", "{path}");
        }
        // A valid id is accepted; with nothing pending the cancel succeeds.
        let (status, body) = post_chat(
            addr,
            "/api/chat/auth-cancel",
            serde_json::json!({"extension_name": "notion", "client_turn_id": TURN}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    #[tokio::test]
    async fn gateway_images_reject_svg_with_422() {
        use base64::Engine;

        let (addr, mut rx) = chat_gateway().await;
        let svg = base64::engine::general_purpose::STANDARD
            .encode("<svg xmlns=\"http://www.w3.org/2000/svg\"/>");
        let (status, body) = post_chat(
            addr,
            "/api/chat/send",
            serde_json::json!({
                "content": "", "thread_id": THREAD,
                "images": [{"media_type": "image/png", "data": TINY_PNG_B64},
                           {"media_type": "image/svg+xml", "data": svg}],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body,
            "image 2 rejected: unsupported image type (allowed: png, jpeg, gif, webp)"
        );
        assert!(
            rx.try_recv().is_err(),
            "an invalid image refuses the whole message"
        );
    }

    #[tokio::test]
    async fn gateway_images_reject_oversize_with_413() {
        let (addr, mut rx) = chat_gateway().await;
        // One base64 quad over 5 MiB decoded: refused before decoding.
        let oversize = "A".repeat(4 * (5 * 1024 * 1024usize).div_ceil(3) + 4);
        let (status, body) = post_chat(
            addr,
            "/api/chat/send",
            serde_json::json!({
                "content": "", "thread_id": THREAD,
                "images": [{"media_type": "image/png", "data": oversize}],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body, "image 1 rejected: larger than 5242880 bytes");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn gateway_images_reject_count_total_and_encoding() {
        let png = image("image/png", TINY_PNG_B64);
        let six = vec![png.clone(); 6];
        assert_eq!(
            gateway_image_attachments(&six).err(),
            Some((
                StatusCode::UNPROCESSABLE_ENTITY,
                "image 6 rejected: too many images (6, at most 5)".to_string()
            ))
        );

        // Each image is under 5 MiB, together they exceed 7 MiB: refused before decoding.
        let half = "A".repeat(4_893_360);
        let total = [image("image/png", &half), image("image/png", &half)];
        assert_eq!(
            gateway_image_attachments(&total).err(),
            Some((
                StatusCode::PAYLOAD_TOO_LARGE,
                "images rejected: the images exceed 7340032 bytes in total".to_string()
            ))
        );

        let cases = [
            (
                image("image/png", "not base64!!"),
                "image 1 rejected: invalid base64",
            ),
            (image("image/png", ""), "image 1 rejected: no image data"),
            (
                image("text/plain", TINY_PNG_B64),
                "image 1 rejected: unsupported image type (allowed: png, jpeg, gif, webp)",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                gateway_image_attachments(&[input]).err(),
                Some((StatusCode::UNPROCESSABLE_ENTITY, expected.to_string()))
            );
        }
        assert_eq!(gateway_image_attachments(&[]).map(|a| a.len()), Ok(0));
    }

    #[tokio::test]
    async fn gateway_images_use_sniffed_mime() {
        let (addr, mut rx) = chat_gateway().await;
        let (status, body) = post_chat(
            addr,
            "/api/chat/send",
            serde_json::json!({
                "content": "what is this?", "thread_id": THREAD, "client_turn_id": TURN,
                "images": [{"media_type": "image/jpeg", "data": TINY_PNG_B64}],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let msg = next_message(&mut rx).await;
        let [attachment] = msg.attachments.as_slice() else {
            panic!("expected one attachment, got {}", msg.attachments.len());
        };
        assert_eq!(attachment.mime_type, "image/png");
        assert_eq!(attachment.filename.as_deref(), Some("image-1.png"));
        assert_eq!(attachment.kind, crate::channels::AttachmentKind::Image);
        assert_eq!(attachment.size_bytes, Some(attachment.data.len() as u64));
    }

    #[tokio::test]
    async fn gateway_image_ids_are_unique_across_requests() {
        let (addr, mut rx) = chat_gateway().await;
        let mut ids = std::collections::HashSet::new();
        for _ in 0..2 {
            let (status, body) = post_chat(
                addr,
                "/api/chat/send",
                serde_json::json!({
                    "content": "", "thread_id": THREAD,
                    "images": [{"media_type": "image/png", "data": TINY_PNG_B64},
                               {"media_type": "image/png", "data": TINY_PNG_B64}],
                }),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
            for attachment in next_message(&mut rx).await.attachments {
                let uuid = attachment
                    .id
                    .strip_prefix("web-image-")
                    .expect("web-image- prefix");
                assert!(Uuid::parse_str(uuid).is_ok(), "{}", attachment.id);
                assert!(ids.insert(attachment.id), "attachment ids must be unique");
            }
        }
        assert_eq!(ids.len(), 4);
    }

    #[test]
    fn chat_capabilities_reports_engine_features_limits_voice() {
        let full = serde_json::to_value(chat_capabilities(true, true)).expect("JSON");
        assert_eq!(
            full,
            serde_json::json!({
                "api_version": 1,
                "server_version": env!("CARGO_PKG_VERSION"),
                "engine": "v2",
                "features": ["status", "attachments", "transcribe", "stream"],
                "limits": {
                    "max_content_bytes": 100000,
                    "max_attachments": 5,
                    "max_attachment_bytes": 5242880,
                    "max_total_attachment_bytes": 7340032,
                    "attachment_mime_types": ["image/png", "image/jpeg", "image/gif", "image/webp"],
                    "max_transcribe_b64_bytes": 25165824,
                    "transcribe_formats": ["wav", "ogg", "opus", "mp3", "mp4", "webm", "flac", "m4a", "aac"],
                    "messages_per_minute": 30,
                    "transcribes_per_minute": 10,
                    "max_request_bytes": 10485760
                },
                "voice": {"provider": "lunarvoice", "stt": true, "tts": true},
                "credential": "master"
            })
        );

        let bare = serde_json::to_value(chat_capabilities(false, false)).expect("JSON");
        assert_eq!(bare["engine"], "v1");
        assert_eq!(
            bare["features"],
            serde_json::json!(["status", "attachments"])
        );
        assert_eq!(bare["limits"]["max_transcribe_b64_bytes"], 0);
        assert_eq!(bare["limits"]["transcribe_formats"], serde_json::json!([]));
        assert_eq!(
            bare["voice"],
            serde_json::json!({"provider": null, "stt": false, "tts": false})
        );
        assert_eq!(bare["credential"], "master");

        let v2_without_voice = serde_json::to_value(chat_capabilities(false, true)).expect("JSON");
        assert_eq!(
            v2_without_voice["features"],
            serde_json::json!(["status", "attachments", "stream"])
        );
    }

    #[test]
    fn per_minute_scales_the_window() {
        assert_eq!(per_minute((30, 60)), 30);
        assert_eq!(per_minute((10, 30)), 20);
        assert_eq!(per_minute((10, 120)), 5);
        assert_eq!(per_minute((10, 0)), 600);
    }
}
