//! Central extension manager that dispatches operations by ExtensionKind.
//!
//! Holds references to channel runtime, WASM tool runtime, MCP infrastructure,
//! secrets store, and tool registry. All extension operations (search, install,
//! auth, activate, list, remove) flow through here.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rand::Rng;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::channels::ChannelManager;
use crate::channels::wasm::{
    LoadedChannel, RegisteredEndpoint, SharedWasmChannel, WasmChannelLoader, WasmChannelRouter,
    WasmChannelRuntime,
};
use crate::extensions::discovery::OnlineDiscovery;
use crate::extensions::registry::ExtensionRegistry;
use crate::extensions::{
    ActivateResult, AuthResult, ConfigureResult, ExtensionError, ExtensionKind, ExtensionSource,
    InstallResult, InstalledExtension, RegistryEntry, ResultSource, SearchResult, ToolAuthState,
    UpgradeOutcome, UpgradeResult,
};
use crate::hooks::HookRegistry;
use crate::pairing::PairingStore;
use crate::secrets::{CreateSecretParams, SecretsStore};
use crate::tools::mcp::McpClient;
use crate::tools::mcp::auth::{
    authorize_mcp_server, canonical_resource_uri, discover_full_oauth_metadata,
    find_available_port, is_authenticated, register_client,
};
use crate::tools::mcp::config::{McpPlacement, McpServerConfig};
use crate::tools::mcp::session::McpSessionManager;
use crate::tools::wasm::{WasmToolLoader, WasmToolRuntime, discover_tools};
use crate::tools::{Tool, ToolRegistry};

/// Pending OAuth authorization state.
struct PendingAuth {
    _name: String,
    _kind: ExtensionKind,
    created_at: std::time::Instant,
    /// Background task listening for the OAuth callback.
    /// Aborted when a new auth flow starts for the same extension.
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

struct HostedOAuthFlowStart {
    name: String,
    kind: ExtensionKind,
    auth_url: String,
    expected_state: String,
    flow: crate::cli::oauth_defaults::PendingOAuthFlow,
}

fn normalize_oauth_callback_path(path: &str) -> String {
    let trimmed_path = path.trim_end_matches('/');
    if trimmed_path.is_empty() {
        "/oauth/callback".to_string()
    } else if trimmed_path.ends_with("/oauth/callback") {
        trimmed_path.to_string()
    } else {
        format!("{trimmed_path}/oauth/callback")
    }
}

fn normalize_hosted_callback_url(callback_url: &str) -> String {
    if let Ok(mut parsed) = url::Url::parse(callback_url) {
        let normalized_path = normalize_oauth_callback_path(parsed.path());
        parsed.set_path(&normalized_path);
        return parsed.to_string();
    }

    let normalized_callback_url = callback_url.trim_end_matches('/');
    if normalized_callback_url.ends_with("/oauth/callback") {
        normalized_callback_url.to_string()
    } else {
        format!("{normalized_callback_url}/oauth/callback")
    }
}

/// Runtime infrastructure needed for hot-activating WASM channels.
///
/// Set after construction via [`ExtensionManager::set_channel_runtime`] once the
/// channel manager, WASM runtime, pairing store, and webhook router are available.
struct ChannelRuntimeState {
    channel_manager: Arc<ChannelManager>,
    wasm_channel_runtime: Arc<WasmChannelRuntime>,
    pairing_store: Arc<PairingStore>,
    wasm_channel_router: Arc<WasmChannelRouter>,
    wasm_channel_owner_ids: std::collections::HashMap<String, i64>,
    wasm_channel_owner_actor_ids: std::collections::HashMap<String, String>,
    /// Global `tool_activity` flag forwarded to hot-activated WASM channels.
    tool_activity: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum McpRuntimeHealth {
    Healthy,
    Recovering,
    RetryPending,
    CircuitOpen,
}

struct ActiveMcpServer {
    owner_id: String,
    client: Arc<McpClient>,
    tools: Vec<Arc<dyn Tool>>,
    tools_generation: u64,
    managed_stdio: bool,
    managed_wasm: bool,
    generation: u64,
    health: McpRuntimeHealth,
    restart_attempts: u32,
    healthy_since: Instant,
    retry_after: Option<Instant>,
}

struct McpRecoveryCandidate {
    name: String,
    owner_id: String,
    generation: u64,
}

struct PreparedMcpRuntime {
    client: Arc<McpClient>,
    tools: Vec<Arc<dyn Tool>>,
}

struct McpRecoveryTask {
    generation: u64,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

enum McpRecoveryClaim {
    Skip,
    Recover(Vec<Arc<dyn Tool>>),
    Retry {
        dead_tools: Vec<Arc<dyn Tool>>,
        attempts: u32,
        delay: Duration,
    },
    CircuitOpen {
        dead_tools: Vec<Arc<dyn Tool>>,
        attempts: u32,
    },
}

enum McpRecoveryAbort {
    Cancelled,
    TimedOut,
}

const MCP_SUPERVISOR_INTERVAL: Duration = Duration::from_secs(2);
const MCP_RESTART_BACKOFF_BASE: Duration = Duration::from_secs(1);
const MCP_RESTART_BACKOFF_MAX: Duration = Duration::from_secs(60);
const MCP_RESTART_MAX_ATTEMPTS: u32 = 5;
const MCP_RESTART_STABILITY_WINDOW: Duration = Duration::from_secs(30);
const MCP_RECOVERY_STEP_TIMEOUT: Duration = Duration::from_secs(45);
const MCP_SUPERVISOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

async fn join_or_abort_tasks(
    handles: &mut [tokio::task::JoinHandle<()>],
    timeout: Duration,
) -> bool {
    if tokio::time::timeout(timeout, futures::future::join_all(handles.iter_mut()))
        .await
        .is_ok()
    {
        return false;
    }
    for handle in handles.iter() {
        handle.abort();
    }
    let _ = futures::future::join_all(handles.iter_mut()).await;
    true
}

fn mcp_restart_backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(6);
    let capped = MCP_RESTART_BACKOFF_BASE
        .checked_mul(1_u32 << exponent)
        .unwrap_or(MCP_RESTART_BACKOFF_MAX)
        .min(MCP_RESTART_BACKOFF_MAX);
    let max_millis = capped.as_millis() as u64;
    let min_millis = (max_millis / 2).max(1);
    Duration::from_millis(rand::thread_rng().gen_range(min_millis..=max_millis))
}

async fn await_mcp_recovery_step<T>(
    cancel: &CancellationToken,
    future: impl std::future::Future<Output = T>,
) -> Result<T, McpRecoveryAbort> {
    tokio::select! {
        _ = cancel.cancelled() => Err(McpRecoveryAbort::Cancelled),
        result = tokio::time::timeout(MCP_RECOVERY_STEP_TIMEOUT, future) => {
            result.map_err(|_| McpRecoveryAbort::TimedOut)
        }
    }
}

#[derive(Default)]
struct StoppedMcpRuntime {
    tool_names: Vec<String>,
    cleanup_warning: Option<String>,
}

/// Setup schema returned to web UI for extension configuration.
pub struct ExtensionSetupSchema {
    pub secrets: Vec<crate::channels::web::types::SecretFieldInfo>,
    pub fields: Vec<crate::channels::web::types::SetupFieldInfo>,
}

/// Only these global (non-namespaced) setting paths may be written by extension
/// setup fields. Everything else must be under `extensions.<name>.*`.
const ALLOWED_GLOBAL_SETUP_SETTING_PATHS: &[&str] = &[
    "llm_backend",
    "selected_model",
    "ollama_base_url",
    "openai_compatible_base_url",
];

fn build_wasm_channel_runtime_config_updates(
    tunnel_url: Option<&str>,
    webhook_secret: Option<&str>,
    owner_id: Option<i64>,
) -> HashMap<String, serde_json::Value> {
    let mut config_updates = HashMap::new();

    if let Some(tunnel_url) = tunnel_url {
        config_updates.insert(
            "tunnel_url".to_string(),
            serde_json::Value::String(tunnel_url.to_string()),
        );
    }

    if let Some(secret) = webhook_secret {
        config_updates.insert(
            "webhook_secret".to_string(),
            serde_json::Value::String(secret.to_string()),
        );
    }

    if let Some(owner_id) = owner_id {
        config_updates.insert("owner_id".to_string(), serde_json::json!(owner_id));
    }

    config_updates
}

fn channel_auth_instructions(secret: &crate::channels::wasm::SecretSetupSchema) -> String {
    secret.prompt.clone()
}

/// Central manager for extension lifecycle operations.
pub struct ExtensionManager {
    registry: ExtensionRegistry,
    discovery: OnlineDiscovery,

    // MCP infrastructure
    mcp_session_manager: Arc<McpSessionManager>,
    mcp_process_manager: Arc<crate::tools::mcp::process::McpProcessManager>,
    /// Serializes MCP runtime transitions for each server name.
    mcp_lifecycle_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Active MCP clients and their exact registered tools, keyed by server name.
    active_mcp_servers: RwLock<HashMap<String, ActiveMcpServer>>,
    /// Monotonic runtime identity used to reject stale crash-recovery work.
    mcp_runtime_generation: AtomicU64,
    /// Background host-local stdio recovery supervisor and cancellation signal.
    mcp_supervisor: Mutex<Option<(CancellationToken, tokio::task::JoinHandle<()>)>>,
    /// One cancellable recovery task per managed stdio server.
    mcp_recovery_tasks: Mutex<HashMap<String, McpRecoveryTask>>,

    // WASM tool infrastructure
    wasm_tool_runtime: Option<Arc<WasmToolRuntime>>,
    wasm_tools_dir: PathBuf,
    wasm_channels_dir: PathBuf,

    // WASM channel hot-activation infrastructure (set post-construction)
    channel_runtime: RwLock<Option<ChannelRuntimeState>>,

    // Shared
    secrets: Arc<dyn SecretsStore + Send + Sync>,
    tool_registry: Arc<ToolRegistry>,
    hooks: Option<Arc<HookRegistry>>,
    pending_auth: RwLock<HashMap<(String, String), PendingAuth>>,
    /// Tunnel URL for webhook configuration and remote OAuth callbacks.
    tunnel_url: std::sync::RwLock<Option<String>>,
    user_id: String,
    /// Optional database store for DB-backed MCP config.
    store: Option<Arc<dyn crate::db::Database>>,
    /// Names of WASM channels that were successfully loaded at startup.
    active_channel_names: RwLock<HashSet<String>>,
    /// Last activation error for each WASM channel (ephemeral, cleared on success).
    activation_errors: RwLock<HashMap<String, String>>,
    /// SSE broadcast manager (set post-construction via `set_sse_sender()`).
    sse_manager: RwLock<Option<Arc<crate::channels::web::sse::SseManager>>>,
    /// Shared registry of pending OAuth flows for gateway-routed callbacks.
    ///
    /// Keyed by CSRF `state` parameter. Populated in `start_wasm_oauth()`
    /// when running in gateway mode, consumed by the web gateway's
    /// `/oauth/callback` handler.
    pending_oauth_flows: crate::cli::oauth_defaults::PendingOAuthRegistry,
    /// Gateway auth token for authenticating with the platform token exchange proxy.
    /// Read once at construction from `GATEWAY_AUTH_TOKEN` env var.
    gateway_token: Option<String>,
    /// When `true`, OAuth flows always return an auth URL to the caller
    /// instead of opening a browser on the server via `open::that()`.
    /// Set by the web gateway at startup via `enable_gateway_mode()`.
    gateway_mode: std::sync::atomic::AtomicBool,
    /// The gateway's own base URL for building OAuth redirect URIs.
    /// Set by the web gateway at startup via `enable_gateway_mode()`.
    gateway_base_url: RwLock<Option<String>>,
}

/// Sanitize a URL for logging by removing query parameters and credentials.
/// Prevents accidental logging of API keys, OAuth tokens, or other sensitive data in URLs.
fn sanitize_url_for_logging(url: &str) -> String {
    // If URL is very short or doesn't look like a URL, just use as-is
    if url.len() < 10 || !url.contains("://") {
        return url.to_string();
    }

    // Try to parse and remove sensitive components
    if let Ok(mut parsed) = url::Url::parse(url) {
        // Remove query string and fragment
        parsed.set_query(None);
        parsed.set_fragment(None);

        // Remove userinfo (username and password) if present
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);

        parsed.to_string()
    } else {
        // Fallback: strip after ? or #
        url.split(['?', '#']).next().unwrap_or(url).to_string()
    }
}

impl ExtensionManager {
    pub fn owner_id(&self) -> &str {
        &self.user_id
    }

    pub async fn active_tool_names(&self) -> HashSet<String> {
        let mut names = HashSet::new();
        match self.list(None, false, &self.user_id).await {
            Ok(extensions) => {
                for extension in extensions {
                    match extension.kind {
                        ExtensionKind::WasmTool if extension.active => {
                            names.insert(extension.name);
                        }
                        ExtensionKind::McpServer if extension.active => {
                            names.extend(extension.tools);
                        }
                        _ => {}
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    owner_id = %self.user_id,
                    "Failed to list active extensions while resolving autonomous tool scope: {}",
                    err
                );
            }
        }
        names
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mcp_session_manager: Arc<McpSessionManager>,
        mcp_process_manager: Arc<crate::tools::mcp::process::McpProcessManager>,
        secrets: Arc<dyn SecretsStore + Send + Sync>,
        tool_registry: Arc<ToolRegistry>,
        hooks: Option<Arc<HookRegistry>>,
        wasm_tool_runtime: Option<Arc<WasmToolRuntime>>,
        wasm_tools_dir: PathBuf,
        wasm_channels_dir: PathBuf,
        tunnel_url: Option<String>,
        user_id: String,
        store: Option<Arc<dyn crate::db::Database>>,
        catalog_entries: Vec<RegistryEntry>,
    ) -> Self {
        let registry = if catalog_entries.is_empty() {
            ExtensionRegistry::new()
        } else {
            ExtensionRegistry::new_with_catalog(catalog_entries)
        };
        Self {
            registry,
            discovery: OnlineDiscovery::new(),
            mcp_session_manager,
            mcp_process_manager,
            mcp_lifecycle_locks: Mutex::new(HashMap::new()),
            active_mcp_servers: RwLock::new(HashMap::new()),
            mcp_runtime_generation: AtomicU64::new(1),
            mcp_supervisor: Mutex::new(None),
            mcp_recovery_tasks: Mutex::new(HashMap::new()),
            wasm_tool_runtime,
            wasm_tools_dir,
            wasm_channels_dir,
            channel_runtime: RwLock::new(None),
            secrets,
            tool_registry,
            hooks,
            pending_auth: RwLock::new(HashMap::new()),
            tunnel_url: std::sync::RwLock::new(tunnel_url),
            user_id,
            store,
            active_channel_names: RwLock::new(HashSet::new()),
            activation_errors: RwLock::new(HashMap::new()),
            sse_manager: RwLock::new(None),
            pending_oauth_flows: crate::cli::oauth_defaults::new_pending_oauth_registry(),
            gateway_token: std::env::var("GATEWAY_AUTH_TOKEN").ok(),
            gateway_mode: std::sync::atomic::AtomicBool::new(false),
            gateway_base_url: RwLock::new(None),
        }
    }

    fn next_mcp_runtime_generation(&self) -> u64 {
        self.mcp_runtime_generation.fetch_add(1, Ordering::Relaxed)
    }

    /// Start monitoring host-local stdio MCP runtimes for unexpected exits.
    ///
    /// The task keeps only a weak manager reference between checks, so it cannot
    /// keep application state alive after normal shutdown.
    pub async fn start_mcp_supervisor(self: &Arc<Self>) {
        let mut supervisor = self.mcp_supervisor.lock().await;
        if supervisor
            .as_ref()
            .is_some_and(|(_, handle)| !handle.is_finished())
        {
            return;
        }
        if let Some((cancel, stale)) = supervisor.take() {
            cancel.cancel();
            let _ = stale.await;
        }

        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let manager = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(MCP_SUPERVISOR_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    _ = interval.tick() => {}
                }
                let Some(manager) = manager.upgrade() else {
                    break;
                };
                manager.supervise_mcp_runtimes_once().await;
            }
        });
        *supervisor = Some((cancel, handle));
    }

    /// Stop crash recovery before application shutdown tears down MCP children.
    pub async fn shutdown_mcp_supervisor(&self) {
        let supervisor = self.mcp_supervisor.lock().await.take();
        if let Some((cancel, mut supervisor)) = supervisor {
            cancel.cancel();
            if tokio::time::timeout(MCP_SUPERVISOR_SHUTDOWN_TIMEOUT, &mut supervisor)
                .await
                .is_err()
            {
                tracing::warn!("MCP supervisor did not stop before timeout; aborting task");
                supervisor.abort();
                let _ = supervisor.await;
            }
        }

        let tasks = {
            let mut active = self.mcp_recovery_tasks.lock().await;
            active.drain().map(|(_, task)| task).collect::<Vec<_>>()
        };
        for task in &tasks {
            task.cancel.cancel();
        }
        let mut handles = tasks
            .into_iter()
            .map(|task| task.handle)
            .collect::<Vec<_>>();
        if join_or_abort_tasks(&mut handles, MCP_SUPERVISOR_SHUTDOWN_TIMEOUT).await {
            tracing::warn!("MCP recovery tasks did not stop before timeout; aborting them");
        }
    }

    /// Stop supervision, unregister every MCP wrapper, and close all live runtimes.
    pub async fn shutdown_mcp_runtimes(&self) {
        self.shutdown_mcp_supervisor().await;
        let runtimes = self
            .active_mcp_servers
            .write()
            .await
            .drain()
            .collect::<Vec<_>>();
        for (_, runtime) in &runtimes {
            for tool in &runtime.tools {
                self.tool_registry
                    .unregister_if_same(tool.name(), tool)
                    .await;
            }
        }
        let shutdowns = runtimes.iter().map(|(name, runtime)| async move {
            (
                name,
                self.shutdown_mcp_runtime(name, Some(runtime.client.as_ref()))
                    .await,
            )
        });
        for (name, result) in futures::future::join_all(shutdowns).await {
            if let Err(error) = result {
                tracing::warn!(server = %name, %error, "MCP runtime shutdown was incomplete");
            }
        }
    }

    async fn supervise_mcp_runtimes_once(self: &Arc<Self>) {
        let candidates = {
            let runtimes = self.active_mcp_servers.read().await;
            runtimes
                .iter()
                .filter(|(_, runtime)| {
                    runtime.managed_stdio
                        || runtime.managed_wasm
                        || runtime.client.tools_generation() != runtime.tools_generation
                })
                .map(|(name, runtime)| McpRecoveryCandidate {
                    name: name.clone(),
                    owner_id: runtime.owner_id.clone(),
                    generation: runtime.generation,
                })
                .collect::<Vec<_>>()
        };

        for candidate in candidates {
            self.spawn_mcp_recovery_task(candidate).await;
        }
    }

    async fn spawn_mcp_recovery_task(self: &Arc<Self>, candidate: McpRecoveryCandidate) {
        let mut tasks = self.mcp_recovery_tasks.lock().await;
        if tasks
            .get(&candidate.name)
            .is_some_and(|task| !task.handle.is_finished())
        {
            return;
        }
        tasks.remove(&candidate.name);

        let name = candidate.name.clone();
        let task_name = name.clone();
        let generation = candidate.generation;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let manager = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            if let Some(manager) = manager.upgrade() {
                manager
                    .recover_mcp_runtime_if_needed(candidate, task_cancel)
                    .await;
                manager
                    .finish_mcp_recovery_task(&task_name, generation)
                    .await;
            }
        });
        tasks.insert(
            name,
            McpRecoveryTask {
                generation,
                cancel,
                handle,
            },
        );
    }

    async fn finish_mcp_recovery_task(&self, name: &str, generation: u64) {
        let mut tasks = self.mcp_recovery_tasks.lock().await;
        if tasks
            .get(name)
            .is_some_and(|task| task.generation == generation)
        {
            tasks.remove(name);
        }
    }

    /// Update URL discovered from a managed tunnel after application assembly.
    pub fn set_tunnel_url(&self, tunnel_url: Option<String>) {
        if let Ok(mut current) = self.tunnel_url.write() {
            *current = tunnel_url;
        }
    }

    fn current_tunnel_url(&self) -> Option<String> {
        self.tunnel_url
            .read()
            .ok()
            .and_then(|current| current.clone())
    }

    /// Enable gateway mode so OAuth flows return auth URLs to the frontend
    /// instead of calling `open::that()` on the server.
    ///
    /// `base_url` is the gateway's own public URL (e.g. `https://my-gateway.example.com`),
    /// used to build OAuth redirect URIs when `LUNARWING_OAUTH_CALLBACK_URL` (legacy
    /// `IRONCLAW_OAUTH_CALLBACK_URL`) is not set.
    pub async fn enable_gateway_mode(&self, base_url: String) {
        self.gateway_mode
            .store(true, std::sync::atomic::Ordering::Release);
        *self.gateway_base_url.write().await = Some(base_url);
    }

    /// Returns `true` if OAuth should use gateway mode (return auth URL to
    /// frontend) rather than CLI mode (open browser on server via `open::that`).
    ///
    /// Gateway mode is active when any of:
    /// - `enable_gateway_mode()` was called (web gateway is running), OR
    /// - `LUNARWING_OAUTH_CALLBACK_URL` (legacy `IRONCLAW_OAUTH_CALLBACK_URL`) is set to a non-loopback URL, OR
    /// - `self.tunnel_url` is set to a non-loopback URL
    pub fn should_use_gateway_mode(&self) -> bool {
        if self.gateway_mode.load(std::sync::atomic::Ordering::Acquire) {
            return true;
        }
        if crate::cli::oauth_defaults::use_gateway_callback() {
            return true;
        }
        self.current_tunnel_url()
            .as_ref()
            .filter(|u| !u.is_empty())
            .and_then(|raw| url::Url::parse(raw).ok())
            .and_then(|u| u.host_str().map(String::from))
            .map(|host| !crate::cli::oauth_defaults::is_loopback_host(&host))
            .unwrap_or(false)
    }

    /// Returns the OAuth redirect URI for gateway mode, or `None` for local mode.
    ///
    /// Priority:
    /// 1. `LUNARWING_OAUTH_CALLBACK_URL` env var (legacy `IRONCLAW_OAUTH_CALLBACK_URL`; via `callback_url()`)
    /// 2. `gateway_base_url` (set by `enable_gateway_mode()`)
    /// 3. `tunnel_url` (from config)
    /// 4. `None` (local/CLI mode)
    async fn gateway_callback_redirect_uri(&self) -> Option<String> {
        use crate::cli::oauth_defaults;
        if oauth_defaults::use_gateway_callback() {
            return Some(normalize_hosted_callback_url(
                &oauth_defaults::callback_url(),
            ));
        }
        // Use gateway_base_url from enable_gateway_mode()
        if let Some(ref base) = *self.gateway_base_url.read().await {
            let base = base.trim_end_matches('/');
            return Some(format!("{}/oauth/callback", base));
        }
        // Fall back to tunnel_url
        self.current_tunnel_url()
            .as_ref()
            .filter(|u| !u.is_empty())
            .and_then(|raw| {
                let url = url::Url::parse(raw).ok()?;
                let host = url.host_str().map(String::from)?;
                if oauth_defaults::is_loopback_host(&host) {
                    return None;
                }
                let base = raw.trim_end_matches('/');
                Some(format!("{}/oauth/callback", base))
            })
    }

    /// Inject a registry entry for testing. The entry is added to the discovery
    /// cache so it appears in search results alongside built-in entries.
    pub async fn inject_registry_entry(&self, entry: crate::extensions::RegistryEntry) {
        self.registry.cache_discovered(vec![entry]).await;
    }

    /// Configure the channel runtime infrastructure for hot-activating WASM channels.
    ///
    /// Call after construction (and after wrapping in `Arc`) once the channel
    /// manager, WASM runtime, pairing store, and webhook router are available.
    /// Without this, channel activation returns an error.
    pub async fn set_channel_runtime(
        &self,
        channel_manager: Arc<ChannelManager>,
        wasm_channel_runtime: Arc<WasmChannelRuntime>,
        pairing_store: Arc<PairingStore>,
        wasm_channel_router: Arc<WasmChannelRouter>,
        wasm_channel_owner_ids: std::collections::HashMap<String, i64>,
        wasm_channel_owner_actor_ids: std::collections::HashMap<String, String>,
    ) {
        *self.channel_runtime.write().await = Some(ChannelRuntimeState {
            channel_manager,
            wasm_channel_runtime,
            pairing_store,
            wasm_channel_router,
            wasm_channel_owner_ids,
            wasm_channel_owner_actor_ids,
            tool_activity: false,
        });
    }

    /// Set the global `tool_activity` flag for hot-activated WASM channels.
    ///
    /// Call after [`Self::set_channel_runtime`]; before runtime state exists
    /// this is a no-op (hot activation is impossible then anyway).
    pub async fn set_channel_tool_activity(&self, tool_activity: bool) {
        if let Some(rt) = self.channel_runtime.write().await.as_mut() {
            rt.tool_activity = tool_activity;
        }
    }

    async fn current_channel_owner_actor_id(&self, name: &str) -> Option<String> {
        {
            let rt_guard = self.channel_runtime.read().await;
            if let Some(actor_id) = rt_guard
                .as_ref()
                .and_then(|rt| rt.wasm_channel_owner_actor_ids.get(name).cloned())
            {
                return Some(actor_id);
            }
        }

        if let Some(store) = self.store.as_ref() {
            let key = format!("channels.wasm_channel_owner_actor_ids.{name}");
            match store.get_setting(&self.user_id, &key).await {
                Ok(Some(serde_json::Value::String(actor_id))) if !actor_id.trim().is_empty() => {
                    return Some(actor_id);
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(error) => {
                    tracing::debug!(
                        channel = %name,
                        %error,
                        "Failed to read persisted WASM channel owner actor ID"
                    );
                }
            }
        }

        self.current_channel_owner_id(name)
            .await
            .map(|owner_id| owner_id.to_string())
    }

    async fn current_channel_owner_id(&self, name: &str) -> Option<i64> {
        {
            let rt_guard = self.channel_runtime.read().await;
            if let Some(owner_id) = rt_guard
                .as_ref()
                .and_then(|rt| rt.wasm_channel_owner_ids.get(name).copied())
            {
                return Some(owner_id);
            }
        }

        let store = self.store.as_ref()?;
        let key = format!("channels.wasm_channel_owner_ids.{name}");
        match store.get_setting(&self.user_id, &key).await {
            Ok(Some(serde_json::Value::Number(n))) => n.as_i64(),
            Ok(Some(serde_json::Value::String(s))) => s.parse::<i64>().ok(),
            Ok(Some(_)) | Ok(None) => None,
            Err(e) => {
                tracing::debug!(
                    channel = %name,
                    error = %e,
                    "Failed to read persisted wasm channel owner id"
                );
                None
            }
        }
    }

    async fn load_channel_runtime_config_overrides(
        &self,
        name: &str,
    ) -> HashMap<String, serde_json::Value> {
        let mut overrides = HashMap::new();

        let cap_path = self
            .wasm_channels_dir
            .join(format!("{}.capabilities.json", name));
        if cap_path.exists()
            && let Ok(cap_bytes) = tokio::fs::read(&cap_path).await
            && let Ok(cap_file) =
                crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(&cap_bytes)
        {
            let saved_fields = self.load_tool_setup_fields(name).await.unwrap_or_default();
            for field in &cap_file.setup.required_fields {
                if let Some(value) = self
                    .resolve_setup_field_value(name, field, &saved_fields)
                    .await
                {
                    overrides.insert(field.name.clone(), value);
                }
            }
        }

        overrides
    }

    pub async fn has_wasm_channel_owner_binding(&self, name: &str) -> bool {
        self.current_channel_owner_actor_id(name).await.is_some()
    }

    /// Return the durable owner scope for a channel with an external owner binding.
    /// The WASM wrapper resolves that scope through its persisted protocol target.
    pub(crate) async fn notification_target_for_channel(&self, name: &str) -> Option<String> {
        self.has_wasm_channel_owner_binding(name)
            .await
            .then(|| self.user_id.clone())
    }

    /// Access the secrets store (used by OAuth callback handlers).
    pub fn secrets(&self) -> &Arc<dyn SecretsStore + Send + Sync> {
        &self.secrets
    }

    /// Inject a pre-created MCP client (from startup loading) into the manager.
    ///
    /// Startup-loaded MCP clients register their tools in `ToolRegistry` but are
    /// otherwise dropped. This method stores the client so that `list()` reports
    /// accurate "connected" status and reconnection/session management works.
    pub(crate) async fn inject_mcp_client(
        &self,
        name: String,
        client: Arc<crate::tools::mcp::McpClient>,
        tools: Vec<Arc<dyn Tool>>,
    ) {
        if name.is_empty() {
            tracing::warn!("inject_mcp_client called with empty name; ignoring");
            return;
        }
        if let Err(e) = Self::validate_extension_name(&name) {
            tracing::warn!(
                error = %e,
                name = %name,
                "inject_mcp_client called with invalid name; ignoring"
            );
            return;
        }
        let host_config = self
            .get_mcp_server(&name, &self.user_id)
            .await
            .is_ok_and(|server| server.placement == McpPlacement::Host);
        if !host_config {
            for tool in &tools {
                self.tool_registry
                    .unregister_if_same(tool.name(), tool)
                    .await;
            }
            if let Err(error) = self
                .shutdown_mcp_runtime(&name, Some(client.as_ref()))
                .await
            {
                tracing::warn!(server = %name, %error, "Rejected non-host MCP startup injection; cleanup incomplete");
            }
            tracing::warn!(server = %name, "Rejected MCP startup injection without current host placement");
            return;
        }
        let managed_stdio = self.mcp_process_manager.get(&name).await.is_some();
        let managed_wasm = !managed_stdio && client.runtime_is_wasm();
        let generation = self.next_mcp_runtime_generation();
        let tools_generation = client.tools_generation();
        self.active_mcp_servers.write().await.insert(
            name,
            ActiveMcpServer {
                owner_id: self.user_id.clone(),
                client,
                tools,
                tools_generation,
                managed_stdio,
                managed_wasm,
                generation,
                health: McpRuntimeHealth::Healthy,
                restart_attempts: 0,
                healthy_since: Instant::now(),
                retry_after: None,
            },
        );
    }

    /// Register channel names that were loaded at startup.
    /// Called after WASM channels are loaded so `list()` reports accurate active status.
    pub async fn set_active_channels(&self, names: Vec<String>) {
        let mut active = self.active_channel_names.write().await;
        active.extend(names);
    }

    /// Persist the set of active channel names to the settings store.
    ///
    /// Saved under key `activated_channels` so channels auto-activate on restart.
    async fn persist_active_channels(&self, user_id: &str) {
        let Some(ref store) = self.store else {
            return;
        };
        let names: Vec<String> = self
            .active_channel_names
            .read()
            .await
            .iter()
            .cloned()
            .collect();
        let value = serde_json::json!(names);
        if let Err(e) = store
            .set_setting(user_id, "activated_channels", &value)
            .await
        {
            tracing::warn!(error = %e, "Failed to persist activated_channels setting");
        }
    }

    /// Load previously activated channel names from the settings store.
    ///
    /// Returns channel names that were activated in a prior session so they can
    /// be auto-activated at startup.
    pub async fn load_persisted_active_channels(&self, user_id: &str) -> Vec<String> {
        let Some(ref store) = self.store else {
            return Vec::new();
        };
        match store.get_setting(user_id, "activated_channels").await {
            Ok(Some(value)) => match serde_json::from_value(value) {
                Ok(names) => names,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to deserialize activated_channels");
                    Vec::new()
                }
            },
            Ok(None) => Vec::new(),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load activated_channels setting");
                Vec::new()
            }
        }
    }

    /// Set the SSE broadcast sender for pushing extension status events to the web UI.
    pub async fn set_sse_sender(&self, sse: Arc<crate::channels::web::sse::SseManager>) {
        *self.sse_manager.write().await = Some(sse);
    }

    /// Returns the pending OAuth flow registry for sharing with the web gateway.
    ///
    /// The gateway's `/oauth/callback` handler uses this to look up pending flows
    /// by CSRF `state` parameter and complete the token exchange.
    pub fn pending_oauth_flows(&self) -> &crate::cli::oauth_defaults::PendingOAuthRegistry {
        &self.pending_oauth_flows
    }

    pub async fn sse_sender(&self) -> Option<Arc<crate::channels::web::sse::SseManager>> {
        self.sse_manager.read().await.clone()
    }

    /// Return the concrete secret name that would satisfy the current auth
    /// requirement for an installed extension, when one exists.
    pub async fn first_missing_auth_secret_pub(&self, name: &str, user_id: &str) -> Option<String> {
        let kind = self.determine_installed_kind(name, user_id).await.ok()?;
        match kind {
            ExtensionKind::McpServer => {
                let server = self.get_mcp_server(name, user_id).await.ok()?;
                if crate::tools::mcp::auth::is_authenticated(&server, &self.secrets, user_id).await
                {
                    None
                } else {
                    Some(server.token_secret_name())
                }
            }
            ExtensionKind::WasmTool => {
                let cap = self.load_tool_capabilities(name).await?;
                let auth = cap.auth?;
                let token_is_managed = self
                    .secrets
                    .exists(user_id, &auth.secret_name)
                    .await
                    .unwrap_or(false);
                let has_env_token = auth
                    .env_var
                    .as_ref()
                    .is_some_and(|v| std::env::var(v).is_ok());
                if token_is_managed || has_env_token {
                    None
                } else {
                    Some(auth.secret_name)
                }
            }
            ExtensionKind::WasmChannel => {
                // Read channel capabilities file directly
                let cap_path = self
                    .wasm_channels_dir
                    .join(format!("{}.capabilities.json", name));
                let cap_bytes = tokio::fs::read(&cap_path).await.ok()?;
                let cap_file =
                    crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(&cap_bytes).ok()?;
                for secret in &cap_file.setup.required_secrets {
                    if secret.optional {
                        continue;
                    }
                    if !self
                        .secrets
                        .exists(user_id, &secret.name)
                        .await
                        .unwrap_or(false)
                    {
                        return Some(secret.name.clone());
                    }
                }
                None
            }
        }
    }

    async fn clear_pending_extension_auth(&self, name: &str, user_id: &str) {
        {
            let mut pending = self.pending_auth.write().await;
            if let Some(old) = pending.remove(&(name.to_string(), user_id.to_string()))
                && let Some(handle) = old.task_handle
            {
                handle.abort();
            }
        }

        let mut flows = self.pending_oauth_flows.write().await;
        flows.retain(|_, flow| flow.extension_name != name || flow.user_id != user_id);
    }

    fn rewrite_oauth_state_param(
        auth_url: String,
        expected_state: &str,
        hosted_state: &str,
    ) -> String {
        if hosted_state == expected_state {
            return auth_url;
        }

        let Ok(mut parsed) = url::Url::parse(&auth_url) else {
            return auth_url.replace(
                &format!("state={}", urlencoding::encode(expected_state)),
                &format!("state={}", urlencoding::encode(hosted_state)),
            );
        };

        let mut replaced = false;
        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(key, value)| {
                if key == "state" {
                    replaced = true;
                    (key.into_owned(), hosted_state.to_string())
                } else {
                    (key.into_owned(), value.into_owned())
                }
            })
            .collect();

        {
            let mut query_pairs = parsed.query_pairs_mut();
            query_pairs.clear();
            for (key, value) in pairs {
                query_pairs.append_pair(&key, &value);
            }
            if !replaced {
                query_pairs.append_pair("state", hosted_state);
            }
        }

        parsed.to_string()
    }

    async fn start_gateway_oauth_flow(&self, request: HostedOAuthFlowStart) -> AuthResult {
        use crate::cli::oauth_defaults;

        oauth_defaults::sweep_expired_flows(&self.pending_oauth_flows).await;

        let hosted_state = oauth_defaults::build_platform_state(&request.expected_state);
        let auth_url = Self::rewrite_oauth_state_param(
            request.auth_url,
            &request.expected_state,
            &hosted_state,
        );

        let user_id = request.flow.user_id.clone();
        self.pending_oauth_flows
            .write()
            .await
            .insert(request.expected_state, request.flow);

        self.pending_auth.write().await.insert(
            (request.name.clone(), user_id),
            PendingAuth {
                _name: request.name.clone(),
                _kind: request.kind,
                created_at: std::time::Instant::now(),
                task_handle: None,
            },
        );

        AuthResult::awaiting_authorization(
            request.name,
            request.kind,
            auth_url,
            "gateway".to_string(),
        )
    }

    /// Broadcast an extension status change to the web UI via SSE.
    async fn broadcast_extension_status(
        &self,
        user_id: &str,
        name: &str,
        status: &str,
        message: Option<&str>,
    ) {
        if let Some(ref sse) = *self.sse_manager.read().await {
            sse.broadcast_for_user(
                user_id,
                crate::channels::web::types::SseEvent::ExtensionStatus {
                    extension_name: name.to_string(),
                    status: status.to_string(),
                    message: message.map(|m| m.to_string()),
                },
            );
        }
    }

    async fn lock_mcp_lifecycle(&self, name: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .mcp_lifecycle_locks
            .lock()
            .await
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        lock.lock_owned().await
    }

    async fn cancel_mcp_recovery(&self, name: &str) {
        let cancel = self
            .mcp_recovery_tasks
            .lock()
            .await
            .get(name)
            .map(|task| task.cancel.clone());
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
    }

    async fn lock_mcp_lifecycle_for_manual(&self, name: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.cancel_mcp_recovery(name).await;
        self.lock_mcp_lifecycle(name).await
    }

    /// Search for extensions. If `discover` is true, also searches online.
    pub async fn search(
        &self,
        query: &str,
        discover: bool,
    ) -> Result<Vec<SearchResult>, ExtensionError> {
        let mut results = self.registry.search(query).await;

        if discover && results.is_empty() {
            tracing::info!("No built-in results for '{}', searching online...", query);
            let discovered = self.discovery.discover(query).await;

            if !discovered.is_empty() {
                // Cache for future lookups
                self.registry.cache_discovered(discovered.clone()).await;

                // Add to results
                for entry in discovered {
                    results.push(SearchResult {
                        entry,
                        source: ResultSource::Discovered,
                        validated: true,
                    });
                }
            }
        }

        Ok(results)
    }

    /// Install an extension by name (from registry) or by explicit URL.
    pub async fn install(
        &self,
        name: &str,
        url: Option<&str>,
        kind_hint: Option<ExtensionKind>,
        user_id: &str,
    ) -> Result<InstallResult, ExtensionError> {
        let sanitized_url = url.map(sanitize_url_for_logging);
        tracing::info!(extension = %name, url = ?sanitized_url, kind = ?kind_hint, "Installing extension");
        Self::validate_extension_name(name)?;

        // If we have a registry entry, use it (prefer kind_hint to resolve collisions)
        if let Some(entry) = self.registry.get_with_kind(name, kind_hint).await {
            return self.install_from_entry(&entry, user_id).await.map_err(|e| {
                tracing::error!(extension = %name, error = %e, "Extension install failed");
                e
            });
        }

        // If a URL was provided, determine kind and install
        if let Some(url) = url {
            let kind = kind_hint.unwrap_or_else(|| infer_kind_from_url(url));
            return match kind {
                ExtensionKind::McpServer => self.install_mcp_from_url(name, url, user_id).await,
                ExtensionKind::WasmTool => self.install_wasm_tool_from_url(name, url).await,
                ExtensionKind::WasmChannel => {
                    self.install_wasm_channel_from_url(name, url, None).await
                }
            }
            .map_err(|e| {
                let sanitized = sanitize_url_for_logging(url);
                tracing::error!(extension = %name, url = %sanitized, error = %e, "Extension install from URL failed");
                e
            });
        }

        let err = ExtensionError::NotFound(format!(
            "'{}' not found in registry. Try searching with discover:true or provide a URL.",
            name
        ));
        tracing::warn!(extension = %name, "Extension not found in registry");
        Err(err)
    }

    /// Check auth status for an installed extension.
    ///
    /// Read-only for WASM extensions; may initiate OAuth for MCP servers.
    /// To provide secrets, use [`configure()`] instead.
    pub async fn auth(&self, name: &str, user_id: &str) -> Result<AuthResult, ExtensionError> {
        // Clean up expired pending auths
        self.cleanup_expired_auths().await;

        // Determine what kind of extension this is
        let kind = self.determine_installed_kind(name, user_id).await?;

        match kind {
            ExtensionKind::McpServer if self.should_use_gateway_mode() => {
                let _lifecycle_guard = self.lock_mcp_lifecycle(name).await;
                self.auth_mcp(name, user_id).await
            }
            ExtensionKind::McpServer => self.auth_mcp(name, user_id).await,
            ExtensionKind::WasmTool => self.auth_wasm_tool(name, user_id).await,
            ExtensionKind::WasmChannel => self.auth_wasm_channel_status(name, user_id).await,
        }
    }

    /// Activate an installed (and optionally authenticated) extension.
    pub async fn activate(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<ActivateResult, ExtensionError> {
        Self::validate_extension_name(name)?;
        let kind = self.determine_installed_kind(name, user_id).await?;

        match kind {
            ExtensionKind::McpServer => self.activate_mcp(name, user_id).await,
            ExtensionKind::WasmTool => self.activate_wasm_tool(name, user_id).await,
            ExtensionKind::WasmChannel => self.activate_wasm_channel(name, user_id).await,
        }
    }

    /// Activate after OAuth only when an MCP server remains enabled.
    pub async fn activate_after_auth(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<Option<ActivateResult>, ExtensionError> {
        Self::validate_extension_name(name)?;
        let kind = self.determine_installed_kind(name, user_id).await?;
        match kind {
            ExtensionKind::McpServer => self.activate_mcp_if_enabled(name, user_id).await,
            ExtensionKind::WasmTool | ExtensionKind::WasmChannel => {
                self.activate(name, user_id).await.map(Some)
            }
        }
    }

    /// Deactivate an MCP server without deleting its configuration or credentials.
    pub async fn deactivate(&self, name: &str, user_id: &str) -> Result<String, ExtensionError> {
        Self::validate_extension_name(name)?;
        let kind = self.determine_installed_kind(name, user_id).await?;
        if kind != ExtensionKind::McpServer {
            return Err(ExtensionError::Other(format!(
                "Only MCP servers can be deactivated; '{name}' is a {kind}"
            )));
        }

        let _lifecycle_guard = self.lock_mcp_lifecycle_for_manual(name).await;
        let server = self.set_mcp_enabled(name, user_id, false).await?;
        self.clear_pending_extension_auth(name, user_id).await;
        let stopped = self.stop_mcp_runtime(name, user_id).await;
        let mut message = if server.placement == McpPlacement::JobWorker {
            format!(
                "Disabled job-worker MCP server '{name}' for future Docker jobs; running jobs retain immutable MCP snapshots; cleaned up {} stale host tool(s)",
                stopped.tool_names.len()
            )
        } else {
            format!(
                "Deactivated host MCP server '{name}' and unloaded {} tool(s); configuration preserved",
                stopped.tool_names.len()
            )
        };
        if let Some(warning) = stopped.cleanup_warning {
            message.push_str(&format!(". Runtime cleanup warning: {warning}"));
        }
        self.broadcast_extension_status(user_id, name, "inactive", Some(&message))
            .await;

        Ok(message)
    }

    /// Atomically toggle an MCP server and apply the corresponding runtime transition.
    pub async fn toggle_mcp(&self, name: &str, user_id: &str) -> Result<String, ExtensionError> {
        Self::validate_extension_name(name)?;
        let kind = self.determine_installed_kind(name, user_id).await?;
        if kind != ExtensionKind::McpServer {
            return Err(ExtensionError::Other(format!(
                "Only MCP servers can be toggled; '{name}' is a {kind}"
            )));
        }

        let _lifecycle_guard = self.lock_mcp_lifecycle_for_manual(name).await;
        let server = if let Some(ref store) = self.store {
            crate::tools::mcp::config::toggle_mcp_server_enabled_db(store.as_ref(), user_id, name)
                .await
        } else {
            crate::tools::mcp::config::toggle_mcp_server_enabled(name).await
        }
        .map_err(|error| ExtensionError::Config(error.to_string()))?;

        if server.placement == McpPlacement::JobWorker {
            self.clear_pending_extension_auth(name, user_id).await;
            let stopped = self.stop_mcp_runtime(name, user_id).await;
            let mut message = if server.enabled {
                format!(
                    "Enabled job-worker MCP server '{name}' for future Docker jobs; running jobs retain immutable MCP snapshots"
                )
            } else {
                format!(
                    "Disabled job-worker MCP server '{name}' for future Docker jobs; running jobs retain immutable MCP snapshots"
                )
            };
            if !stopped.tool_names.is_empty() {
                message.push_str(&format!(
                    "; cleaned up {} stale host tool(s)",
                    stopped.tool_names.len()
                ));
            }
            if let Some(warning) = stopped.cleanup_warning {
                message.push_str(&format!(". Runtime cleanup warning: {warning}"));
            }
            let status = if server.enabled {
                "enabled"
            } else {
                "inactive"
            };
            self.broadcast_extension_status(user_id, name, status, Some(&message))
                .await;
            return Ok(message);
        }

        if server.enabled {
            if let Err(error) = self.ensure_mcp_owner_available(name, user_id).await {
                return Err(self
                    .rollback_mcp_activation_unless_auth_required(user_id, &server, error)
                    .await);
            }
            return match self
                .activate_mcp_locked(name, user_id, server.clone())
                .await
            {
                Ok(result) => Ok(result.message),
                Err(error) => Err(self
                    .rollback_mcp_activation_unless_auth_required(user_id, &server, error)
                    .await),
            };
        }

        self.clear_pending_extension_auth(name, user_id).await;
        let stopped = self.stop_mcp_runtime(name, user_id).await;
        let mut message = format!(
            "Deactivated MCP server '{name}' and unloaded {} tool(s); configuration preserved",
            stopped.tool_names.len()
        );
        if let Some(warning) = stopped.cleanup_warning {
            message.push_str(&format!(". Runtime cleanup warning: {warning}"));
        }
        self.broadcast_extension_status(user_id, name, "inactive", Some(&message))
            .await;
        Ok(message)
    }

    /// List extensions with their status.
    ///
    /// When `include_available` is `true`, registry entries that are not yet
    /// installed are appended with `installed: false`.
    pub async fn list(
        &self,
        kind_filter: Option<ExtensionKind>,
        include_available: bool,
        user_id: &str,
    ) -> Result<Vec<InstalledExtension>, ExtensionError> {
        let mut extensions = Vec::new();

        // List MCP servers
        if kind_filter.is_none() || kind_filter == Some(ExtensionKind::McpServer) {
            match self.load_mcp_servers(user_id).await {
                Ok(servers) => {
                    for server in &servers.servers {
                        let authenticated = is_authenticated(server, &self.secrets, user_id).await;
                        let worker_placement = server.placement == McpPlacement::JobWorker;
                        let (health, tools) = if worker_placement {
                            (None, Vec::new())
                        } else {
                            let active_servers = self.active_mcp_servers.read().await;
                            let runtime = active_servers
                                .get(&server.name)
                                .filter(|runtime| runtime.owner_id == user_id);
                            let tools = runtime
                                .filter(|runtime| runtime.health == McpRuntimeHealth::Healthy)
                                .map(|runtime| {
                                    runtime
                                        .tools
                                        .iter()
                                        .map(|tool| tool.name().to_string())
                                        .collect()
                                })
                                .unwrap_or_default();
                            (runtime.map(|runtime| runtime.health), tools)
                        };
                        let active = health == Some(McpRuntimeHealth::Healthy);
                        let recovering = matches!(
                            health,
                            Some(McpRuntimeHealth::Recovering | McpRuntimeHealth::RetryPending)
                        );

                        let display_name = self
                            .registry
                            .get_with_kind(&server.name, Some(ExtensionKind::McpServer))
                            .await
                            .map(|e| e.display_name);
                        extensions.push(InstalledExtension {
                            name: server.name.clone(),
                            kind: ExtensionKind::McpServer,
                            display_name,
                            description: server.description.clone(),
                            url: match server.effective_transport() {
                                crate::tools::mcp::config::EffectiveTransport::Http
                                | crate::tools::mcp::config::EffectiveTransport::Gateway { .. } => {
                                    Some(server.url.clone())
                                }
                                _ => None,
                            },
                            transport: Some(
                                match server.effective_transport() {
                                    crate::tools::mcp::config::EffectiveTransport::Http => "http",
                                    crate::tools::mcp::config::EffectiveTransport::Stdio {
                                        ..
                                    } => "stdio",
                                    crate::tools::mcp::config::EffectiveTransport::Unix {
                                        ..
                                    } => "unix",
                                    crate::tools::mcp::config::EffectiveTransport::Wasm {
                                        ..
                                    } => "wasm",
                                    crate::tools::mcp::config::EffectiveTransport::Gateway {
                                        ..
                                    } => "gateway",
                                    crate::tools::mcp::config::EffectiveTransport::External {
                                        ..
                                    } => "external",
                                }
                                .to_string(),
                            ),
                            command: match server.effective_transport() {
                                crate::tools::mcp::config::EffectiveTransport::Stdio {
                                    command,
                                    ..
                                } => Some(command.to_string()),
                                _ => None,
                            },
                            placement: Some(server.placement),
                            authenticated: !matches!(
                                server.effective_transport(),
                                crate::tools::mcp::config::EffectiveTransport::Http
                                    | crate::tools::mcp::config::EffectiveTransport::Gateway { .. }
                            ) || authenticated,
                            active,
                            recovering,
                            enabled: Some(if worker_placement {
                                server.should_run_in_job()
                            } else {
                                server.enabled
                            }),
                            tools,
                            needs_setup: false,
                            has_auth: false,
                            installed: true,
                            activation_error: (health == Some(McpRuntimeHealth::CircuitOpen)
                                || server.recovery_circuit_open)
                                .then(|| {
                                    if worker_placement {
                                        "Automatic recovery stopped; job-worker MCP is ineligible for future Docker jobs until explicitly activated"
                                            .to_string()
                                    } else {
                                        "Automatic recovery stopped after repeated host MCP crashes; activate manually to retry"
                                            .to_string()
                                    }
                                }),
                            version: None,
                        });
                    }
                }
                Err(e) => {
                    tracing::debug!("Failed to load MCP servers for listing: {}", e);
                }
            }
        }

        // List WASM tools
        if (kind_filter.is_none() || kind_filter == Some(ExtensionKind::WasmTool))
            && self.wasm_tools_dir.exists()
        {
            match discover_tools(&self.wasm_tools_dir).await {
                Ok(tools) => {
                    for (name, discovered) in tools {
                        let active = self.tool_registry.has(&name).await;

                        let registry_entry = self
                            .registry
                            .get_with_kind(&name, Some(ExtensionKind::WasmTool))
                            .await;
                        let display_name = registry_entry.as_ref().map(|e| e.display_name.clone());
                        let auth_state = self.check_tool_auth_status(&name, user_id).await;
                        let version = if let Some(ref cap_path) = discovered.capabilities_path {
                            tokio::fs::read(cap_path)
                                .await
                                .ok()
                                .and_then(|bytes| {
                                    crate::tools::wasm::CapabilitiesFile::from_bytes(&bytes).ok()
                                })
                                .and_then(|cap| cap.version)
                        } else {
                            None
                        };
                        let version =
                            version.or_else(|| registry_entry.and_then(|e| e.version.clone()));
                        extensions.push(InstalledExtension {
                            name: name.clone(),
                            kind: ExtensionKind::WasmTool,
                            display_name,
                            description: None,
                            url: None,
                            transport: None,
                            command: None,
                            placement: None,
                            authenticated: auth_state == ToolAuthState::Ready,
                            active,
                            recovering: false,
                            enabled: None,
                            tools: if active { vec![name] } else { Vec::new() },
                            needs_setup: auth_state == ToolAuthState::NeedsSetup,
                            has_auth: auth_state != ToolAuthState::NoAuth,
                            installed: true,
                            activation_error: None,
                            version,
                        });
                    }
                }
                Err(e) => {
                    tracing::debug!("Failed to discover WASM tools for listing: {}", e);
                }
            }
        }

        // List WASM channels
        if (kind_filter.is_none() || kind_filter == Some(ExtensionKind::WasmChannel))
            && self.wasm_channels_dir.exists()
        {
            match crate::channels::wasm::discover_channels(&self.wasm_channels_dir).await {
                Ok(channels) => {
                    let active_names = self.active_channel_names.read().await;
                    let errors = self.activation_errors.read().await;
                    for (name, discovered) in channels {
                        let active = active_names.contains(&name);
                        let auth_state = self.check_channel_auth_status(&name, user_id).await;
                        let activation_error = errors.get(&name).cloned();
                        let registry_entry = self
                            .registry
                            .get_with_kind(&name, Some(ExtensionKind::WasmChannel))
                            .await;
                        let display_name = registry_entry.as_ref().map(|e| e.display_name.clone());
                        let version = if let Some(ref cap_path) = discovered.capabilities_path {
                            tokio::fs::read(cap_path)
                                .await
                                .ok()
                                .and_then(|bytes| {
                                    crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(
                                        &bytes,
                                    )
                                    .ok()
                                })
                                .and_then(|cap| cap.version)
                        } else {
                            None
                        };
                        let version =
                            version.or_else(|| registry_entry.and_then(|e| e.version.clone()));
                        extensions.push(InstalledExtension {
                            name,
                            kind: ExtensionKind::WasmChannel,
                            display_name,
                            description: None,
                            url: None,
                            transport: None,
                            command: None,
                            placement: None,
                            authenticated: auth_state == ToolAuthState::Ready,
                            active,
                            recovering: false,
                            enabled: None,
                            tools: Vec::new(),
                            needs_setup: auth_state == ToolAuthState::NeedsSetup,
                            has_auth: auth_state != ToolAuthState::NoAuth,
                            installed: true,
                            activation_error,
                            version,
                        });
                    }
                }
                Err(e) => {
                    tracing::debug!("Failed to discover WASM channels for listing: {}", e);
                }
            }
        }

        // Append available-but-not-installed registry entries
        if include_available {
            let installed_names: std::collections::HashSet<(String, ExtensionKind)> = extensions
                .iter()
                .map(|e| (e.name.clone(), e.kind))
                .collect();

            for entry in self.registry.all_entries().await {
                if let Some(filter) = kind_filter
                    && entry.kind != filter
                {
                    continue;
                }
                if installed_names.contains(&(entry.name.clone(), entry.kind)) {
                    continue;
                }
                extensions.push(InstalledExtension {
                    name: entry.name,
                    kind: entry.kind,
                    display_name: Some(entry.display_name),
                    description: Some(entry.description),
                    url: None,
                    transport: None,
                    command: None,
                    placement: None,
                    authenticated: false,
                    active: false,
                    recovering: false,
                    enabled: None,
                    tools: Vec::new(),
                    needs_setup: false,
                    has_auth: false,
                    installed: false,
                    activation_error: None,
                    version: entry.version,
                });
            }
        }

        Ok(extensions)
    }

    /// Remove an installed extension.
    pub async fn remove(&self, name: &str, user_id: &str) -> Result<String, ExtensionError> {
        Self::validate_extension_name(name)?;
        let kind = self.determine_installed_kind(name, user_id).await?;

        if kind != ExtensionKind::McpServer {
            self.clear_pending_extension_auth(name, user_id).await;
        }

        match kind {
            ExtensionKind::McpServer => {
                let _lifecycle_guard = self.lock_mcp_lifecycle_for_manual(name).await;
                let placement = self
                    .get_mcp_server(name, user_id)
                    .await
                    .map_err(|error| ExtensionError::Config(error.to_string()))?
                    .placement;
                self.remove_mcp_server(name, user_id)
                    .await
                    .map_err(|e| ExtensionError::Config(e.to_string()))?;
                self.clear_pending_extension_auth(name, user_id).await;
                let stopped = self.stop_mcp_runtime(name, user_id).await;

                let mut message = if placement == McpPlacement::JobWorker {
                    format!(
                        "Removed job-worker MCP server '{name}' from future Docker jobs; running jobs retain immutable MCP snapshots; cleaned up {} stale host tool(s)",
                        stopped.tool_names.len()
                    )
                } else {
                    format!(
                        "Removed host MCP server '{}' and {} tool(s)",
                        name,
                        stopped.tool_names.len()
                    )
                };
                if let Some(warning) = stopped.cleanup_warning {
                    message.push_str(&format!(". Runtime cleanup warning: {warning}"));
                }
                Ok(message)
            }
            ExtensionKind::WasmTool => {
                // Remove only exact WASM registration. Other runtimes may use
                // same name and must retain their ownership.
                if let Some(tool) = self.tool_registry.get(name).await
                    && tool.is_wasm_tool()
                {
                    self.tool_registry.unregister_if_same(name, &tool).await;
                }

                // Evict compiled module from runtime cache so reinstall uses fresh binary
                if let Some(ref rt) = self.wasm_tool_runtime {
                    rt.remove(name).await;
                }

                // Clear stale activation errors so reinstall starts clean
                self.activation_errors.write().await.remove(name);

                // Revoke credential mappings from the shared registry
                let cap_path = self
                    .wasm_tools_dir
                    .join(format!("{}.capabilities.json", name));
                self.revoke_credential_mappings(&cap_path).await;

                // Unregister hooks registered from this plugin source.
                let removed_hooks = self
                    .unregister_hook_prefix(&format!("plugin.tool:{}::", name))
                    .await
                    + self
                        .unregister_hook_prefix(&format!("plugin.dev_tool:{}::", name))
                        .await;
                if removed_hooks > 0 {
                    tracing::info!(
                        extension = name,
                        removed_hooks = removed_hooks,
                        "Removed plugin hooks for WASM tool"
                    );
                }

                // Delete files
                let wasm_path = self.wasm_tools_dir.join(format!("{}.wasm", name));

                if wasm_path.exists() {
                    tokio::fs::remove_file(&wasm_path)
                        .await
                        .map_err(|e| ExtensionError::Other(e.to_string()))?;
                }
                if cap_path.exists() {
                    let _ = tokio::fs::remove_file(&cap_path).await;
                }

                Ok(format!("Removed WASM tool '{}'", name))
            }
            ExtensionKind::WasmChannel => {
                // Remove from active set and persist
                self.active_channel_names.write().await.remove(name);
                self.persist_active_channels(user_id).await;

                // Clear stale activation errors so reinstall starts clean
                self.activation_errors.write().await.remove(name);

                // Delete channel files
                let wasm_path = self.wasm_channels_dir.join(format!("{}.wasm", name));
                let cap_path = self
                    .wasm_channels_dir
                    .join(format!("{}.capabilities.json", name));

                // Revoke credential mappings before deleting the capabilities file
                self.revoke_credential_mappings(&cap_path).await;

                if wasm_path.exists() {
                    tokio::fs::remove_file(&wasm_path)
                        .await
                        .map_err(|e| ExtensionError::Other(e.to_string()))?;
                }
                if cap_path.exists() {
                    let _ = tokio::fs::remove_file(&cap_path).await;
                }

                Ok(format!(
                    "Removed channel '{}'. Restart LunarWing for the change to take effect.",
                    name
                ))
            }
        }
    }

    /// Upgrade installed WASM extensions to match the current host WIT version.
    ///
    /// If `name` is `Some`, upgrades only that extension.  If `None`, checks all
    /// installed WASM tools and channels and upgrades any that are outdated.
    ///
    /// The upgrade preserves authentication secrets — only the `.wasm` binary
    /// (and `.capabilities.json`) are replaced.
    pub async fn upgrade(
        &self,
        name: Option<&str>,
        user_id: &str,
    ) -> Result<UpgradeResult, ExtensionError> {
        // Collect extensions to check
        let mut candidates: Vec<(String, ExtensionKind)> = Vec::new();

        if let Some(name) = name {
            Self::validate_extension_name(name)?;
            let kind = self.determine_installed_kind(name, user_id).await?;
            if kind == ExtensionKind::McpServer {
                return Err(ExtensionError::Other(
                    "MCP servers don't have WIT versions and cannot be upgraded this way"
                        .to_string(),
                ));
            }
            candidates.push((name.to_string(), kind));
        } else {
            // Discover all installed WASM tools
            if self.wasm_tools_dir.exists()
                && let Ok(tools) = discover_tools(&self.wasm_tools_dir).await
            {
                for (tool_name, _) in tools {
                    candidates.push((tool_name, ExtensionKind::WasmTool));
                }
            }
            // Discover all installed WASM channels
            if self.wasm_channels_dir.exists()
                && let Ok(channels) =
                    crate::channels::wasm::discover_channels(&self.wasm_channels_dir).await
            {
                for (ch_name, _) in channels {
                    candidates.push((ch_name, ExtensionKind::WasmChannel));
                }
            }
        }

        if candidates.is_empty() {
            return Ok(UpgradeResult {
                results: Vec::new(),
                message: "No WASM extensions installed.".to_string(),
            });
        }

        let mut outcomes = Vec::new();

        for (ext_name, kind) in &candidates {
            let outcome = self.upgrade_one(ext_name, *kind, user_id).await;
            outcomes.push(outcome);
        }

        let upgraded = outcomes.iter().filter(|o| o.status == "upgraded").count();
        let up_to_date = outcomes
            .iter()
            .filter(|o| o.status == "already_up_to_date")
            .count();
        let failed = outcomes.iter().filter(|o| o.status == "failed").count();

        let message = format!(
            "{} extension(s) checked: {} upgraded, {} already up to date, {} failed",
            outcomes.len(),
            upgraded,
            up_to_date,
            failed
        );

        Ok(UpgradeResult {
            results: outcomes,
            message,
        })
    }

    /// Upgrade a single WASM extension if its WIT version is outdated.
    async fn upgrade_one(&self, name: &str, kind: ExtensionKind, user_id: &str) -> UpgradeOutcome {
        let (cap_dir, host_wit) = match kind {
            ExtensionKind::WasmTool => (&self.wasm_tools_dir, crate::tools::wasm::WIT_TOOL_VERSION),
            ExtensionKind::WasmChannel => (
                &self.wasm_channels_dir,
                crate::tools::wasm::WIT_CHANNEL_VERSION,
            ),
            ExtensionKind::McpServer => {
                return UpgradeOutcome {
                    name: name.to_string(),
                    kind,
                    status: "failed".to_string(),
                    detail: "This extension type cannot be upgraded this way".to_string(),
                };
            }
        };

        // Read current WIT version from capabilities
        let cap_path = cap_dir.join(format!("{}.capabilities.json", name));
        let declared_wit = if cap_path.exists() {
            match tokio::fs::read(&cap_path).await {
                Ok(bytes) => {
                    let wit: Option<String> = match kind {
                        ExtensionKind::WasmTool => {
                            crate::tools::wasm::CapabilitiesFile::from_bytes(&bytes)
                                .ok()
                                .and_then(|c| c.wit_version)
                        }
                        ExtensionKind::WasmChannel => {
                            crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(&bytes)
                                .ok()
                                .and_then(|c| c.wit_version)
                        }
                        ExtensionKind::McpServer => None,
                    };
                    wit
                }
                Err(_) => None,
            }
        } else {
            None
        };

        // Check if upgrade is needed
        let needs_upgrade =
            crate::tools::wasm::check_wit_version_compat(name, declared_wit.as_deref(), host_wit)
                .is_err();

        if !needs_upgrade {
            return UpgradeOutcome {
                name: name.to_string(),
                kind,
                status: "already_up_to_date".to_string(),
                detail: format!(
                    "WIT {} matches host WIT {}",
                    declared_wit.as_deref().unwrap_or("unknown"),
                    host_wit
                ),
            };
        }

        // Check registry for a newer version
        let entry = self.registry.get_with_kind(name, Some(kind)).await;
        let Some(entry) = entry else {
            return UpgradeOutcome {
                name: name.to_string(),
                kind,
                status: "not_in_registry".to_string(),
                detail: format!(
                    "Extension '{}' has outdated WIT {} (host: {}), \
                     but is not in the registry. Reinstall manually with a URL.",
                    name,
                    declared_wit.as_deref().unwrap_or("unknown"),
                    host_wit
                ),
            };
        };

        // Delete old .wasm file (keep secrets intact)
        let wasm_path = cap_dir.join(format!("{}.wasm", name));
        if wasm_path.exists()
            && let Err(e) = tokio::fs::remove_file(&wasm_path).await
        {
            return UpgradeOutcome {
                name: name.to_string(),
                kind,
                status: "failed".to_string(),
                detail: format!("Failed to remove old WASM binary: {}", e),
            };
        }
        // Also remove old capabilities so install_from_entry can write the new one
        if cap_path.exists() {
            let _ = tokio::fs::remove_file(&cap_path).await;
        }

        // Reinstall from registry
        match self.install_from_entry(&entry, user_id).await {
            Ok(_) => {
                tracing::info!(
                    extension = %name,
                    old_wit = ?declared_wit,
                    new_host_wit = %host_wit,
                    "Upgraded WASM extension"
                );
                UpgradeOutcome {
                    name: name.to_string(),
                    kind,
                    status: "upgraded".to_string(),
                    detail: format!(
                        "Upgraded from WIT {} to host WIT {}. Restart to activate.",
                        declared_wit.as_deref().unwrap_or("unknown"),
                        host_wit
                    ),
                }
            }
            Err(e) => UpgradeOutcome {
                name: name.to_string(),
                kind,
                status: "failed".to_string(),
                detail: format!("Reinstall failed: {}. Old files were removed.", e),
            },
        }
    }

    /// Get detailed info about an installed extension (version, wit_version, host compatibility).
    pub async fn extension_info(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<serde_json::Value, ExtensionError> {
        Self::validate_extension_name(name)?;
        let kind = self.determine_installed_kind(name, user_id).await?;

        match kind {
            ExtensionKind::WasmTool => {
                let cap_path = self
                    .wasm_tools_dir
                    .join(format!("{}.capabilities.json", name));
                let wasm_path = self.wasm_tools_dir.join(format!("{}.wasm", name));

                let mut info = serde_json::json!({
                    "name": name,
                    "kind": "wasm_tool",
                    "installed": wasm_path.exists(),
                });

                if cap_path.exists()
                    && let Ok(bytes) = tokio::fs::read(&cap_path).await
                    && let Ok(cap) = crate::tools::wasm::CapabilitiesFile::from_bytes(&bytes)
                {
                    info["version"] =
                        serde_json::json!(cap.version.unwrap_or_else(|| "unknown".into()));
                    info["wit_version"] =
                        serde_json::json!(cap.wit_version.unwrap_or_else(|| "unknown".into()));
                }

                info["host_wit_version"] = serde_json::json!(crate::tools::wasm::WIT_TOOL_VERSION);

                Ok(info)
            }
            ExtensionKind::WasmChannel => {
                let cap_path = self
                    .wasm_channels_dir
                    .join(format!("{}.capabilities.json", name));
                let wasm_path = self.wasm_channels_dir.join(format!("{}.wasm", name));

                let mut info = serde_json::json!({
                    "name": name,
                    "kind": "wasm_channel",
                    "installed": wasm_path.exists(),
                    "active": self.active_channel_names.read().await.contains(name),
                });

                if cap_path.exists()
                    && let Ok(bytes) = tokio::fs::read(&cap_path).await
                    && let Ok(cap) =
                        crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(&bytes)
                {
                    info["version"] =
                        serde_json::json!(cap.version.unwrap_or_else(|| "unknown".into()));
                    info["wit_version"] =
                        serde_json::json!(cap.wit_version.unwrap_or_else(|| "unknown".into()));
                }

                info["host_wit_version"] =
                    serde_json::json!(crate::tools::wasm::WIT_CHANNEL_VERSION);

                Ok(info)
            }
            ExtensionKind::McpServer => {
                let server = self
                    .get_mcp_server(name, user_id)
                    .await
                    .map_err(|error| ExtensionError::Config(error.to_string()))?;
                let worker_placement = server.placement == McpPlacement::JobWorker;
                let health = self
                    .active_mcp_servers
                    .read()
                    .await
                    .get(name)
                    .filter(|runtime| runtime.owner_id == user_id)
                    .map(|runtime| runtime.health);
                let info = serde_json::json!({
                    "name": name,
                    "kind": "mcp_server",
                    "placement": server.placement,
                    "worker_eligible": server.should_run_in_job(),
                    "connected": !worker_placement && health == Some(McpRuntimeHealth::Healthy),
                    "recovering": !worker_placement && matches!(
                        health,
                        Some(McpRuntimeHealth::Recovering | McpRuntimeHealth::RetryPending)
                    ),
                    "recovery_failed": !worker_placement
                        && health == Some(McpRuntimeHealth::CircuitOpen),
                });
                Ok(info)
            }
        }
    }

    // ── MCP config helpers (DB with disk fallback) ─────────────────────

    async fn load_mcp_servers(
        &self,
        user_id: &str,
    ) -> Result<crate::tools::mcp::config::McpServersFile, crate::tools::mcp::config::ConfigError>
    {
        if let Some(ref store) = self.store {
            crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), user_id).await
        } else {
            crate::tools::mcp::config::load_mcp_servers().await
        }
    }

    async fn get_mcp_server(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<McpServerConfig, crate::tools::mcp::config::ConfigError> {
        let servers = self.load_mcp_servers(user_id).await?;
        servers.get(name).cloned().ok_or_else(|| {
            crate::tools::mcp::config::ConfigError::ServerNotFound {
                name: name.to_string(),
            }
        })
    }

    async fn add_mcp_server(
        &self,
        config: McpServerConfig,
        user_id: &str,
    ) -> Result<(), crate::tools::mcp::config::ConfigError> {
        config.validate()?;
        if let Some(ref store) = self.store {
            crate::tools::mcp::config::persist_mcp_server_db(store.as_ref(), user_id, config, false)
                .await
        } else {
            crate::tools::mcp::config::persist_mcp_server(config, false).await
        }
    }

    async fn remove_mcp_server(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<(), crate::tools::mcp::config::ConfigError> {
        if let Some(ref store) = self.store {
            crate::tools::mcp::config::remove_mcp_server_db(store.as_ref(), user_id, name).await
        } else {
            crate::tools::mcp::config::remove_mcp_server(name).await
        }
    }

    async fn set_mcp_enabled(
        &self,
        name: &str,
        user_id: &str,
        enabled: bool,
    ) -> Result<McpServerConfig, ExtensionError> {
        if let Some(ref store) = self.store {
            crate::tools::mcp::config::set_mcp_server_enabled_db(
                store.as_ref(),
                user_id,
                name,
                enabled,
            )
            .await
            .map_err(|e| ExtensionError::Config(e.to_string()))
        } else {
            crate::tools::mcp::config::set_mcp_server_enabled(name, enabled)
                .await
                .map_err(|e| ExtensionError::Config(e.to_string()))
        }
    }

    async fn disable_mcp_if_unchanged(
        &self,
        user_id: &str,
        expected: &McpServerConfig,
    ) -> Result<bool, ExtensionError> {
        if let Some(ref store) = self.store {
            crate::tools::mcp::config::disable_mcp_server_if_unchanged_db(
                store.as_ref(),
                user_id,
                expected,
            )
            .await
        } else {
            crate::tools::mcp::config::disable_mcp_server_if_unchanged(expected).await
        }
        .map_err(|error| ExtensionError::Config(error.to_string()))
    }

    async fn persist_mcp_recovery_circuit_open(
        &self,
        user_id: &str,
        name: &str,
    ) -> Result<(), ExtensionError> {
        if let Some(ref store) = self.store {
            crate::tools::mcp::config::mark_mcp_recovery_circuit_open_db(
                store.as_ref(),
                user_id,
                name,
            )
            .await
        } else {
            crate::tools::mcp::config::mark_mcp_recovery_circuit_open(name).await
        }
        .map_err(|error| ExtensionError::Config(error.to_string()))
    }

    async fn rollback_failed_mcp_activation(
        &self,
        user_id: &str,
        server: &McpServerConfig,
        error: ExtensionError,
    ) -> ExtensionError {
        match self.disable_mcp_if_unchanged(user_id, server).await {
            Ok(true) => error,
            Ok(false) => ExtensionError::Other(format!(
                "{error}; enabled state changed concurrently and was not rolled back"
            )),
            Err(rollback_error) => ExtensionError::Other(format!(
                "{error}; failed to roll back enabled state: {rollback_error}"
            )),
        }
    }

    /// Roll back the enabled flag after a failed activation, except when the
    /// failure is [`ExtensionError::AuthRequired`]. Authentication is a
    /// waypoint, not a failure: the caller continues into the OAuth flow, and
    /// the post-callback `activate_after_auth` path only completes activation
    /// and resumes paused engine gates while the server stays enabled.
    async fn rollback_mcp_activation_unless_auth_required(
        &self,
        user_id: &str,
        server: &McpServerConfig,
        error: ExtensionError,
    ) -> ExtensionError {
        if matches!(error, ExtensionError::AuthRequired) {
            return error;
        }
        self.rollback_failed_mcp_activation(user_id, server, error)
            .await
    }

    async fn recover_mcp_runtime_if_needed(
        &self,
        candidate: McpRecoveryCandidate,
        cancel: CancellationToken,
    ) {
        let _lifecycle_guard = tokio::select! {
            _ = cancel.cancelled() => return,
            guard = self.lock_mcp_lifecycle(&candidate.name) => guard,
        };
        self.refresh_mcp_tools_if_needed(&candidate, &cancel).await;
        if !self.apply_mcp_recovery_claim(&candidate).await {
            return;
        }

        let config = await_mcp_recovery_step(
            &cancel,
            self.get_mcp_server(&candidate.name, &candidate.owner_id),
        )
        .await;
        let server = match config {
            Err(McpRecoveryAbort::Cancelled) => return,
            Err(McpRecoveryAbort::TimedOut) => {
                self.schedule_mcp_recovery_retry(&candidate, "configuration read timed out")
                    .await;
                return;
            }
            Ok(Ok(server))
                if server.enabled
                    && server.placement == McpPlacement::Host
                    && matches!(
                        server.effective_transport(),
                        crate::tools::mcp::config::EffectiveTransport::Stdio { .. }
                            | crate::tools::mcp::config::EffectiveTransport::Wasm { .. }
                    ) =>
            {
                server
            }
            Ok(Ok(_)) | Ok(Err(crate::tools::mcp::config::ConfigError::ServerNotFound { .. })) => {
                self.stop_mcp_runtime(&candidate.name, &candidate.owner_id)
                    .await;
                return;
            }
            Ok(Err(_)) => {
                self.schedule_mcp_recovery_retry(&candidate, "configuration unavailable")
                    .await;
                return;
            }
        };

        self.rebuild_mcp_runtime(candidate, server, &cancel).await;
    }

    async fn refresh_mcp_tools_if_needed(
        &self,
        candidate: &McpRecoveryCandidate,
        cancel: &CancellationToken,
    ) {
        let (client, old_tools, observed_generation) = {
            let runtimes = self.active_mcp_servers.read().await;
            let Some(runtime) = runtimes.get(&candidate.name).filter(|runtime| {
                runtime.owner_id == candidate.owner_id
                    && runtime.generation == candidate.generation
                    && runtime.health == McpRuntimeHealth::Healthy
                    && runtime.client.tools_generation() != runtime.tools_generation
            }) else {
                return;
            };
            (
                Arc::clone(&runtime.client),
                runtime.tools.clone(),
                runtime.client.tools_generation(),
            )
        };

        let replacement = match await_mcp_recovery_step(cancel, client.create_tools()).await {
            Ok(Ok(tools)) => tools,
            Ok(Err(error)) => {
                tracing::warn!(server = %candidate.name, %error, "MCP tools/list refresh failed; retaining prior tools");
                return;
            }
            Err(McpRecoveryAbort::TimedOut) => {
                tracing::warn!(server = %candidate.name, "MCP tools/list refresh timed out; retaining prior tools");
                return;
            }
            Err(McpRecoveryAbort::Cancelled) => return,
        };
        if let Err(conflicts) = self
            .tool_registry
            .replace_runtime_tools(&old_tools, replacement.clone())
            .await
        {
            tracing::warn!(server = %candidate.name, tools = ?conflicts, "MCP tools/list refresh conflicted; retaining prior tools");
            return;
        }

        let mut runtimes = self.active_mcp_servers.write().await;
        let Some(runtime) = runtimes.get_mut(&candidate.name).filter(|runtime| {
            runtime.owner_id == candidate.owner_id && runtime.generation == candidate.generation
        }) else {
            drop(runtimes);
            let _ = self
                .tool_registry
                .replace_runtime_tools(&replacement, old_tools)
                .await;
            return;
        };
        runtime.tools = replacement;
        runtime.tools_generation = observed_generation;
        tracing::info!(server = %candidate.name, tools = runtime.tools.len(), "Refreshed MCP tools after list change notification");
    }

    async fn claim_mcp_recovery(&self, candidate: &McpRecoveryCandidate) -> McpRecoveryClaim {
        let (health, retry_after) = {
            let runtimes = self.active_mcp_servers.read().await;
            let Some(runtime) = runtimes.get(&candidate.name).filter(|runtime| {
                runtime.owner_id == candidate.owner_id
                    && runtime.generation == candidate.generation
                    && (runtime.managed_stdio || runtime.managed_wasm)
            }) else {
                return McpRecoveryClaim::Skip;
            };
            (runtime.health, runtime.retry_after)
        };

        match health {
            McpRuntimeHealth::Recovering | McpRuntimeHealth::CircuitOpen => {
                return McpRecoveryClaim::Skip;
            }
            McpRuntimeHealth::RetryPending => {
                if retry_after.is_some_and(|retry| retry > Instant::now()) {
                    return McpRecoveryClaim::Skip;
                }
            }
            McpRuntimeHealth::Healthy => {
                if !self.mcp_runtime_needs_recovery(candidate).await {
                    return McpRecoveryClaim::Skip;
                }
            }
        }

        let mut runtimes = self.active_mcp_servers.write().await;
        let Some(runtime) = runtimes.get_mut(&candidate.name).filter(|runtime| {
            runtime.owner_id == candidate.owner_id && runtime.generation == candidate.generation
        }) else {
            return McpRecoveryClaim::Skip;
        };
        let dead_tools = runtime.tools.clone();
        if health == McpRuntimeHealth::Healthy
            && runtime.healthy_since.elapsed() >= MCP_RESTART_STABILITY_WINDOW
        {
            runtime.restart_attempts = 0;
        }
        runtime.retry_after = None;
        if runtime.restart_attempts >= MCP_RESTART_MAX_ATTEMPTS {
            runtime.health = McpRuntimeHealth::CircuitOpen;
            return McpRecoveryClaim::CircuitOpen {
                dead_tools,
                attempts: runtime.restart_attempts,
            };
        }
        let attempts = runtime.restart_attempts + 1;
        if health == McpRuntimeHealth::Healthy && attempts > 1 {
            let delay = mcp_restart_backoff(attempts);
            runtime.health = McpRuntimeHealth::RetryPending;
            runtime.retry_after = Some(Instant::now() + delay);
            return McpRecoveryClaim::Retry {
                dead_tools,
                attempts,
                delay,
            };
        }
        runtime.restart_attempts = attempts;
        runtime.health = McpRuntimeHealth::Recovering;
        McpRecoveryClaim::Recover(dead_tools)
    }

    async fn mcp_runtime_needs_recovery(&self, candidate: &McpRecoveryCandidate) -> bool {
        let (managed_stdio, client) = {
            let runtimes = self.active_mcp_servers.read().await;
            let Some(runtime) = runtimes.get(&candidate.name).filter(|runtime| {
                runtime.owner_id == candidate.owner_id && runtime.generation == candidate.generation
            }) else {
                return false;
            };
            (runtime.managed_stdio, Arc::clone(&runtime.client))
        };
        if !managed_stdio {
            let failed = !client.runtime_is_healthy();
            if failed {
                tracing::warn!(server = %candidate.name, "Host-local WASM MCP runtime trapped");
            }
            return failed;
        }

        match self.mcp_process_manager.state(&candidate.name).await {
            Ok(crate::tools::mcp::McpProcessState::Running) => false,
            Ok(crate::tools::mcp::McpProcessState::StdioClosed) => {
                tracing::warn!(server = %candidate.name, "Host-local MCP server stdio closed unexpectedly");
                true
            }
            Ok(crate::tools::mcp::McpProcessState::Exited(status)) => {
                tracing::warn!(server = %candidate.name, %status, "Host-local MCP server exited unexpectedly");
                true
            }
            Ok(crate::tools::mcp::McpProcessState::Missing) | Err(_) => {
                tracing::warn!(server = %candidate.name, "Host-local MCP server runtime is missing");
                true
            }
        }
    }

    async fn apply_mcp_recovery_claim(&self, candidate: &McpRecoveryCandidate) -> bool {
        let claim = self.claim_mcp_recovery(candidate).await;
        let dead_tools = match &claim {
            McpRecoveryClaim::Skip => return false,
            McpRecoveryClaim::Recover(tools)
            | McpRecoveryClaim::Retry {
                dead_tools: tools, ..
            }
            | McpRecoveryClaim::CircuitOpen {
                dead_tools: tools, ..
            } => tools,
        };
        for tool in dead_tools {
            self.tool_registry
                .unregister_if_same(tool.name(), tool)
                .await;
        }

        match claim {
            McpRecoveryClaim::Recover(_) => {
                self.broadcast_extension_status(
                    &candidate.owner_id,
                    &candidate.name,
                    "recovering",
                    Some("Host-local MCP server exited; recovery started"),
                )
                .await;
                true
            }
            McpRecoveryClaim::Retry {
                attempts, delay, ..
            } => {
                tracing::warn!(server = %candidate.name, attempts, retry_millis = delay.as_millis(), "Unstable MCP runtime; recovery delayed");
                self.broadcast_extension_status(
                    &candidate.owner_id,
                    &candidate.name,
                    "recovering",
                    Some("Host-local MCP server exited again; retry delayed"),
                )
                .await;
                false
            }
            McpRecoveryClaim::CircuitOpen { attempts, .. } => {
                self.open_mcp_recovery_circuit(candidate, attempts).await;
                false
            }
            McpRecoveryClaim::Skip => false,
        }
    }

    async fn rebuild_mcp_runtime(
        &self,
        candidate: McpRecoveryCandidate,
        server: McpServerConfig,
        cancel: &CancellationToken,
    ) {
        let (old_client, old_tools) = {
            let runtimes = self.active_mcp_servers.read().await;
            let Some(runtime) = runtimes.get(&candidate.name).filter(|runtime| {
                runtime.owner_id == candidate.owner_id && runtime.generation == candidate.generation
            }) else {
                return;
            };
            (Arc::clone(&runtime.client), runtime.tools.clone())
        };

        let shutdown = await_mcp_recovery_step(
            cancel,
            self.shutdown_mcp_runtime(&candidate.name, Some(old_client.as_ref())),
        )
        .await;
        match shutdown {
            Err(McpRecoveryAbort::Cancelled) => return,
            Err(McpRecoveryAbort::TimedOut) => {
                self.schedule_mcp_recovery_retry(&candidate, "runtime cleanup timed out")
                    .await;
                return;
            }
            Ok(Err(error)) => {
                tracing::warn!(server = %candidate.name, %error, "MCP crash cleanup was incomplete");
            }
            Ok(Ok(())) => {}
        }

        let preparation = await_mcp_recovery_step(
            cancel,
            self.prepare_mcp_runtime(&candidate.name, &candidate.owner_id, server),
        )
        .await;
        let prepared = match preparation {
            Err(McpRecoveryAbort::Cancelled) => {
                let _ = self.shutdown_mcp_runtime(&candidate.name, None).await;
                return;
            }
            Err(McpRecoveryAbort::TimedOut) => {
                let _ = self.shutdown_mcp_runtime(&candidate.name, None).await;
                self.schedule_mcp_recovery_retry(&candidate, "runtime initialization timed out")
                    .await;
                return;
            }
            Ok(Ok(prepared)) => prepared,
            Ok(Err(_)) => {
                self.schedule_mcp_recovery_retry(&candidate, "runtime initialization failed")
                    .await;
                return;
            }
        };

        let replacement_tools = prepared.tools.clone();
        if let Err(conflicts) = self
            .tool_registry
            .replace_runtime_tools(&old_tools, replacement_tools)
            .await
        {
            tracing::warn!(server = %candidate.name, tools = ?conflicts, "MCP recovery tool replacement conflicted");
            self.cleanup_failed_mcp_activation(&candidate.name, prepared.client.as_ref())
                .await;
            self.schedule_mcp_recovery_retry(&candidate, "tool registration conflict")
                .await;
            return;
        }

        self.finish_mcp_recovery(candidate, prepared).await;
    }

    async fn finish_mcp_recovery(
        &self,
        candidate: McpRecoveryCandidate,
        prepared: PreparedMcpRuntime,
    ) {
        let generation = self.next_mcp_runtime_generation();
        let mut runtimes = self.active_mcp_servers.write().await;
        let Some(runtime) = runtimes.get_mut(&candidate.name).filter(|runtime| {
            runtime.owner_id == candidate.owner_id && runtime.generation == candidate.generation
        }) else {
            drop(runtimes);
            for tool in &prepared.tools {
                self.tool_registry
                    .unregister_if_same(tool.name(), tool)
                    .await;
            }
            self.cleanup_failed_mcp_activation(&candidate.name, prepared.client.as_ref())
                .await;
            return;
        };
        runtime.client = prepared.client;
        runtime.tools = prepared.tools;
        runtime.tools_generation = runtime.client.tools_generation();
        runtime.generation = generation;
        runtime.health = McpRuntimeHealth::Healthy;
        runtime.healthy_since = Instant::now();
        runtime.retry_after = None;
        let tool_count = runtime.tools.len();
        drop(runtimes);

        tracing::info!(server = %candidate.name, tools = tool_count, "Recovered host-local MCP server");
        self.broadcast_extension_status(
            &candidate.owner_id,
            &candidate.name,
            "active",
            Some("Host-local MCP server recovered after an unexpected exit"),
        )
        .await;
    }

    async fn schedule_mcp_recovery_retry(
        &self,
        candidate: &McpRecoveryCandidate,
        reason: &'static str,
    ) {
        let mut runtimes = self.active_mcp_servers.write().await;
        let Some(runtime) = runtimes.get_mut(&candidate.name).filter(|runtime| {
            runtime.owner_id == candidate.owner_id && runtime.generation == candidate.generation
        }) else {
            return;
        };
        let attempts = runtime.restart_attempts;
        if attempts >= MCP_RESTART_MAX_ATTEMPTS {
            runtime.health = McpRuntimeHealth::CircuitOpen;
            runtime.retry_after = None;
            drop(runtimes);
            self.open_mcp_recovery_circuit(candidate, attempts).await;
            return;
        }
        let delay = mcp_restart_backoff(attempts.saturating_add(1));
        runtime.health = McpRuntimeHealth::RetryPending;
        runtime.retry_after = Some(Instant::now() + delay);
        drop(runtimes);

        tracing::warn!(server = %candidate.name, attempts, retry_millis = delay.as_millis(), reason, "MCP recovery scheduled for retry");
        self.broadcast_extension_status(
            &candidate.owner_id,
            &candidate.name,
            "recovering",
            Some("Host-local MCP server recovery failed; retry scheduled"),
        )
        .await;
    }

    async fn broadcast_mcp_circuit_open(&self, candidate: &McpRecoveryCandidate, attempts: u32) {
        tracing::error!(server = %candidate.name, attempts, "MCP automatic recovery circuit opened");
        self.broadcast_extension_status(
            &candidate.owner_id,
            &candidate.name,
            "failed",
            Some("Host-local MCP server repeatedly crashed; automatic recovery stopped"),
        )
        .await;
    }

    async fn open_mcp_recovery_circuit(&self, candidate: &McpRecoveryCandidate, attempts: u32) {
        if let Err(error) = self
            .persist_mcp_recovery_circuit_open(&candidate.owner_id, &candidate.name)
            .await
        {
            tracing::warn!(server = %candidate.name, %error, "Failed to persist MCP recovery circuit state");
        }
        let client = self
            .active_mcp_servers
            .read()
            .await
            .get(&candidate.name)
            .filter(|runtime| {
                runtime.owner_id == candidate.owner_id && runtime.generation == candidate.generation
            })
            .map(|runtime| Arc::clone(&runtime.client));
        if let Some(client) = client
            && let Err(error) = self
                .shutdown_mcp_runtime(&candidate.name, Some(client.as_ref()))
                .await
        {
            tracing::warn!(server = %candidate.name, %error, "MCP circuit-open cleanup was incomplete");
        }
        self.broadcast_mcp_circuit_open(candidate, attempts).await;
    }

    async fn stop_mcp_runtime(&self, name: &str, user_id: &str) -> StoppedMcpRuntime {
        if self
            .active_mcp_servers
            .read()
            .await
            .get(name)
            .is_some_and(|runtime| runtime.owner_id != user_id)
        {
            return StoppedMcpRuntime::default();
        }
        let runtime = self.active_mcp_servers.write().await.remove(name);
        let tool_names = runtime
            .as_ref()
            .map(|runtime| {
                runtime
                    .tools
                    .iter()
                    .map(|tool| tool.name().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if let Some(runtime) = runtime.as_ref() {
            for tool in &runtime.tools {
                self.tool_registry
                    .unregister_if_same(tool.name(), tool)
                    .await;
            }
        }

        let cleanup_warning = self
            .shutdown_mcp_runtime(
            name,
            runtime.as_ref().map(|runtime| runtime.client.as_ref()),
        )
            .await
            .err()
            .map(|error| {
                tracing::warn!(server = %name, error = %error, "MCP runtime cleanup was incomplete");
                error.to_string()
            });
        StoppedMcpRuntime {
            tool_names,
            cleanup_warning,
        }
    }

    async fn shutdown_mcp_runtime(
        &self,
        name: &str,
        client: Option<&McpClient>,
    ) -> Result<(), ExtensionError> {
        crate::tools::mcp::factory::shutdown_client_runtime(
            name,
            client,
            &self.mcp_session_manager,
            &self.mcp_process_manager,
        )
        .await
        .map_err(|error| ExtensionError::Other(error.to_string()))
    }

    async fn cleanup_failed_mcp_activation(&self, name: &str, client: &McpClient) {
        if let Err(error) = self.shutdown_mcp_runtime(name, Some(client)).await {
            tracing::warn!(
                server = %name,
                error = %error,
                "Failed to clean up MCP runtime after activation failure"
            );
        }
    }

    fn classify_mcp_activation_error(message: String) -> ExtensionError {
        let lower = message.to_ascii_lowercase();
        if lower.contains("requires authentication")
            || message.contains("401")
            || (message.contains("400")
                && (lower.contains("authorization") || lower.contains("authenticate")))
        {
            ExtensionError::AuthRequired
        } else {
            ExtensionError::ActivationFailed(message)
        }
    }

    // ── Private helpers ──────────────────────────────────────────────────

    async fn install_from_entry(
        &self,
        entry: &RegistryEntry,
        user_id: &str,
    ) -> Result<InstallResult, ExtensionError> {
        let primary_result = self
            .try_install_from_source(entry, &entry.source, user_id)
            .await;
        match fallback_decision(&primary_result, &entry.fallback_source) {
            FallbackDecision::Return => primary_result,
            FallbackDecision::TryFallback => {
                // TryFallback guarantees primary is Err and fallback_source is Some.
                let (primary_err, fallback) = match (primary_result, entry.fallback_source.as_ref())
                {
                    (Err(e), Some(f)) => (e, f),
                    (other, _) => return other,
                };
                tracing::info!(
                    extension = %entry.name,
                    primary_error = %primary_err,
                    "Primary install failed, trying fallback source"
                );
                match self.try_install_from_source(entry, fallback, user_id).await {
                    Ok(result) => Ok(result),
                    Err(fallback_err) => {
                        tracing::error!(
                            extension = %entry.name,
                            fallback_error = %fallback_err,
                            "Fallback install also failed"
                        );
                        Err(combine_install_errors(primary_err, fallback_err))
                    }
                }
            }
        }
    }

    /// Attempt to install an extension using a specific source.
    async fn try_install_from_source(
        &self,
        entry: &RegistryEntry,
        source: &ExtensionSource,
        user_id: &str,
    ) -> Result<InstallResult, ExtensionError> {
        match entry.kind {
            ExtensionKind::McpServer => {
                let mut config = match source {
                    ExtensionSource::McpUrl { url } | ExtensionSource::Discovered { url } => {
                        McpServerConfig::new(&entry.name, url)
                    }
                    ExtensionSource::McpStdio { command, args, env } => {
                        McpServerConfig::new_stdio(&entry.name, command, args.clone(), env.clone())
                    }
                    _ => {
                        return Err(ExtensionError::InstallFailed(
                            "Registry entry for MCP server has no supported transport".to_string(),
                        ));
                    }
                };
                config.description = Some(entry.description.clone());
                self.install_mcp_config(config, user_id).await
            }
            ExtensionKind::WasmTool => match source {
                ExtensionSource::WasmDownload {
                    wasm_url,
                    capabilities_url,
                } => {
                    self.install_wasm_tool_from_url_with_caps(
                        &entry.name,
                        wasm_url,
                        capabilities_url.as_deref(),
                    )
                    .await
                }
                ExtensionSource::WasmBuildable {
                    build_dir,
                    crate_name,
                    ..
                } => {
                    self.install_wasm_from_buildable(
                        &entry.name,
                        build_dir.as_deref(),
                        crate_name.as_deref(),
                        &self.wasm_tools_dir,
                        ExtensionKind::WasmTool,
                    )
                    .await
                }
                _ => Err(ExtensionError::InstallFailed(
                    "WASM tool entry has no download URL or build info".to_string(),
                )),
            },
            ExtensionKind::WasmChannel => match source {
                ExtensionSource::WasmDownload {
                    wasm_url,
                    capabilities_url,
                } => {
                    self.install_wasm_channel_from_url(
                        &entry.name,
                        wasm_url,
                        capabilities_url.as_deref(),
                    )
                    .await
                }
                ExtensionSource::WasmBuildable {
                    build_dir,
                    crate_name,
                    ..
                } => {
                    self.install_wasm_from_buildable(
                        &entry.name,
                        build_dir.as_deref(),
                        crate_name.as_deref(),
                        &self.wasm_channels_dir,
                        ExtensionKind::WasmChannel,
                    )
                    .await
                }
                _ => Err(ExtensionError::InstallFailed(
                    "WASM channel entry has no download URL or build info".to_string(),
                )),
            },
        }
    }

    async fn install_mcp_from_url(
        &self,
        name: &str,
        url: &str,
        user_id: &str,
    ) -> Result<InstallResult, ExtensionError> {
        self.install_mcp_config(McpServerConfig::new(name, url), user_id)
            .await
    }

    /// Install a complete MCP server configuration without starting it.
    pub async fn install_mcp_config(
        &self,
        config: McpServerConfig,
        user_id: &str,
    ) -> Result<InstallResult, ExtensionError> {
        Self::validate_extension_name(&config.name)?;
        let _lifecycle_guard = self.lock_mcp_lifecycle(&config.name).await;
        if self.get_mcp_server(&config.name, user_id).await.is_ok() {
            return Err(ExtensionError::AlreadyInstalled(config.name));
        }

        config
            .validate()
            .map_err(|e| ExtensionError::Config(e.to_string()))?;
        let name = config.name.clone();
        let placement = config.placement;
        let transport = match config.effective_transport() {
            crate::tools::mcp::config::EffectiveTransport::Http => "HTTP",
            crate::tools::mcp::config::EffectiveTransport::Stdio { .. } => "stdio",
            crate::tools::mcp::config::EffectiveTransport::Unix { .. } => "Unix socket",
            crate::tools::mcp::config::EffectiveTransport::Wasm { .. } => "WASM actor",
            crate::tools::mcp::config::EffectiveTransport::Gateway { .. } => "gateway",
            crate::tools::mcp::config::EffectiveTransport::External { .. } => "external runtime",
        };

        self.add_mcp_server(config, user_id)
            .await
            .map_err(|e| ExtensionError::Config(e.to_string()))?;

        tracing::info!(server = %name, transport, placement = ?placement, "Installed MCP server");
        let message = if placement == McpPlacement::JobWorker {
            format!(
                "MCP server '{name}' installed with {transport} transport and job_worker placement. Command must exist in worker image; activation and enabled changes apply to future Docker jobs only."
            )
        } else {
            format!(
                "MCP server '{name}' installed with {transport} transport and host placement. Run activate to connect."
            )
        };
        Ok(InstallResult {
            name: name.clone(),
            kind: ExtensionKind::McpServer,
            message,
        })
    }

    /// Read an installed MCP server's configuration.
    pub async fn get_mcp_server_pub(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<McpServerConfig, ExtensionError> {
        self.get_mcp_server(name, user_id)
            .await
            .map_err(|e| ExtensionError::Config(e.to_string()))
    }

    /// Update an installed MCP server's configuration in-place.
    ///
    /// Preserves enabled state, description, approval policy, and package provenance.
    /// Host credentials and sandbox policy remain host-only. Active host runtimes
    /// restart only for host-to-host updates.
    pub async fn update_mcp(
        &self,
        name: &str,
        mut new_config: McpServerConfig,
        user_id: &str,
    ) -> Result<String, ExtensionError> {
        Self::validate_extension_name(name)?;
        let kind = self.determine_installed_kind(name, user_id).await?;
        if kind != ExtensionKind::McpServer {
            return Err(ExtensionError::Other(format!(
                "Only MCP servers can be updated in-place; '{name}' is a {kind}"
            )));
        }

        let _lifecycle_guard = self.lock_mcp_lifecycle_for_manual(name).await;

        let existing = self
            .get_mcp_server(name, user_id)
            .await
            .map_err(|e| ExtensionError::Config(e.to_string()))?;
        new_config.name = existing.name.clone();
        new_config.enabled = existing.enabled;
        if new_config.oauth.is_none() && new_config.placement == McpPlacement::Host {
            new_config.oauth = existing.oauth.clone();
        }
        if new_config.headers.is_empty() && new_config.placement == McpPlacement::Host {
            new_config.headers = existing.headers.clone();
        }
        if new_config.description.is_none() {
            new_config.description = existing.description.clone();
        }
        new_config.always_require_approval = existing.always_require_approval.clone();
        if new_config.provenance.is_none() {
            new_config.provenance = existing.provenance.clone();
        }
        if new_config.sandbox.is_none() && new_config.placement == McpPlacement::Host {
            new_config.sandbox = existing.sandbox.clone();
        }

        new_config
            .validate()
            .map_err(|e| ExtensionError::Config(e.to_string()))?;

        let was_active = self
            .active_mcp_servers
            .read()
            .await
            .get(name)
            .is_some_and(|rt| rt.owner_id == user_id);

        self.persist_mcp_config(new_config.clone(), user_id).await?;

        if new_config.placement == McpPlacement::JobWorker {
            self.clear_pending_extension_auth(name, user_id).await;
        }

        if was_active {
            self.stop_mcp_runtime(name, user_id).await;
        }

        let reactivate_host = was_active
            && existing.placement == McpPlacement::Host
            && new_config.placement == McpPlacement::Host
            && new_config.enabled;
        let (message, status) = if reactivate_host {
            match self
                .activate_mcp_locked(name, user_id, new_config.clone())
                .await
            {
                Ok(result) => (
                    format!(
                        "Updated and reactivated MCP server '{name}'; {}",
                        result.message
                    ),
                    "updated",
                ),
                Err(e) => {
                    let rollback = self.disable_mcp_if_unchanged(user_id, &new_config).await;
                    let rollback_status = match rollback {
                        Ok(true) => "server disabled until explicit activation",
                        Ok(false) => "persisted state changed concurrently and was not disabled",
                        Err(ref error) => {
                            tracing::warn!(server = %name, %error, "Failed to disable MCP server after update reactivation failure");
                            "failed to persist disabled state"
                        }
                    };
                    (
                        format!(
                            "Updated MCP server '{name}' config but reactivation failed: {e}; {rollback_status}"
                        ),
                        "failed",
                    )
                }
            }
        } else if new_config.placement == McpPlacement::JobWorker {
            (
                format!(
                    "Updated MCP server '{name}' for job_worker placement; future Docker jobs use new config, running jobs retain immutable snapshots"
                ),
                "updated",
            )
        } else if existing.placement == McpPlacement::JobWorker {
            (
                format!(
                    "Updated MCP server '{name}' to host placement; explicit activation required"
                ),
                "updated",
            )
        } else {
            (
                format!("Updated MCP server '{name}' configuration"),
                "updated",
            )
        };

        tracing::info!(server = %name, "MCP server configuration updated in-place");
        self.broadcast_extension_status(user_id, name, status, Some(&message))
            .await;
        Ok(message)
    }

    async fn persist_mcp_config(
        &self,
        config: McpServerConfig,
        user_id: &str,
    ) -> Result<(), ExtensionError> {
        if let Some(ref store) = self.store {
            crate::tools::mcp::config::persist_mcp_server_db(store.as_ref(), user_id, config, true)
                .await
        } else {
            crate::tools::mcp::config::persist_mcp_server(config, true).await
        }
        .map_err(|e| ExtensionError::Config(e.to_string()))
    }

    async fn install_wasm_tool_from_url(
        &self,
        name: &str,
        url: &str,
    ) -> Result<InstallResult, ExtensionError> {
        self.install_wasm_tool_from_url_with_caps(name, url, None)
            .await
    }

    async fn install_wasm_tool_from_url_with_caps(
        &self,
        name: &str,
        url: &str,
        capabilities_url: Option<&str>,
    ) -> Result<InstallResult, ExtensionError> {
        self.download_and_install_wasm(name, url, capabilities_url, &self.wasm_tools_dir)
            .await?;

        Ok(InstallResult {
            name: name.to_string(),
            kind: ExtensionKind::WasmTool,
            message: format!("WASM tool '{}' installed. Run activate to load it.", name),
        })
    }

    async fn install_wasm_channel_from_url(
        &self,
        name: &str,
        url: &str,
        capabilities_url: Option<&str>,
    ) -> Result<InstallResult, ExtensionError> {
        self.download_and_install_wasm(name, url, capabilities_url, &self.wasm_channels_dir)
            .await?;

        Ok(InstallResult {
            name: name.to_string(),
            kind: ExtensionKind::WasmChannel,
            message: format!(
                "WASM channel '{}' installed. Run activate to start it.",
                name,
            ),
        })
    }

    /// Download a WASM extension (tool or channel) from URL and install to target directory.
    ///
    /// Handles both tar.gz bundles (containing `.wasm` + `.capabilities.json`) and bare
    /// `.wasm` files. Validates HTTPS, size limits, and file format.
    async fn download_and_install_wasm(
        &self,
        name: &str,
        url: &str,
        capabilities_url: Option<&str>,
        target_dir: &std::path::Path,
    ) -> Result<(), ExtensionError> {
        // Require HTTPS to prevent downgrade attacks
        if !url.starts_with("https://") {
            return Err(ExtensionError::InstallFailed(
                "Only HTTPS URLs are allowed for extension downloads".to_string(),
            ));
        }

        // 50 MB cap to prevent disk-fill DoS
        const MAX_DOWNLOAD_SIZE: usize = 50 * 1024 * 1024;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| ExtensionError::DownloadFailed(e.to_string()))?;

        let sanitized_url = sanitize_url_for_logging(url);
        tracing::debug!(extension = %name, url = %sanitized_url, "Downloading WASM extension");

        let response = client.get(url).send().await.map_err(|e| {
            tracing::error!(extension = %name, url = %sanitized_url, error = %e, "Download request failed");
            ExtensionError::DownloadFailed(e.to_string())
        })?;

        if !response.status().is_success() {
            let status = response.status();
            tracing::error!(
                extension = %name,
                url = %sanitized_url,
                status = %status,
                "Download returned non-success HTTP status"
            );
            return Err(ExtensionError::DownloadFailed(format!(
                "HTTP {} from {}",
                status, url
            )));
        }

        // Check Content-Length header before downloading the full body
        if let Some(len) = response.content_length()
            && len as usize > MAX_DOWNLOAD_SIZE
        {
            return Err(ExtensionError::InstallFailed(format!(
                "Download too large ({} bytes, max {} bytes)",
                len, MAX_DOWNLOAD_SIZE
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| ExtensionError::DownloadFailed(e.to_string()))?;

        if bytes.len() > MAX_DOWNLOAD_SIZE {
            return Err(ExtensionError::InstallFailed(format!(
                "Download too large ({} bytes, max {} bytes)",
                bytes.len(),
                MAX_DOWNLOAD_SIZE
            )));
        }

        // Ensure target directory exists
        tokio::fs::create_dir_all(target_dir)
            .await
            .map_err(|e| ExtensionError::InstallFailed(e.to_string()))?;

        let wasm_path = target_dir.join(format!("{}.wasm", name));
        let caps_path = target_dir.join(format!("{}.capabilities.json", name));

        // Detect format: gzip (tar.gz bundle) or bare WASM
        if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
            // tar.gz bundle: extract {name}.wasm and {name}.capabilities.json
            self.extract_wasm_tar_gz(name, &bytes, &wasm_path, &caps_path)?;
        } else {
            // Bare WASM file: validate magic number
            if bytes.len() < 4 || &bytes[..4] != b"\0asm" {
                return Err(ExtensionError::InstallFailed(
                    "Downloaded file is not a valid WASM binary (bad magic number)".to_string(),
                ));
            }

            tokio::fs::write(&wasm_path, &bytes)
                .await
                .map_err(|e| ExtensionError::InstallFailed(e.to_string()))?;

            // Download capabilities separately if URL provided
            if let Some(caps_url) = capabilities_url {
                const MAX_CAPS_SIZE: usize = 1024 * 1024; // 1 MB
                match client.get(caps_url).send().await {
                    Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                        Ok(caps_bytes) if caps_bytes.len() <= MAX_CAPS_SIZE => {
                            if let Err(e) = tokio::fs::write(&caps_path, &caps_bytes).await {
                                tracing::warn!(
                                    "Failed to write capabilities for '{}': {}",
                                    name,
                                    e
                                );
                            }
                        }
                        Ok(caps_bytes) => {
                            tracing::warn!(
                                "Capabilities file for '{}' too large ({} bytes, max {})",
                                name,
                                caps_bytes.len(),
                                MAX_CAPS_SIZE
                            );
                        }
                        Err(e) => {
                            tracing::warn!("Failed to download capabilities for '{}': {}", name, e);
                        }
                    },
                    _ => {
                        tracing::warn!(
                            "Failed to download capabilities for '{}' from {}",
                            name,
                            caps_url
                        );
                    }
                }
            }
        }

        tracing::info!(
            "Installed WASM extension '{}' from {} to {}",
            name,
            url,
            wasm_path.display()
        );

        Ok(())
    }

    /// Extract a tar.gz bundle into the WASM tools directory.
    fn extract_wasm_tar_gz(
        &self,
        name: &str,
        bytes: &[u8],
        target_wasm: &std::path::Path,
        target_caps: &std::path::Path,
    ) -> Result<(), ExtensionError> {
        use flate2::read::GzDecoder;
        use tar::Archive;

        use std::io::Read as _;

        let decoder = GzDecoder::new(bytes);
        let mut archive = Archive::new(decoder);
        // Defense-in-depth: do not preserve permissions or extended attributes
        archive.set_preserve_permissions(false);
        #[cfg(any(unix, target_os = "redox"))]
        archive.set_unpack_xattrs(false);

        // 100 MB cap on decompressed entry size to prevent decompression bombs
        const MAX_ENTRY_SIZE: u64 = 100 * 1024 * 1024;

        let wasm_filename = format!("{}.wasm", name);
        let caps_filename = format!("{}.capabilities.json", name);
        let mut found_wasm = false;

        let entries = archive
            .entries()
            .map_err(|e| ExtensionError::InstallFailed(format!("Bad tar.gz archive: {}", e)))?;

        for entry in entries {
            let mut entry = entry
                .map_err(|e| ExtensionError::InstallFailed(format!("Bad tar.gz entry: {}", e)))?;

            if entry.size() > MAX_ENTRY_SIZE {
                return Err(ExtensionError::InstallFailed(format!(
                    "Archive entry too large ({} bytes, max {} bytes)",
                    entry.size(),
                    MAX_ENTRY_SIZE
                )));
            }

            let entry_path = entry
                .path()
                .map_err(|e| {
                    ExtensionError::InstallFailed(format!("Invalid path in tar.gz: {}", e))
                })?
                .to_path_buf();

            let filename = entry_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");

            if filename == wasm_filename {
                let mut data = Vec::with_capacity(entry.size() as usize);
                std::io::Read::read_to_end(&mut entry.by_ref().take(MAX_ENTRY_SIZE), &mut data)
                    .map_err(|e| ExtensionError::InstallFailed(e.to_string()))?;
                std::fs::write(target_wasm, &data)
                    .map_err(|e| ExtensionError::InstallFailed(e.to_string()))?;
                found_wasm = true;
            } else if filename == caps_filename {
                let mut data = Vec::with_capacity(entry.size() as usize);
                std::io::Read::read_to_end(&mut entry.by_ref().take(MAX_ENTRY_SIZE), &mut data)
                    .map_err(|e| ExtensionError::InstallFailed(e.to_string()))?;
                std::fs::write(target_caps, &data)
                    .map_err(|e| ExtensionError::InstallFailed(e.to_string()))?;
            }
        }

        if !found_wasm {
            return Err(ExtensionError::InstallFailed(format!(
                "tar.gz archive does not contain '{}'",
                wasm_filename
            )));
        }

        Ok(())
    }

    /// Install a WASM extension from local build artifacts (WasmBuildable source).
    ///
    /// Resolves the build directory (relative to `CARGO_MANIFEST_DIR` or absolute),
    /// looks for the compiled WASM artifact, and copies it (plus capabilities.json)
    /// to the install directory. Falls back to an error if artifacts don't exist.
    async fn install_wasm_from_buildable(
        &self,
        name: &str,
        build_dir: Option<&str>,
        crate_name: Option<&str>,
        target_dir: &std::path::Path,
        kind: ExtensionKind,
    ) -> Result<InstallResult, ExtensionError> {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        // Resolve build directory
        let resolved_dir = match build_dir {
            Some(dir) => {
                let p = std::path::Path::new(dir);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    manifest_dir.join(dir)
                }
            }
            None => manifest_dir.to_path_buf(),
        };

        // Determine the binary name to look for
        let binary_name = crate_name.unwrap_or(name);

        let wasm_src =
            crate::registry::artifacts::find_wasm_artifact(&resolved_dir, binary_name, "release")
                .ok_or_else(|| {
                ExtensionError::InstallFailed(format!(
                    "'{}' requires building from source. Build artifact not found. \
                         Run `cargo component build --release` in {} first, \
                         or use `lunarwing registry install {}`.",
                    name,
                    resolved_dir.display(),
                    name,
                ))
            })?;

        let wasm_dst = crate::registry::artifacts::install_wasm_files(
            &wasm_src,
            &resolved_dir,
            name,
            target_dir,
            true,
        )
        .await
        .map_err(|e| ExtensionError::InstallFailed(e.to_string()))?;

        let kind_label = match kind {
            ExtensionKind::WasmTool => "WASM tool",
            ExtensionKind::WasmChannel => "WASM channel",
            ExtensionKind::McpServer => "MCP server",
        };

        tracing::info!(
            "Installed {} '{}' from build artifacts at {}",
            kind_label,
            name,
            wasm_dst.display(),
        );

        Ok(InstallResult {
            name: name.to_string(),
            kind,
            message: format!(
                "{} '{}' installed from local build artifacts. Run activate to load it.",
                kind_label, name,
            ),
        })
    }

    async fn auth_mcp(&self, name: &str, user_id: &str) -> Result<AuthResult, ExtensionError> {
        let server = self
            .get_mcp_server(name, user_id)
            .await
            .map_err(|e| ExtensionError::NotInstalled(e.to_string()))?;

        if server.placement == McpPlacement::JobWorker {
            return Err(ExtensionError::AuthNotSupported(format!(
                "Job-worker MCP server '{name}' cannot use host OAuth; configure worker-scoped env_secrets instead"
            )));
        }

        if !matches!(
            server.effective_transport(),
            crate::tools::mcp::config::EffectiveTransport::Http
                | crate::tools::mcp::config::EffectiveTransport::Gateway { .. }
        ) {
            return Ok(AuthResult::no_auth_required(name, ExtensionKind::McpServer));
        }

        // Check if already authenticated
        if is_authenticated(&server, &self.secrets, user_id).await {
            return Ok(AuthResult::authenticated(name, ExtensionKind::McpServer));
        }

        // In gateway mode, build an auth URL and return it for the frontend to
        // open in the same browser. The gateway's /oauth/callback handler will
        // complete the token exchange.
        if self.should_use_gateway_mode() {
            return match self.auth_mcp_build_url(name, &server, user_id).await {
                Ok(result) => Ok(result),
                Err(ExtensionError::AuthNotSupported(_)) => Ok(AuthResult::awaiting_token(
                    name,
                    ExtensionKind::McpServer,
                    format!(
                        "Server '{}' does not support OAuth. \
                         Please provide an API token/key for this server.",
                        name
                    ),
                    None,
                )),
                Err(e) => Err(e),
            };
        }

        // CLI/local mode: run the full blocking OAuth flow (opens browser, waits for callback)
        match authorize_mcp_server(&server, &self.secrets, user_id).await {
            Ok(_token) => {
                tracing::info!("MCP server '{}' authenticated via OAuth", name);
                Ok(AuthResult::authenticated(name, ExtensionKind::McpServer))
            }
            Err(crate::tools::mcp::auth::AuthError::NotSupported) => {
                // Server doesn't support OAuth, try building a URL
                match self.auth_mcp_build_url(name, &server, user_id).await {
                    Ok(result) => Ok(result),
                    Err(_) => Ok(AuthResult::awaiting_token(
                        name,
                        ExtensionKind::McpServer,
                        format!(
                            "Server '{}' does not support OAuth. \
                             Please provide an API token/key for this server.",
                            name
                        ),
                        None,
                    )),
                }
            }
            Err(e) => {
                // OAuth failed for some other reason, fall back to manual token
                Ok(AuthResult::awaiting_token(
                    name,
                    ExtensionKind::McpServer,
                    format!(
                        "OAuth failed for '{}': {}. \
                         Please provide an API token/key manually.",
                        name, e
                    ),
                    None,
                ))
            }
        }
    }

    /// Build an auth URL for MCP OAuth.
    ///
    /// In gateway mode, stores a `PendingOAuthFlow` so the web gateway's
    /// `/oauth/callback` handler can complete the token exchange — the auth
    /// URL is sent to the frontend which opens it in the same browser.
    /// In local/CLI mode, builds the URL for the user to open manually.
    async fn auth_mcp_build_url(
        &self,
        name: &str,
        server: &McpServerConfig,
        user_id: &str,
    ) -> Result<AuthResult, ExtensionError> {
        if server.placement != McpPlacement::Host {
            return Err(ExtensionError::AuthNotSupported(format!(
                "Job-worker MCP server '{name}' cannot use host OAuth"
            )));
        }
        // Try to discover OAuth metadata and build a URL the user can open manually
        let metadata = discover_full_oauth_metadata(&server.url)
            .await
            .map_err(|e| match e {
                crate::tools::mcp::auth::AuthError::NotSupported => {
                    ExtensionError::AuthNotSupported(e.to_string())
                }
                _ => ExtensionError::AuthFailed(e.to_string()),
            })?;

        use crate::cli::oauth_defaults;

        let is_gateway = self.should_use_gateway_mode();
        self.clear_pending_extension_auth(name, user_id).await;

        // Build redirect URI: gateway uses the public callback URL,
        // local mode binds a random port.
        let redirect_uri = if let Some(uri) = self.gateway_callback_redirect_uri().await {
            uri
        } else {
            let port = find_available_port()
                .await
                .map_err(|e| ExtensionError::AuthFailed(e.to_string()))?;
            format!("http://localhost:{}/callback", port.1)
        };

        // Try DCR if no client_id configured
        let (client_id, client_secret) = if let Some(ref oauth) = server.oauth {
            (oauth.client_id.clone(), None)
        } else if let Some(ref reg_endpoint) = metadata.registration_endpoint {
            let registration = register_client(reg_endpoint, &redirect_uri)
                .await
                .map_err(|e| ExtensionError::AuthFailed(e.to_string()))?;

            (registration.client_id, None)
        } else {
            return Err(ExtensionError::AuthNotSupported(
                "Server doesn't support OAuth or Dynamic Client Registration".to_string(),
            ));
        };

        // RFC 8707: resource parameter to scope the token to this MCP server
        let resource = canonical_resource_uri(&server.url);

        // Build authorization URL with CSRF state using the shared oauth_defaults
        // builder, which generates PKCE + state for us.
        let mut extra_params = server
            .oauth
            .as_ref()
            .map(|o| o.extra_params.clone())
            .unwrap_or_default();
        extra_params.insert("resource".to_string(), resource.clone());

        let scopes = server
            .oauth
            .as_ref()
            .map(|o| o.scopes.clone())
            .unwrap_or_else(|| metadata.scopes_supported.clone());

        let oauth_result = oauth_defaults::build_oauth_url(
            &metadata.authorization_endpoint,
            &client_id,
            &redirect_uri,
            &scopes,
            true, // Always use PKCE for MCP
            &extra_params,
        );
        let expected_state = oauth_result.state;
        let code_verifier = oauth_result.code_verifier;

        if is_gateway {
            let mut token_exchange_extra_params = HashMap::new();
            token_exchange_extra_params.insert("resource".to_string(), resource.clone());

            let flow = oauth_defaults::PendingOAuthFlow {
                extension_name: name.to_string(),
                display_name: server.name.clone(),
                token_url: metadata.token_endpoint,
                client_id,
                client_secret,
                redirect_uri,
                code_verifier,
                access_token_field: "access_token".to_string(),
                secret_name: server.token_secret_name(),
                provider: Some(format!("mcp:{}", name)),
                validation_endpoint: None,
                scopes,
                user_id: user_id.to_string(),
                secrets: Arc::clone(&self.secrets),
                sse_manager: self.sse_manager.read().await.clone(),
                gateway_token: self.gateway_token.clone(),
                token_exchange_extra_params,
                client_id_secret_name: if server.oauth.is_none() {
                    Some(server.client_id_secret_name())
                } else {
                    None
                },
                client_secret_secret_name: None,
                client_secret_expires_at: None,
                auto_activate_extension: false,
                created_at: std::time::Instant::now(),
            };

            Ok(self
                .start_gateway_oauth_flow(HostedOAuthFlowStart {
                    name: name.to_string(),
                    kind: ExtensionKind::McpServer,
                    auth_url: oauth_result.url,
                    expected_state,
                    flow,
                })
                .await)
        } else {
            // Local mode: return URL for manual opening
            self.pending_auth.write().await.insert(
                (name.to_string(), user_id.to_string()),
                PendingAuth {
                    _name: name.to_string(),
                    _kind: ExtensionKind::McpServer,
                    created_at: std::time::Instant::now(),
                    task_handle: None,
                },
            );

            Ok(AuthResult::awaiting_authorization(
                name,
                ExtensionKind::McpServer,
                oauth_result.url,
                "local".to_string(),
            ))
        }
    }

    async fn auth_wasm_tool(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<AuthResult, ExtensionError> {
        // Read the capabilities file to get auth config
        let cap_path = self
            .wasm_tools_dir
            .join(format!("{}.capabilities.json", name));

        if !cap_path.exists() {
            return Ok(AuthResult::no_auth_required(name, ExtensionKind::WasmTool));
        }

        let cap_bytes = tokio::fs::read(&cap_path)
            .await
            .map_err(|e| ExtensionError::Other(e.to_string()))?;

        let cap_file = crate::tools::wasm::CapabilitiesFile::from_bytes(&cap_bytes)
            .map_err(|e| ExtensionError::Other(e.to_string()))?;

        let auth = match cap_file.auth {
            Some(auth) => auth,
            None => {
                return Ok(AuthResult::no_auth_required(name, ExtensionKind::WasmTool));
            }
        };

        // Check env var first
        if let Some(ref env_var) = auth.env_var
            && let Ok(value) = std::env::var(env_var)
        {
            // Store the env var value as a secret
            let params =
                CreateSecretParams::new(&auth.secret_name, &value).with_provider(name.to_string());
            self.secrets
                .create(user_id, params)
                .await
                .map_err(|e| ExtensionError::AuthFailed(e.to_string()))?;

            return Ok(AuthResult::authenticated(name, ExtensionKind::WasmTool));
        }

        // Check if already authenticated (with scope expansion detection)
        let token_exists = self
            .secrets
            .exists(user_id, &auth.secret_name)
            .await
            .unwrap_or(false);

        if token_exists {
            // If this tool has OAuth config, check whether new scopes are needed
            let needs_reauth = if let Some(ref oauth) = auth.oauth {
                let merged = self
                    .collect_shared_scopes(&auth.secret_name, &oauth.scopes, user_id)
                    .await;
                let needs = self
                    .needs_scope_expansion(&auth.secret_name, &merged, user_id)
                    .await;
                tracing::debug!(
                    tool = name,
                    secret_name = %auth.secret_name,
                    merged_scopes = ?merged,
                    needs_reauth = needs,
                    "Scope expansion check"
                );
                needs
            } else {
                false
            };

            if !needs_reauth {
                return Ok(AuthResult::authenticated(name, ExtensionKind::WasmTool));
            }
            // Fall through to OAuth branch for scope expansion
        }

        // OAuth flow: if the tool has OAuth config, start the browser-based flow.
        // But only if credentials are available — if the tool has setup secrets
        // for client_id/secret that aren't configured yet, return needs_setup.
        if let Some(ref oauth) = auth.oauth {
            if self
                .needs_setup_credentials(name, &auth, oauth, user_id)
                .await
            {
                let display = auth.display_name.as_deref().unwrap_or(name);
                return Ok(AuthResult::needs_setup(
                    name,
                    ExtensionKind::WasmTool,
                    format!(
                        "Configure OAuth credentials for {} in the Setup tab.",
                        display
                    ),
                    auth.setup_url.clone(),
                ));
            }

            return self
                .start_wasm_oauth(name, &auth, oauth, user_id)
                .await
                .map_err(|e| ExtensionError::AuthFailed(e.to_string()));
        }

        // Return instructions for manual token entry
        let display = auth.display_name.unwrap_or_else(|| name.to_string());
        let instructions = auth
            .instructions
            .unwrap_or_else(|| format!("Please provide your {} API token/key.", display));

        Ok(AuthResult::awaiting_token(
            name,
            ExtensionKind::WasmTool,
            instructions,
            auth.setup_url,
        ))
    }

    /// Determine the auth readiness of a WASM channel.
    async fn check_channel_auth_status(&self, name: &str, user_id: &str) -> ToolAuthState {
        let cap_path = self
            .wasm_channels_dir
            .join(format!("{}.capabilities.json", name));
        let Ok(cap_bytes) = tokio::fs::read(&cap_path).await else {
            return ToolAuthState::NoAuth;
        };
        let Ok(cap_file) = crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(&cap_bytes)
        else {
            return ToolAuthState::NoAuth;
        };

        let required: Vec<_> = cap_file
            .setup
            .required_secrets
            .iter()
            .filter(|s| !s.optional)
            .collect();
        if required.is_empty() {
            return ToolAuthState::NoAuth;
        }

        let all_provided = futures::future::join_all(
            required
                .iter()
                .map(|s| self.secrets.exists(user_id, &s.name)),
        )
        .await
        .into_iter()
        .all(|r| r.unwrap_or(false));

        if all_provided {
            ToolAuthState::Ready
        } else {
            ToolAuthState::NeedsSetup
        }
    }

    /// Load and parse a WASM tool's capabilities file.
    ///
    /// Returns `None` if the file doesn't exist or can't be parsed.
    async fn load_tool_capabilities(
        &self,
        name: &str,
    ) -> Option<crate::tools::wasm::CapabilitiesFile> {
        let cap_path = self
            .wasm_tools_dir
            .join(format!("{}.capabilities.json", name));
        let cap_bytes = tokio::fs::read(&cap_path).await.ok()?;
        crate::tools::wasm::CapabilitiesFile::from_bytes(&cap_bytes).ok()
    }

    /// Collect merged OAuth scopes from all installed tools sharing the same secret_name.
    ///
    /// When multiple tools share an OAuth provider (e.g., google-calendar and google-drive
    /// both use `google_oauth_token`), we request all their scopes in a single OAuth flow
    /// so one login covers everything.
    async fn collect_shared_scopes(
        &self,
        secret_name: &str,
        base_scopes: &[String],
        _user_id: &str,
    ) -> Vec<String> {
        let mut all_scopes: std::collections::BTreeSet<String> =
            base_scopes.iter().cloned().collect();

        if let Ok(tools) = discover_tools(&self.wasm_tools_dir).await {
            for tool_name in tools.keys() {
                if let Some(cap) = self.load_tool_capabilities(tool_name).await
                    && let Some(auth) = &cap.auth
                    && auth.secret_name == secret_name
                    && let Some(oauth) = &auth.oauth
                {
                    all_scopes.extend(oauth.scopes.iter().cloned());
                }
            }
        }

        all_scopes.into_iter().collect()
    }

    /// Check whether the stored scopes are insufficient for the merged scopes.
    async fn needs_scope_expansion(
        &self,
        secret_name: &str,
        merged_scopes: &[String],
        user_id: &str,
    ) -> bool {
        if merged_scopes.is_empty() {
            return false;
        }

        let scopes_key = format!("{}_scopes", secret_name);
        let stored_scopes: std::collections::HashSet<String> =
            match self.secrets.get_decrypted(user_id, &scopes_key).await {
                Ok(secret) => {
                    let scopes: std::collections::HashSet<String> = secret
                        .expose()
                        .split_whitespace()
                        .map(String::from)
                        .collect();
                    tracing::debug!(
                        secret_name,
                        stored_scopes = ?scopes,
                        "Loaded stored scopes for expansion check"
                    );
                    scopes
                }
                Err(_) => {
                    // No stored scopes record — this is a legacy token created before
                    // scope tracking. Force re-auth to ensure all required scopes are granted.
                    tracing::debug!(
                        secret_name,
                        "No stored scopes record, forcing re-auth for legacy token"
                    );
                    return true;
                }
            };

        // Check if any merged scope is missing from stored scopes
        merged_scopes
            .iter()
            .any(|scope| !stored_scopes.contains(scope))
    }

    /// Find the setup secret names for OAuth client_id and client_secret.
    ///
    /// Scans `setup.required_secrets` for names containing "client_id" and "client_secret".
    /// Returns `(Option<(name, optional)>, Option<(name, optional)>)`.
    async fn find_setup_credential_names(
        &self,
        tool_name: &str,
    ) -> (Option<(String, bool)>, Option<(String, bool)>) {
        let Some(cap) = self.load_tool_capabilities(tool_name).await else {
            return (None, None);
        };
        let Some(setup) = &cap.setup else {
            return (None, None);
        };

        let mut client_id_entry = None;
        let mut client_secret_entry = None;
        for secret in &setup.required_secrets {
            let lower = secret.name.to_lowercase();
            if lower.ends_with("client_id") || lower == "client_id" {
                client_id_entry = Some((secret.name.clone(), secret.optional));
            } else if lower.ends_with("client_secret") || lower == "client_secret" {
                client_secret_entry = Some((secret.name.clone(), secret.optional));
            }
        }
        (client_id_entry, client_secret_entry)
    }

    /// Check if OAuth client credentials (client_id / client_secret) require
    /// user input via the Setup tab. Returns `true` when at least one required
    /// credential cannot be resolved through the full chain:
    /// secrets store → inline → env var → builtin.
    async fn needs_setup_credentials(
        &self,
        name: &str,
        auth: &crate::tools::wasm::AuthCapabilitySchema,
        oauth: &crate::tools::wasm::OAuthConfigSchema,
        user_id: &str,
    ) -> bool {
        let builtin = crate::cli::oauth_defaults::builtin_credentials(&auth.secret_name);
        let (id_entry, secret_entry) = self.find_setup_credential_names(name).await;

        for (entry, inline, env, fallback) in [
            (
                &id_entry,
                &oauth.client_id,
                &oauth.client_id_env,
                builtin.as_ref().map(|c| c.client_id),
            ),
            (
                &secret_entry,
                &oauth.client_secret,
                &oauth.client_secret_env,
                builtin.as_ref().map(|c| c.client_secret),
            ),
        ] {
            let Some((ref setup_name, optional)) = *entry else {
                continue;
            };
            if optional {
                continue;
            }
            let resolved = self
                .resolve_oauth_credential(inline, env, fallback, Some(setup_name), user_id)
                .await
                .is_some();
            if !resolved {
                return true;
            }
        }
        false
    }

    /// Resolve an OAuth credential value via: secrets store → inline → env var → builtin.
    ///
    /// For web gateway users, the secrets store is checked first because client_id/secret
    /// may have been entered via the Setup tab (stored as setup secrets).
    async fn resolve_oauth_credential(
        &self,
        inline_value: &Option<String>,
        env_var_name: &Option<String>,
        builtin_value: Option<&str>,
        setup_secret_name: Option<&str>,
        user_id: &str,
    ) -> Option<String> {
        // 1. Check secrets store (entered via Setup tab)
        if let Some(secret_name) = setup_secret_name
            && let Ok(secret) = self.secrets.get_decrypted(user_id, secret_name).await
        {
            let val = secret.expose();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }

        // 2. Inline value from capabilities.json
        if let Some(val) = inline_value {
            return Some(val.clone());
        }

        // 3. Runtime environment variable
        if let Some(env) = env_var_name
            && let Ok(val) = std::env::var(env)
        {
            return Some(val);
        }

        // 4. Built-in defaults
        builtin_value.map(String::from)
    }

    /// Start the OAuth browser flow for a WASM tool.
    ///
    /// Binds a callback listener, builds the authorization URL, spawns a background
    /// task to wait for the callback and exchange the code, then returns the auth URL
    /// immediately so the web UI can open it.
    async fn start_wasm_oauth(
        &self,
        name: &str,
        auth: &crate::tools::wasm::AuthCapabilitySchema,
        oauth: &crate::tools::wasm::OAuthConfigSchema,
        user_id: &str,
    ) -> Result<AuthResult, String> {
        use crate::cli::oauth_defaults;

        let builtin = oauth_defaults::builtin_credentials(&auth.secret_name);

        // Find setup secret names for client_id and client_secret from capabilities.
        // These are the actual names used in the Setup tab (e.g., "google_oauth_client_id"),
        // which may differ from "{secret_name}_client_id".
        let (setup_client_id_entry, setup_client_secret_entry) =
            self.find_setup_credential_names(name).await;
        let setup_client_id_name = setup_client_id_entry.map(|(n, _)| n);
        let setup_client_secret_name = setup_client_secret_entry.map(|(n, _)| n);

        // Resolve client_id: setup secrets → inline → env var → builtin
        let client_id = self
            .resolve_oauth_credential(
                &oauth.client_id,
                &oauth.client_id_env,
                builtin.as_ref().map(|c| c.client_id),
                setup_client_id_name.as_deref(),
                user_id,
            )
            .await
            .ok_or_else(|| {
                let env_name = oauth
                    .client_id_env
                    .as_deref()
                    .unwrap_or("the client_id env var");
                let mut msg = format!(
                    "OAuth client_id not configured for '{}'. \
                     Enter it in the Setup tab or set {} env var",
                    name, env_name
                );
                if let Some(override_env) =
                    crate::cli::oauth_defaults::builtin_client_id_override_env(&auth.secret_name)
                {
                    msg.push_str(&format!(", or build with {override_env}"));
                }
                msg.push('.');
                msg
            })?;

        // Resolve client_secret (optional for PKCE-only flows)
        let client_secret = self
            .resolve_oauth_credential(
                &oauth.client_secret,
                &oauth.client_secret_env,
                builtin.as_ref().map(|c| c.client_secret),
                setup_client_secret_name.as_deref(),
                user_id,
            )
            .await;

        self.clear_pending_extension_auth(name, user_id).await;

        let redirect_uri = self
            .gateway_callback_redirect_uri()
            .await
            .unwrap_or_else(|| format!("{}/callback", oauth_defaults::callback_url()));

        // Merge scopes from all tools sharing this provider
        let merged_scopes = self
            .collect_shared_scopes(&auth.secret_name, &oauth.scopes, user_id)
            .await;

        // Build authorization URL with CSRF state
        let oauth_result = oauth_defaults::build_oauth_url(
            &oauth.authorization_url,
            &client_id,
            &redirect_uri,
            &merged_scopes,
            oauth.use_pkce,
            &oauth.extra_params,
        );
        let auth_url = oauth_result.url.clone();
        let code_verifier = oauth_result.code_verifier;
        let expected_state = oauth_result.state;

        let display_name = auth
            .display_name
            .clone()
            .unwrap_or_else(|| name.to_string());

        if self.should_use_gateway_mode() {
            // When an exchange proxy is configured, omit the client_secret if it
            // was resolved from built-in defaults (desktop app credentials). The
            // proxy holds the correct web-app secret for platform-registered OAuth
            // apps. Sending the desktop secret would cause a client_id/secret
            // mismatch because the container's GOOGLE_OAUTH_CLIENT_ID is the web
            // app, not the desktop app.
            let proxy_client_secret = oauth_defaults::hosted_proxy_client_secret(
                &client_secret,
                builtin.as_ref(),
                oauth_defaults::exchange_proxy_url().is_some(),
            );

            let flow = oauth_defaults::PendingOAuthFlow {
                extension_name: name.to_string(),
                display_name: display_name.clone(),
                token_url: oauth.token_url.clone(),
                client_id: client_id.clone(),
                client_secret: proxy_client_secret,
                redirect_uri: redirect_uri.clone(),
                code_verifier,
                access_token_field: oauth.access_token_field.clone(),
                secret_name: auth.secret_name.clone(),
                provider: auth.provider.clone(),
                validation_endpoint: auth.validation_endpoint.clone(),
                scopes: merged_scopes,
                user_id: user_id.to_string(),
                secrets: Arc::clone(&self.secrets),
                sse_manager: self.sse_manager.read().await.clone(),
                gateway_token: self.gateway_token.clone(),
                token_exchange_extra_params: std::collections::HashMap::new(),
                client_id_secret_name: None,
                client_secret_secret_name: None,
                client_secret_expires_at: None,
                auto_activate_extension: false,
                created_at: std::time::Instant::now(),
            };

            Ok(self
                .start_gateway_oauth_flow(HostedOAuthFlowStart {
                    name: name.to_string(),
                    kind: ExtensionKind::WasmTool,
                    auth_url,
                    expected_state,
                    flow,
                })
                .await)
        } else {
            // TCP listener mode: bind port 9876 and spawn a background task
            // to wait for the callback. This is the original flow for local/desktop use.
            let listener = oauth_defaults::bind_callback_listener()
                .await
                .map_err(|e| format!("Failed to start OAuth callback listener: {}", e))?;

            let token_url = oauth.token_url.clone();
            let access_token_field = oauth.access_token_field.clone();
            let secret_name = auth.secret_name.clone();
            let provider = auth.provider.clone();
            let validation_endpoint = auth.validation_endpoint.clone();
            let pending_user_id = user_id.to_string();
            let task_user_id = pending_user_id.clone();
            let secrets = Arc::clone(&self.secrets);
            let sse_manager = self.sse_manager.read().await.clone();
            let ext_name = name.to_string();

            let task_handle = tokio::spawn(async move {
                let result: Result<(), String> = async {
                    let code = oauth_defaults::wait_for_callback(
                        listener,
                        "/callback",
                        "code",
                        &display_name,
                        Some(&expected_state),
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                    let token_response = oauth_defaults::exchange_oauth_code(
                        &token_url,
                        &client_id,
                        client_secret.as_deref(),
                        &code,
                        &redirect_uri,
                        code_verifier.as_deref(),
                        &access_token_field,
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                    // Validate the token before storing (catches wrong account, etc.)
                    if let Some(ref validation) = validation_endpoint {
                        oauth_defaults::validate_oauth_token(
                            &token_response.access_token,
                            validation,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    }

                    oauth_defaults::store_oauth_tokens(
                        secrets.as_ref(),
                        &task_user_id,
                        &secret_name,
                        provider.as_deref(),
                        &token_response.access_token,
                        token_response.refresh_token.as_deref(),
                        token_response.expires_in,
                        &merged_scopes,
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                    Ok(())
                }
                .await;

                // Broadcast SSE event
                let (success, message) = match result {
                    Ok(()) => (true, format!("{} authenticated successfully", display_name)),
                    Err(ref e) => (
                        false,
                        format!("{} authentication failed: {}", display_name, e),
                    ),
                };

                match &result {
                    Ok(()) => {
                        tracing::info!(
                            tool = %ext_name,
                            "OAuth completed successfully"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            tool = %ext_name,
                            error = %e,
                            "WASM tool OAuth failed"
                        );
                    }
                }

                if let Some(ref sse) = sse_manager {
                    sse.broadcast(crate::channels::web::types::SseEvent::AuthCompleted {
                        extension_name: ext_name,
                        success,
                        message,
                    });
                }
            });

            // Store pending auth with task handle
            self.pending_auth.write().await.insert(
                (name.to_string(), pending_user_id),
                PendingAuth {
                    _name: name.to_string(),
                    _kind: ExtensionKind::WasmTool,
                    created_at: std::time::Instant::now(),
                    task_handle: Some(task_handle),
                },
            );

            Ok(AuthResult::awaiting_authorization(
                name,
                ExtensionKind::WasmTool,
                auth_url,
                "local".to_string(),
            ))
        }
    }

    /// Returns `true` if a setup secret is an OAuth credential (client_id or client_secret)
    /// that can be resolved without user input — via inline capabilities, env var, or
    /// builtin defaults.
    ///
    /// Used by `check_tool_auth_status()` and `get_setup_schema()` to hide setup fields
    /// that the user doesn't need to fill (e.g., Google tools with builtin credentials).
    fn is_auto_resolved_oauth_field(
        secret_name: &str,
        cap_file: &crate::tools::wasm::CapabilitiesFile,
    ) -> bool {
        let lower = secret_name.to_lowercase();
        let is_client_id = lower.ends_with("client_id") || lower == "client_id";
        let is_client_secret = lower.ends_with("client_secret") || lower == "client_secret";
        if !is_client_id && !is_client_secret {
            return false;
        }
        let Some(ref auth) = cap_file.auth else {
            return false;
        };
        let Some(ref oauth) = auth.oauth else {
            return false;
        };
        let builtin = crate::cli::oauth_defaults::builtin_credentials(&auth.secret_name);

        if is_client_id {
            oauth.client_id.is_some()
                || oauth
                    .client_id_env
                    .as_ref()
                    .is_some_and(|e| std::env::var(e).is_ok())
                || builtin.is_some()
        } else {
            oauth.client_secret.is_some()
                || oauth
                    .client_secret_env
                    .as_ref()
                    .is_some_and(|e| std::env::var(e).is_ok())
                || builtin.is_some()
        }
    }

    /// Determine the auth readiness of a WASM tool.
    async fn check_tool_auth_status(&self, name: &str, user_id: &str) -> ToolAuthState {
        let Some(cap_file) = self.load_tool_capabilities(name).await else {
            return ToolAuthState::NoAuth;
        };

        let saved_fields = self.load_tool_setup_fields(name).await.unwrap_or_default();
        let setup_is_complete = if let Some(setup) = &cap_file.setup {
            let secrets_ready = futures::future::join_all(
                setup
                    .required_secrets
                    .iter()
                    .filter(|s| !s.optional)
                    .filter(|s| !Self::is_auto_resolved_oauth_field(&s.name, &cap_file))
                    .map(|s| self.secrets.exists(&self.user_id, &s.name)),
            )
            .await
            .into_iter()
            .all(|r| r.unwrap_or(false));

            if !secrets_ready {
                false
            } else {
                let mut fields_ready = true;
                for field in &setup.required_fields {
                    if field.optional {
                        continue;
                    }
                    if !self
                        .is_tool_setup_field_provided(name, field, &saved_fields)
                        .await
                    {
                        fields_ready = false;
                        break;
                    }
                }
                fields_ready
            }
        } else {
            true
        };

        if !setup_is_complete {
            return ToolAuthState::NeedsSetup;
        }

        // If the tool declares an auth section, the access token is the
        // authoritative signal — setup secrets (client_id/secret) are
        // intermediate and may be auto-resolved via builtins.
        if let Some(ref auth) = cap_file.auth {
            let has_token = self
                .secrets
                .exists(user_id, &auth.secret_name)
                .await
                .unwrap_or(false)
                || auth
                    .env_var
                    .as_ref()
                    .is_some_and(|v| std::env::var(v).is_ok());
            return if has_token {
                ToolAuthState::Ready
            } else if auth.oauth.is_some() {
                ToolAuthState::NeedsAuth
            } else {
                ToolAuthState::NeedsSetup
            };
        }

        // No auth section — setup_is_complete was already checked above,
        // so if we reach here the setup requirements are satisfied.
        let setup = match &cap_file.setup {
            Some(s) => s,
            None => return ToolAuthState::NoAuth,
        };

        let all_provided = futures::future::join_all(
            setup
                .required_secrets
                .iter()
                .filter(|s| !s.optional)
                .filter(|s| !Self::is_auto_resolved_oauth_field(&s.name, &cap_file))
                .map(|s| self.secrets.exists(user_id, &s.name)),
        )
        .await
        .into_iter()
        .all(|r| r.unwrap_or(false));

        if all_provided {
            ToolAuthState::Ready
        } else {
            ToolAuthState::NeedsSetup
        }
    }

    /// Check auth status for a WASM channel (read-only).
    async fn auth_wasm_channel_status(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<AuthResult, ExtensionError> {
        let cap_path = self
            .wasm_channels_dir
            .join(format!("{}.capabilities.json", name));

        if !cap_path.exists() {
            return Ok(AuthResult::no_auth_required(
                name,
                ExtensionKind::WasmChannel,
            ));
        }

        let cap_bytes = tokio::fs::read(&cap_path)
            .await
            .map_err(|e| ExtensionError::Other(e.to_string()))?;

        let cap_file = crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(&cap_bytes)
            .map_err(|e| ExtensionError::Other(e.to_string()))?;

        let required_secrets = &cap_file.setup.required_secrets;
        let required_fields = &cap_file.setup.required_fields;
        if required_secrets.is_empty() && required_fields.is_empty() {
            return Ok(AuthResult::no_auth_required(
                name,
                ExtensionKind::WasmChannel,
            ));
        }

        let saved_fields = self.load_tool_setup_fields(name).await.unwrap_or_default();

        // Find non-optional secrets that aren't yet stored
        let mut missing = Vec::new();
        for secret in required_secrets {
            if secret.optional {
                continue;
            }
            if !self
                .secrets
                .exists(user_id, &secret.name)
                .await
                .unwrap_or(false)
            {
                missing.push(secret);
            }
        }

        if let Some(secret) = missing.first() {
            return Ok(AuthResult::awaiting_token(
                name,
                ExtensionKind::WasmChannel,
                channel_auth_instructions(secret),
                cap_file.setup.setup_url.clone(),
            ));
        }

        let mut missing_fields = Vec::new();
        for field in required_fields {
            if field.optional {
                continue;
            }
            if !self
                .is_tool_setup_field_provided(name, field, &saved_fields)
                .await
            {
                missing_fields.push(field);
            }
        }

        if missing_fields.is_empty() {
            return Ok(AuthResult::authenticated(name, ExtensionKind::WasmChannel));
        }

        let field = &missing_fields[0];
        Ok(AuthResult::needs_setup(
            name,
            ExtensionKind::WasmChannel,
            field.prompt.clone(),
            cap_file.setup.setup_url.clone(),
        ))
    }

    async fn activate_mcp(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<ActivateResult, ExtensionError> {
        let _lifecycle_guard = self.lock_mcp_lifecycle_for_manual(name).await;
        let current = self
            .get_mcp_server(name, user_id)
            .await
            .map_err(|error| ExtensionError::NotInstalled(error.to_string()))?;
        if current.placement == McpPlacement::Host {
            self.ensure_mcp_owner_available(name, user_id).await?;
        }
        let server = self.set_mcp_enabled(name, user_id, true).await?;
        if server.placement == McpPlacement::JobWorker {
            self.clear_pending_extension_auth(name, user_id).await;
            self.stop_mcp_runtime(name, user_id).await;
            let message = format!(
                "Enabled job-worker MCP server '{name}' for future Docker jobs; command must exist in worker image; running jobs retain immutable MCP snapshots"
            );
            self.broadcast_extension_status(user_id, name, "enabled", Some(&message))
                .await;
            return Ok(ActivateResult {
                name: name.to_string(),
                kind: ExtensionKind::McpServer,
                tools_loaded: Vec::new(),
                message,
            });
        }
        match self
            .activate_mcp_locked(name, user_id, server.clone())
            .await
        {
            Ok(result) => Ok(result),
            Err(error) => Err(self
                .rollback_mcp_activation_unless_auth_required(user_id, &server, error)
                .await),
        }
    }

    async fn activate_mcp_if_enabled(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<Option<ActivateResult>, ExtensionError> {
        let _lifecycle_guard = self.lock_mcp_lifecycle_for_manual(name).await;
        let server = self
            .get_mcp_server(name, user_id)
            .await
            .map_err(|error| ExtensionError::NotInstalled(error.to_string()))?;
        if !server.enabled {
            return Ok(None);
        }
        if server.placement == McpPlacement::JobWorker {
            self.stop_mcp_runtime(name, user_id).await;
            return Ok(Some(ActivateResult {
                name: name.to_string(),
                kind: ExtensionKind::McpServer,
                tools_loaded: Vec::new(),
                message: format!(
                    "Job-worker MCP server '{name}' remains enabled for future Docker jobs; running jobs retain immutable MCP snapshots"
                ),
            }));
        }
        self.ensure_mcp_owner_available(name, user_id).await?;
        self.activate_mcp_locked(name, user_id, server)
            .await
            .map(Some)
    }

    async fn ensure_mcp_owner_available(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<(), ExtensionError> {
        if self
            .active_mcp_servers
            .read()
            .await
            .get(name)
            .is_some_and(|runtime| runtime.owner_id != user_id)
        {
            return Err(ExtensionError::ActivationFailed(format!(
                "MCP server '{name}' is already active for a different owner"
            )));
        }
        Ok(())
    }

    async fn prepare_mcp_runtime(
        &self,
        name: &str,
        user_id: &str,
        server: McpServerConfig,
    ) -> Result<PreparedMcpRuntime, ExtensionError> {
        if server.placement != McpPlacement::Host {
            return Err(ExtensionError::ActivationFailed(format!(
                "Host runtime cannot prepare job-worker MCP server '{name}'"
            )));
        }
        // Resolve operator-declared external runtime descriptors from the same
        // configuration the server entry came from. An unbuildable registry
        // fails closed (external transports get an unknown-runtime error).
        // The load also re-runs the marketplace revocation gate; when a store
        // is configured, re-check this server against the freshest cached list
        // to close the race between the caller's load and activation.
        let (external_runtimes, revocation_store) = match self.load_mcp_servers(user_id).await {
            Ok(file) => {
                let store = file
                    .marketplace
                    .as_ref()
                    .and_then(|marketplace| marketplace.revocation_store().ok().flatten());
                (
                    Arc::new(file.external_runtime_registry().unwrap_or_default()),
                    store,
                )
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "MCP configuration reload failed during activation; external transports fail closed"
                );
                (
                    Arc::new(crate::tools::mcp::descriptor::ExternalRuntimeRegistry::new()),
                    None,
                )
            }
        };
        if let (Some(store), Some(provenance)) = (&revocation_store, &server.provenance) {
            store.check_provenance(provenance).map_err(|error| {
                ExtensionError::ActivationFailed(format!(
                    "MCP server '{name}' is revoked by the marketplace list: {error}"
                ))
            })?;
        }
        let client = Arc::new(
            crate::tools::mcp::create_client_from_config_with_external_runtimes(
                server,
                &self.mcp_session_manager,
                &self.mcp_process_manager,
                Some(Arc::clone(&self.secrets)),
                user_id,
                external_runtimes,
            )
            .await
            .map_err(|error| ExtensionError::ActivationFailed(error.to_string()))?,
        );

        if let Err(error) = client.list_tools().await {
            self.cleanup_failed_mcp_activation(name, client.as_ref())
                .await;
            return Err(Self::classify_mcp_activation_error(error.to_string()));
        }
        match client.create_tools().await {
            Ok(tools) => Ok(PreparedMcpRuntime { client, tools }),
            Err(error) => {
                self.cleanup_failed_mcp_activation(name, client.as_ref())
                    .await;
                Err(ExtensionError::ActivationFailed(error.to_string()))
            }
        }
    }

    async fn activate_mcp_locked(
        &self,
        name: &str,
        user_id: &str,
        server: McpServerConfig,
    ) -> Result<ActivateResult, ExtensionError> {
        if server.placement != McpPlacement::Host {
            return Err(ExtensionError::ActivationFailed(format!(
                "Host runtime cannot activate job-worker MCP server '{name}'"
            )));
        }
        let active_tools = self
            .active_mcp_servers
            .read()
            .await
            .get(name)
            .filter(|runtime| {
                runtime.owner_id == user_id && runtime.health == McpRuntimeHealth::Healthy
            })
            .map(|runtime| {
                runtime
                    .tools
                    .iter()
                    .map(|tool| tool.name().to_string())
                    .collect::<Vec<_>>()
            });
        if let Some(tools_loaded) = active_tools {
            return Ok(ActivateResult {
                name: name.to_string(),
                kind: ExtensionKind::McpServer,
                tools_loaded,
                message: format!("MCP server '{name}' already active"),
            });
        }

        self.stop_mcp_runtime(name, user_id).await;
        let managed_stdio = matches!(
            server.effective_transport(),
            crate::tools::mcp::config::EffectiveTransport::Stdio { .. }
        );
        let managed_wasm = matches!(
            server.effective_transport(),
            crate::tools::mcp::config::EffectiveTransport::Wasm { .. }
        );
        let prepared = self.prepare_mcp_runtime(name, user_id, server).await?;
        if let Err(conflicts) = self
            .tool_registry
            .replace_runtime_tools(&[], prepared.tools.clone())
            .await
        {
            self.cleanup_failed_mcp_activation(name, prepared.client.as_ref())
                .await;
            return Err(ExtensionError::ActivationFailed(format!(
                "MCP tool registration conflicts: {}",
                conflicts.join(", ")
            )));
        }
        let tool_names = prepared
            .tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect::<Vec<_>>();
        let generation = self.next_mcp_runtime_generation();
        let tools_generation = prepared.client.tools_generation();

        self.active_mcp_servers.write().await.insert(
            name.to_string(),
            ActiveMcpServer {
                owner_id: user_id.to_string(),
                client: prepared.client,
                tools: prepared.tools,
                tools_generation,
                managed_stdio,
                managed_wasm,
                generation,
                health: McpRuntimeHealth::Healthy,
                restart_attempts: 0,
                healthy_since: Instant::now(),
                retry_after: None,
            },
        );

        tracing::info!(server = %name, tools = tool_names.len(), "Activated MCP server");
        self.broadcast_extension_status(user_id, name, "active", None)
            .await;

        Ok(ActivateResult {
            name: name.to_string(),
            kind: ExtensionKind::McpServer,
            tools_loaded: tool_names,
            message: format!("Connected to '{name}' and loaded tools"),
        })
    }

    async fn activate_wasm_tool(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<ActivateResult, ExtensionError> {
        // Check if already active without treating another runtime's name as
        // this WASM tool's registration.
        if let Some(registered) = self.tool_registry.get(name).await {
            if !registered.is_wasm_tool() {
                return Err(ExtensionError::ActivationFailed(format!(
                    "Tool name '{name}' is owned by another runtime"
                )));
            }
            return Ok(ActivateResult {
                name: name.to_string(),
                kind: ExtensionKind::WasmTool,
                tools_loaded: vec![name.to_string()],
                message: format!("WASM tool '{}' already active", name),
            });
        }

        // Check auth status — block activation if required secrets are missing.
        // NeedsAuth (OAuth not yet completed) is allowed because configure() loads
        // the tool first, then starts the OAuth flow to obtain the token.
        let auth_state = self.check_tool_auth_status(name, user_id).await;
        if auth_state == ToolAuthState::NeedsSetup {
            return Err(ExtensionError::ActivationFailed(format!(
                "Tool '{}' requires configuration. Use the setup form to provide credentials.",
                name
            )));
        }

        let runtime = self.wasm_tool_runtime.as_ref().ok_or_else(|| {
            ExtensionError::ActivationFailed("WASM runtime not available".to_string())
        })?;

        let wasm_path = self.wasm_tools_dir.join(format!("{}.wasm", name));
        if !wasm_path.exists() {
            return Err(ExtensionError::NotInstalled(format!(
                "WASM tool '{}' not found at {}",
                name,
                wasm_path.display()
            )));
        }

        let cap_path = self
            .wasm_tools_dir
            .join(format!("{}.capabilities.json", name));
        let cap_path_option = if cap_path.exists() {
            Some(cap_path.as_path())
        } else {
            None
        };

        let loader = WasmToolLoader::new(Arc::clone(runtime), Arc::clone(&self.tool_registry))
            .with_secrets_store(Arc::clone(&self.secrets));
        loader
            .load_from_files(name, &wasm_path, cap_path_option)
            .await
            .map_err(|e| ExtensionError::ActivationFailed(e.to_string()))?;

        if let Some(ref hooks) = self.hooks
            && let Some(cap_path) = cap_path_option
        {
            let source = format!("plugin.tool:{}", name);
            let registration =
                crate::hooks::bootstrap::register_plugin_bundle_from_capabilities_file(
                    hooks, &source, cap_path,
                )
                .await;

            if registration.total_registered() > 0 {
                tracing::info!(
                    extension = name,
                    hooks = registration.hooks,
                    outbound_webhooks = registration.outbound_webhooks,
                    "Registered plugin hooks for activated WASM tool"
                );
            }

            if registration.errors > 0 {
                tracing::warn!(
                    extension = name,
                    errors = registration.errors,
                    "Some plugin hooks failed to register"
                );
            }
        }

        tracing::info!("Activated WASM tool '{}'", name);

        Ok(ActivateResult {
            name: name.to_string(),
            kind: ExtensionKind::WasmTool,
            tools_loaded: vec![name.to_string()],
            message: format!("WASM tool '{}' loaded and ready", name),
        })
    }

    /// Activate a WASM channel at runtime without restarting.
    ///
    /// Loads the channel from its WASM file, injects credentials and config,
    /// registers it with the webhook router, and hot-adds it to the channel manager
    /// so its stream feeds into the agent loop.
    async fn activate_wasm_channel(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<ActivateResult, ExtensionError> {
        // If already active, re-inject credentials and refresh webhook secret.
        // Handles the case where a channel was loaded at startup before the
        // user saved secrets via the web UI.
        {
            let active = self.active_channel_names.read().await;
            if active.contains(name) {
                return self.refresh_active_channel(name, user_id).await;
            }
        }

        // Verify runtime infrastructure is available and clone Arcs so we don't
        // hold the RwLock guard across awaits.
        let (
            channel_runtime,
            channel_manager,
            pairing_store,
            wasm_channel_router,
            wasm_channel_owner_ids,
            wasm_channel_owner_actor_ids,
            tool_activity,
        ) = {
            let rt_guard = self.channel_runtime.read().await;
            let rt = rt_guard.as_ref().ok_or_else(|| {
                ExtensionError::ActivationFailed("WASM channel runtime not configured".to_string())
            })?;
            (
                Arc::clone(&rt.wasm_channel_runtime),
                Arc::clone(&rt.channel_manager),
                Arc::clone(&rt.pairing_store),
                Arc::clone(&rt.wasm_channel_router),
                rt.wasm_channel_owner_ids.clone(),
                rt.wasm_channel_owner_actor_ids.clone(),
                rt.tool_activity,
            )
        };

        // Check auth status first
        let auth_state = self.check_channel_auth_status(name, user_id).await;
        if auth_state != ToolAuthState::Ready && auth_state != ToolAuthState::NoAuth {
            return Err(ExtensionError::ActivationFailed(format!(
                "Channel '{}' requires configuration. Use the setup form to provide credentials.",
                name
            )));
        }

        // Load the channel from files
        let wasm_path = self.wasm_channels_dir.join(format!("{}.wasm", name));
        let cap_path = self
            .wasm_channels_dir
            .join(format!("{}.capabilities.json", name));
        let cap_path_option = if cap_path.exists() {
            Some(cap_path.as_path())
        } else {
            None
        };

        let loaded = {
            let settings_store: Option<Arc<dyn crate::db::SettingsStore>> =
                self.store.as_ref().map(|db| Arc::clone(db) as _);
            let loader = WasmChannelLoader::new(
                Arc::clone(&channel_runtime),
                Arc::clone(&pairing_store),
                settings_store,
                self.user_id.clone(),
            )
            .with_secrets_store(Arc::clone(&self.secrets))
            .with_tool_activity(tool_activity);
            loader
                .load_from_files(name, &wasm_path, cap_path_option)
                .await
                .map_err(|e| ExtensionError::ActivationFailed(e.to_string()))?
        };

        self.complete_loaded_wasm_channel_activation(
            name,
            loaded,
            &channel_manager,
            &wasm_channel_router,
            wasm_channel_owner_actor_ids
                .get(name)
                .cloned()
                .or_else(|| wasm_channel_owner_ids.get(name).map(ToString::to_string)),
            wasm_channel_owner_ids.get(name).copied(),
        )
        .await
    }

    async fn complete_loaded_wasm_channel_activation(
        &self,
        requested_name: &str,
        loaded: LoadedChannel,
        channel_manager: &Arc<ChannelManager>,
        wasm_channel_router: &Arc<WasmChannelRouter>,
        owner_actor_id: Option<String>,
        owner_id: Option<i64>,
    ) -> Result<ActivateResult, ExtensionError> {
        let channel_name = loaded.name().to_string();
        let webhook_secret_name = loaded.webhook_secret_name();
        let secret_header = loaded.webhook_secret_header().map(|s| s.to_string());
        let sig_key_secret_name = loaded.signature_key_secret_name();
        let hmac_secret_name = loaded.hmac_secret_name();

        // Get webhook secret from secrets store
        let webhook_secret = self
            .secrets
            .get_decrypted(&self.user_id, &webhook_secret_name)
            .await
            .ok()
            .map(|s| s.expose().to_string());

        let channel_arc = Arc::new(loaded.channel.with_owner_actor_id(owner_actor_id));

        // Inject runtime config (tunnel_url, webhook_secret, owner_id)
        {
            let resolved_owner_id = owner_id.or(self.current_channel_owner_id(&channel_name).await);
            let tunnel_url = self.current_tunnel_url();
            let mut config_updates = build_wasm_channel_runtime_config_updates(
                tunnel_url.as_deref(),
                webhook_secret.as_deref(),
                resolved_owner_id,
            );
            config_updates.extend(
                self.load_channel_runtime_config_overrides(&channel_name)
                    .await,
            );
            config_updates.extend(
                load_channel_config_secret_overrides(
                    Some(self.secrets.as_ref()),
                    &channel_name,
                    &self.user_id,
                )
                .await,
            );

            if !config_updates.is_empty() {
                channel_arc.update_config(config_updates).await;
                tracing::info!(
                    channel = %channel_name,
                    has_tunnel = tunnel_url.is_some(),
                    has_webhook_secret = webhook_secret.is_some(),
                    "Injected runtime config into hot-activated channel"
                );
            }
        }

        // Register with webhook router
        {
            let webhook_path = format!("/webhook/{}", channel_name);
            let endpoints = vec![RegisteredEndpoint {
                channel_name: channel_name.clone(),
                path: webhook_path,
                methods: vec!["POST".to_string()],
                require_secret: webhook_secret.is_some(),
            }];

            wasm_channel_router
                .register(
                    Arc::clone(&channel_arc),
                    endpoints,
                    webhook_secret,
                    secret_header,
                )
                .await;
            tracing::info!(channel = %channel_name, "Registered hot-activated channel with webhook router");

            // Register Ed25519 signature key if declared in capabilities
            if let Some(ref sig_key_name) = sig_key_secret_name
                && let Ok(key_secret) = self
                    .secrets
                    .get_decrypted(&self.user_id, sig_key_name)
                    .await
            {
                match wasm_channel_router
                    .register_signature_key(&channel_name, key_secret.expose())
                    .await
                {
                    Ok(()) => {
                        tracing::info!(channel = %channel_name, "Registered signature key for hot-activated channel")
                    }
                    Err(e) => {
                        tracing::error!(channel = %channel_name, error = %e, "Failed to register signature key")
                    }
                }
            }

            // Register HMAC signing secret if declared in capabilities
            if let Some(hmac_name) = &hmac_secret_name {
                match self.secrets.get_decrypted(&self.user_id, hmac_name).await {
                    Ok(secret) => {
                        wasm_channel_router
                            .register_hmac_secret(&channel_name, secret.expose())
                            .await;
                        tracing::info!(channel = %channel_name, "Registered HMAC signing secret for hot-activated channel");
                    }
                    Err(e) => {
                        tracing::warn!(channel = %channel_name, error = %e, "HMAC secret not found");
                    }
                }
            }
        }

        // Inject credentials
        match inject_channel_credentials_from_secrets(
            &channel_arc,
            Some(self.secrets.as_ref()),
            &channel_name,
            &self.user_id,
        )
        .await
        {
            Ok(count) => {
                if count > 0 {
                    tracing::info!(
                        channel = %channel_name,
                        credentials_injected = count,
                        "Credentials injected into hot-activated channel"
                    );
                }
            }
            Err(e) => {
                tracing::error!(
                    channel = %channel_name,
                    error = %e,
                    "Failed to inject credentials into hot-activated channel"
                );
            }
        }

        // Hot-add the channel to the running agent
        channel_manager
            .hot_add(Box::new(SharedWasmChannel::new(channel_arc)))
            .await
            .map_err(|e| ExtensionError::ActivationFailed(e.to_string()))?;

        // Mark as active
        self.active_channel_names
            .write()
            .await
            .insert(channel_name.clone());

        // Persist activation state so the channel auto-activates on restart
        self.persist_active_channels(&self.user_id).await;

        tracing::info!(channel = %channel_name, "Hot-activated WASM channel");

        Ok(ActivateResult {
            name: channel_name,
            kind: ExtensionKind::WasmChannel,
            tools_loaded: Vec::new(),
            message: format!("Channel '{}' activated and running", requested_name),
        })
    }

    /// Refresh credentials and webhook secret on an already-active channel.
    ///
    /// Called when the user saves new secrets via the setup form for a channel
    /// that was loaded at startup (possibly without credentials).
    async fn refresh_active_channel(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<ActivateResult, ExtensionError> {
        let router = {
            let rt_guard = self.channel_runtime.read().await;
            match rt_guard.as_ref() {
                Some(rt) => Arc::clone(&rt.wasm_channel_router),
                None => {
                    return Ok(ActivateResult {
                        name: name.to_string(),
                        kind: ExtensionKind::WasmChannel,
                        tools_loaded: Vec::new(),
                        message: format!("Channel '{}' is already active", name),
                    });
                }
            }
        };

        let webhook_path = format!("/webhook/{}", name);
        let existing_channel = match router.get_channel_for_path(&webhook_path).await {
            Some(ch) => ch,
            None => {
                return Ok(ActivateResult {
                    name: name.to_string(),
                    kind: ExtensionKind::WasmChannel,
                    tools_loaded: Vec::new(),
                    message: format!("Channel '{}' is already active", name),
                });
            }
        };

        // Re-inject credentials from secrets store into the running channel
        let cred_count = match inject_channel_credentials_from_secrets(
            &existing_channel,
            Some(self.secrets.as_ref()),
            name,
            user_id,
        )
        .await
        {
            Ok(count) => count,
            Err(e) => {
                tracing::warn!(
                    channel = %name,
                    error = %e,
                    "Failed to refresh credentials on already-active channel"
                );
                0
            }
        };

        // Load capabilities file once to extract all secret names
        let cap_path = self
            .wasm_channels_dir
            .join(format!("{}.capabilities.json", name));
        let capabilities_file = match tokio::fs::read(&cap_path).await {
            Ok(bytes) => crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(&bytes).ok(),
            Err(_) => None,
        };

        // Extract all secret names from the capabilities file
        let webhook_secret_name = capabilities_file
            .as_ref()
            .map(|f| f.webhook_secret_name())
            .unwrap_or_else(|| format!("{}_webhook_secret", name));

        let sig_key_secret_name = capabilities_file
            .as_ref()
            .and_then(|f| f.signature_key_secret_name().map(|s| s.to_string()));

        let hmac_secret_name = capabilities_file
            .as_ref()
            .and_then(|f| f.hmac_secret_name().map(|s| s.to_string()));

        let tunnel_url = self.current_tunnel_url();
        let mut config_updates = build_wasm_channel_runtime_config_updates(
            tunnel_url.as_deref(),
            None,
            self.current_channel_owner_id(name).await,
        );
        config_updates.extend(self.load_channel_runtime_config_overrides(name).await);
        config_updates.extend(
            load_channel_config_secret_overrides(Some(self.secrets.as_ref()), name, user_id).await,
        );
        let mut should_rerun_on_start = false;

        // Refresh webhook secret
        if let Ok(secret) = self
            .secrets
            .get_decrypted(user_id, &webhook_secret_name)
            .await
        {
            router
                .update_secret(name, secret.expose().to_string())
                .await;
            config_updates.insert(
                "webhook_secret".to_string(),
                serde_json::Value::String(secret.expose().to_string()),
            );
            should_rerun_on_start = true;
        }

        // Refresh signature key
        if let Some(ref sig_key_name) = sig_key_secret_name
            && let Ok(key_secret) = self.secrets.get_decrypted(user_id, sig_key_name).await
        {
            match router
                .register_signature_key(name, key_secret.expose())
                .await
            {
                Ok(()) => {
                    tracing::info!(channel = %name, "Refreshed signature verification key")
                }
                Err(e) => {
                    tracing::error!(channel = %name, error = %e, "Failed to refresh signature key")
                }
            }
        }

        // Refresh HMAC signing secret
        if let Some(ref hmac_secret_name_ref) = hmac_secret_name {
            match self
                .secrets
                .get_decrypted(user_id, hmac_secret_name_ref)
                .await
            {
                Ok(secret) => {
                    router.register_hmac_secret(name, secret.expose()).await;
                    tracing::info!(channel = %name, "Refreshed HMAC signing secret");
                }
                Err(e) => {
                    tracing::warn!(channel = %name, error = %e, "HMAC secret not found");
                }
            }
        }

        if !config_updates.is_empty() {
            existing_channel.update_config(config_updates).await;
            should_rerun_on_start = true;
        }

        // Re-call on_start() to trigger webhook registration with the
        // now-available credentials.
        if cred_count > 0 || should_rerun_on_start {
            match existing_channel.call_on_start().await {
                Ok(_config) => {
                    tracing::info!(
                        channel = %name,
                        "Re-ran on_start after credential refresh (webhook re-registered)"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        channel = %name,
                        error = %e,
                        "on_start failed after credential refresh"
                    );
                }
            }
        }

        tracing::info!(
            channel = %name,
            credentials_refreshed = cred_count,
            "Refreshed credentials and config on already-active channel"
        );

        Ok(ActivateResult {
            name: name.to_string(),
            kind: ExtensionKind::WasmChannel,
            tools_loaded: Vec::new(),
            message: format!(
                "Channel '{}' is already active; refreshed {} credential(s)",
                name, cred_count
            ),
        })
    }

    /// Determine what kind of installed extension this is.
    async fn determine_installed_kind(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<ExtensionKind, ExtensionError> {
        // Check MCP servers first
        if self.get_mcp_server(name, user_id).await.is_ok() {
            return Ok(ExtensionKind::McpServer);
        }

        // Check WASM tools
        let wasm_path = self.wasm_tools_dir.join(format!("{}.wasm", name));
        if wasm_path.exists() {
            return Ok(ExtensionKind::WasmTool);
        }

        // Check WASM channels
        let channel_path = self.wasm_channels_dir.join(format!("{}.wasm", name));
        if channel_path.exists() {
            return Ok(ExtensionKind::WasmChannel);
        }

        Err(ExtensionError::NotInstalled(format!(
            "'{}' is not installed as an MCP server, WASM tool, or WASM channel",
            name
        )))
    }

    /// Reject names containing path separators or traversal sequences.
    fn validate_extension_name(name: &str) -> Result<(), ExtensionError> {
        if name.contains('/') || name.contains('\\') || name.contains("..") || name.contains('\0') {
            return Err(ExtensionError::InstallFailed(format!(
                "Invalid extension name '{}': contains path separator or traversal characters",
                name
            )));
        }
        Ok(())
    }

    fn setup_fields_setting_key(name: &str) -> String {
        format!("extensions.{name}.setup_fields")
    }

    fn is_allowed_setup_setting_path(name: &str, setting_path: &str) -> bool {
        let namespaced_prefix = format!("extensions.{name}.");
        setting_path.starts_with(&namespaced_prefix)
            || ALLOWED_GLOBAL_SETUP_SETTING_PATHS.contains(&setting_path)
    }

    fn validate_setup_setting_path(name: &str, setting_path: &str) -> Result<(), ExtensionError> {
        if Self::is_allowed_setup_setting_path(name, setting_path) {
            return Ok(());
        }

        Err(ExtensionError::Other(format!(
            "Invalid setting_path '{}' for extension '{}': only 'extensions.{}.*' or approved settings may be written",
            setting_path, name, name
        )))
    }

    fn setting_value_is_present(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Null => false,
            serde_json::Value::String(s) => !s.trim().is_empty(),
            serde_json::Value::Array(a) => !a.is_empty(),
            serde_json::Value::Object(o) => !o.is_empty(),
            _ => true,
        }
    }

    async fn load_tool_setup_fields(
        &self,
        name: &str,
    ) -> Result<HashMap<String, String>, ExtensionError> {
        let Some(ref store) = self.store else {
            return Ok(HashMap::new());
        };

        let key = Self::setup_fields_setting_key(name);
        match store.get_setting(&self.user_id, &key).await {
            Ok(Some(value)) => serde_json::from_value::<HashMap<String, String>>(value)
                .map_err(|e| ExtensionError::Other(format!("Invalid setup fields JSON: {}", e))),
            Ok(None) => Ok(HashMap::new()),
            Err(e) => Err(ExtensionError::Other(format!(
                "Failed to read setup fields for '{}': {}",
                name, e
            ))),
        }
    }

    async fn resolve_setup_field_value(
        &self,
        name: &str,
        field: &crate::tools::wasm::ToolFieldSetupSchema,
        saved_fields: &HashMap<String, String>,
    ) -> Option<serde_json::Value> {
        if let Some(value) = saved_fields.get(&field.name)
            && !value.trim().is_empty()
        {
            return Some(serde_json::Value::String(value.clone()));
        }

        if let (Some(store), Some(setting_path)) = (&self.store, &field.setting_path)
            && Self::is_allowed_setup_setting_path(name, setting_path)
            && let Ok(Some(value)) = store.get_setting(&self.user_id, setting_path).await
            && Self::setting_value_is_present(&value)
        {
            return Some(value);
        }

        None
    }

    async fn save_tool_setup_fields(
        &self,
        name: &str,
        fields: &HashMap<String, String>,
    ) -> Result<(), ExtensionError> {
        let store = self.store.as_ref().ok_or_else(|| {
            ExtensionError::Other("Settings store unavailable for setup field persistence".into())
        })?;
        let key = Self::setup_fields_setting_key(name);
        let value = serde_json::to_value(fields)
            .map_err(|e| ExtensionError::Other(format!("Failed to encode setup fields: {}", e)))?;
        store
            .set_setting(&self.user_id, &key, &value)
            .await
            .map_err(|e| {
                ExtensionError::Other(format!(
                    "Failed to persist setup fields for '{}': {}",
                    name, e
                ))
            })
    }

    async fn is_tool_setup_field_provided(
        &self,
        name: &str,
        field: &crate::tools::wasm::ToolFieldSetupSchema,
        saved_fields: &HashMap<String, String>,
    ) -> bool {
        if saved_fields
            .get(&field.name)
            .is_some_and(|value| !value.trim().is_empty())
        {
            return true;
        }

        if let (Some(store), Some(setting_path)) = (&self.store, &field.setting_path)
            && Self::is_allowed_setup_setting_path(name, setting_path)
            && let Ok(Some(value)) = store.get_setting(&self.user_id, setting_path).await
        {
            return Self::setting_value_is_present(&value);
        }

        false
    }

    async fn cleanup_expired_auths(&self) {
        let mut pending = self.pending_auth.write().await;
        pending.retain(|_, auth| {
            let expired = auth.created_at.elapsed() >= std::time::Duration::from_secs(300);
            if expired {
                // Abort the background listener task to free port 9876
                if let Some(ref handle) = auth.task_handle {
                    handle.abort();
                }
            }
            !expired
        });
    }

    /// Get the setup schema for an extension (secret/text fields and their status).
    pub async fn get_setup_schema(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<ExtensionSetupSchema, ExtensionError> {
        Self::validate_extension_name(name)?;
        let kind = self.determine_installed_kind(name, user_id).await?;
        match kind {
            ExtensionKind::WasmChannel => {
                let cap_path = self
                    .wasm_channels_dir
                    .join(format!("{}.capabilities.json", name));
                if !cap_path.exists() {
                    return Ok(ExtensionSetupSchema {
                        secrets: Vec::new(),
                        fields: Vec::new(),
                    });
                }
                let cap_bytes = tokio::fs::read(&cap_path)
                    .await
                    .map_err(|e| ExtensionError::Other(e.to_string()))?;
                let cap_file =
                    crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(&cap_bytes)
                        .map_err(|e| ExtensionError::Other(e.to_string()))?;

                let mut secrets = Vec::new();
                let mut fields = Vec::new();
                let saved_fields = self.load_tool_setup_fields(name).await.unwrap_or_default();
                for secret in &cap_file.setup.required_secrets {
                    let provided = self
                        .secrets
                        .exists(user_id, &secret.name)
                        .await
                        .unwrap_or(false);
                    secrets.push(crate::channels::web::types::SecretFieldInfo {
                        name: secret.name.clone(),
                        prompt: secret.prompt.clone(),
                        optional: secret.optional,
                        provided,
                        auto_generate: secret.auto_generate.is_some(),
                    });
                }

                for field in &cap_file.setup.required_fields {
                    let provided = self
                        .is_tool_setup_field_provided(name, field, &saved_fields)
                        .await;
                    fields.push(crate::channels::web::types::SetupFieldInfo {
                        name: field.name.clone(),
                        prompt: field.prompt.clone(),
                        optional: field.optional,
                        provided,
                        input_type: field.input_type,
                    });
                }
                Ok(ExtensionSetupSchema { secrets, fields })
            }
            ExtensionKind::WasmTool => {
                let Some(cap_file) = self.load_tool_capabilities(name).await else {
                    return Ok(ExtensionSetupSchema {
                        secrets: Vec::new(),
                        fields: Vec::new(),
                    });
                };

                let mut secrets = Vec::new();
                let mut fields = Vec::new();
                if let Some(setup) = &cap_file.setup {
                    let saved_fields = self.load_tool_setup_fields(name).await.unwrap_or_default();

                    for secret in &setup.required_secrets {
                        if Self::is_auto_resolved_oauth_field(&secret.name, &cap_file) {
                            continue;
                        }
                        let provided = self
                            .secrets
                            .exists(user_id, &secret.name)
                            .await
                            .unwrap_or(false);
                        secrets.push(crate::channels::web::types::SecretFieldInfo {
                            name: secret.name.clone(),
                            prompt: secret.prompt.clone(),
                            optional: secret.optional,
                            provided,
                            auto_generate: false,
                        });
                    }

                    for field in &setup.required_fields {
                        let provided = self
                            .is_tool_setup_field_provided(name, field, &saved_fields)
                            .await;
                        fields.push(crate::channels::web::types::SetupFieldInfo {
                            name: field.name.clone(),
                            prompt: field.prompt.clone(),
                            optional: field.optional,
                            provided,
                            input_type: field.input_type,
                        });
                    }
                }
                Ok(ExtensionSetupSchema { secrets, fields })
            }
            _ => Ok(ExtensionSetupSchema {
                secrets: Vec::new(),
                fields: Vec::new(),
            }),
        }
    }

    /// Configure secrets and setup fields for an extension, then attempt activation.
    ///
    /// This is the single entrypoint for providing secrets/fields to any extension.
    /// Both the chat auth flow and the Extensions tab setup form call this method.
    ///
    /// - Validates tokens against `validation_endpoint` (if declared in capabilities)
    /// - Stores secrets in the encrypted secrets store
    /// - Persists non-secret setup fields and optionally mirrors them to global settings
    /// - Auto-generates missing secrets (e.g., webhook keys)
    /// - Activates the extension after configuration
    pub async fn configure(
        &self,
        name: &str,
        secrets: &std::collections::HashMap<String, String>,
        fields: &std::collections::HashMap<String, String>,
        user_id: &str,
    ) -> Result<ConfigureResult, ExtensionError> {
        Self::validate_extension_name(name)?;
        let kind = self.determine_installed_kind(name, user_id).await?;

        // Load allowed secret names and tool setup field definitions from capabilities.
        let mut channel_cap_file: Option<crate::channels::wasm::ChannelCapabilitiesFile> = None;
        let (allowed_secrets, setup_fields): (
            std::collections::HashSet<String>,
            Vec<crate::tools::wasm::ToolFieldSetupSchema>,
        ) = match kind {
            ExtensionKind::WasmChannel => {
                let cap_path = self
                    .wasm_channels_dir
                    .join(format!("{}.capabilities.json", name));
                if !cap_path.exists() {
                    return Err(ExtensionError::Other(format!(
                        "Capabilities file not found for '{}'",
                        name
                    )));
                }
                let cap_bytes = tokio::fs::read(&cap_path)
                    .await
                    .map_err(|e| ExtensionError::Other(e.to_string()))?;
                let cap_file =
                    crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(&cap_bytes)
                        .map_err(|e| ExtensionError::Other(e.to_string()))?;
                let mut names: std::collections::HashSet<String> = cap_file
                    .setup
                    .required_secrets
                    .iter()
                    .map(|s| s.name.clone())
                    .collect();
                if names.is_empty() && cap_file.setup.required_fields.is_empty() {
                    return Err(ExtensionError::Other(format!(
                        "Channel '{}' has no setup schema — nothing to configure",
                        name
                    )));
                }
                let required_fields = cap_file.setup.required_fields.clone();
                channel_cap_file = Some(cap_file);
                // Keep `names` mutable in case channel auth grows extra secret sources later.
                (std::mem::take(&mut names), required_fields)
            }
            ExtensionKind::WasmTool => {
                let cap_file = self.load_tool_capabilities(name).await.ok_or_else(|| {
                    ExtensionError::Other(format!("Capabilities file not found for '{}'", name))
                })?;
                let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
                let mut required_fields = Vec::new();
                if let Some(ref s) = cap_file.setup {
                    names.extend(s.required_secrets.iter().map(|s| s.name.clone()));
                    required_fields = s.required_fields.clone();
                }
                if let Some(ref auth) = cap_file.auth {
                    names.insert(auth.secret_name.clone());
                }
                if names.is_empty() && required_fields.is_empty() {
                    return Err(ExtensionError::Other(format!(
                        "Tool '{}' has no setup or auth schema — nothing to configure",
                        name
                    )));
                }
                (names, required_fields)
            }
            ExtensionKind::McpServer => {
                let server = self
                    .get_mcp_server(name, user_id)
                    .await
                    .map_err(|e| ExtensionError::NotInstalled(e.to_string()))?;
                let mut names = std::collections::HashSet::new();
                names.insert(server.token_secret_name());
                (names, Vec::new())
            }
        };

        let allowed_fields: std::collections::HashSet<String> =
            setup_fields.iter().map(|f| f.name.clone()).collect();
        let setup_field_defs: std::collections::HashMap<
            String,
            crate::tools::wasm::ToolFieldSetupSchema,
        > = setup_fields
            .into_iter()
            .map(|f| (f.name.clone(), f))
            .collect();

        // Validate secrets against the validation_endpoint if declared in capabilities.
        // The endpoint URL template uses {secret_name} placeholders that are
        // substituted with the provided secret value before making the request.
        if let Some(ref cap_file) = channel_cap_file
            && let Some(ref endpoint_template) = cap_file.setup.validation_endpoint
            && let Some(secret_def) = cap_file
                .setup
                .required_secrets
                .iter()
                .find(|s| !s.optional && secrets.contains_key(&s.name))
            && let Some(token_value) = secrets.get(&secret_def.name)
        {
            let token = token_value.trim();
            if !token.is_empty() {
                let encoded =
                    url::form_urlencoded::byte_serialize(token.as_bytes()).collect::<String>();
                let url = endpoint_template.replace(&format!("{{{}}}", secret_def.name), &encoded);
                // SSRF defense: block private IPs, localhost, cloud metadata endpoints
                crate::tools::builtin::skill_tools::validate_fetch_url(&url)
                    .map_err(|e| ExtensionError::Other(format!("SSRF blocked: {}", e)))?;
                let resp = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .map_err(|e| ExtensionError::Other(e.to_string()))?
                    .get(&url)
                    .send()
                    .await
                    // Transport errors are infrastructure failures, not token issues
                    .map_err(|e| {
                        ExtensionError::Other(format!("Token validation request failed: {}", e))
                    })?;
                if !resp.status().is_success() {
                    return Err(ExtensionError::ValidationFailed(format!(
                        "Invalid token (API returned {})",
                        resp.status()
                    )));
                }
            }
        }

        // Validate and store each submitted secret
        for (secret_name, secret_value) in secrets {
            if !allowed_secrets.contains(secret_name.as_str()) {
                return Err(ExtensionError::Other(format!(
                    "Unknown secret '{}' for extension '{}'",
                    secret_name, name
                )));
            }
            let trimmed_value = secret_value.trim();
            if trimmed_value.is_empty() {
                continue;
            }
            let params =
                CreateSecretParams::new(secret_name, trimmed_value).with_provider(name.to_string());
            self.secrets
                .create(user_id, params)
                .await
                .map_err(|e| ExtensionError::AuthFailed(e.to_string()))?;
        }

        let mut restart_required = false;
        let mut stored_fields = self.load_tool_setup_fields(name).await.unwrap_or_default();

        for (field_name, field_value) in fields {
            if !allowed_fields.contains(field_name.as_str()) {
                return Err(ExtensionError::Other(format!(
                    "Unknown field '{}' for extension '{}'",
                    field_name, name
                )));
            }
            let trimmed = field_value.trim();
            if trimmed.is_empty() {
                continue;
            }

            stored_fields.insert(field_name.clone(), trimmed.to_string());

            if let Some(field_def) = setup_field_defs.get(field_name) {
                if field_def.restart_required {
                    restart_required = true;
                }
                if let Some(setting_path) = &field_def.setting_path {
                    Self::validate_setup_setting_path(name, setting_path)?;
                    let store = self.store.as_ref().ok_or_else(|| {
                        ExtensionError::Other(
                            "Settings store unavailable for setup field persistence".to_string(),
                        )
                    })?;
                    store
                        .set_setting(
                            &self.user_id,
                            setting_path,
                            &serde_json::Value::String(trimmed.to_string()),
                        )
                        .await
                        .map_err(|e| {
                            ExtensionError::Other(format!(
                                "Failed to set '{}' for extension '{}': {}",
                                setting_path, name, e
                            ))
                        })?;
                }
            }
        }

        if !allowed_fields.is_empty() && !fields.is_empty() {
            self.save_tool_setup_fields(name, &stored_fields).await?;
        }

        for field_def in setup_field_defs.values() {
            if field_def.optional {
                continue;
            }
            if !self
                .is_tool_setup_field_provided(name, field_def, &stored_fields)
                .await
            {
                return Err(ExtensionError::Other(format!(
                    "Required field '{}' is missing for extension '{}'",
                    field_def.name, name
                )));
            }
        }

        // Auto-generate any missing secrets (channel-only feature)
        if let Some(ref cap_file) = channel_cap_file {
            for secret_def in &cap_file.setup.required_secrets {
                if let Some(ref auto_gen) = secret_def.auto_generate {
                    let already_provided = secrets
                        .get(&secret_def.name)
                        .is_some_and(|v| !v.trim().is_empty());
                    let already_stored = self
                        .secrets
                        .exists(user_id, &secret_def.name)
                        .await
                        .unwrap_or(false);
                    if !already_provided && !already_stored {
                        use rand::RngCore;
                        use rand::rngs::OsRng;
                        let mut bytes = vec![0u8; auto_gen.length];
                        OsRng.fill_bytes(&mut bytes);
                        let hex_value: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                        let params = CreateSecretParams::new(&secret_def.name, &hex_value)
                            .with_provider(name.to_string());
                        self.secrets
                            .create(user_id, params)
                            .await
                            .map_err(|e| ExtensionError::AuthFailed(e.to_string()))?;
                        tracing::info!(
                            "Auto-generated secret '{}' for channel '{}'",
                            secret_def.name,
                            name
                        );
                    }
                }
            }
        }

        // For tools, save and attempt auto-activation, then check auth.
        if kind == ExtensionKind::WasmTool {
            match self.activate_wasm_tool(name, user_id).await {
                Ok(result) => {
                    // Delete existing OAuth token so auth() starts a fresh flow.
                    // Done AFTER activation succeeds to avoid losing tokens on failure.
                    // This covers Reconfigure: user wants to re-auth (switch account, update creds).
                    if let Some(cap) = self.load_tool_capabilities(name).await
                        && let Some(ref auth_cfg) = cap.auth
                        && auth_cfg.oauth.is_some()
                    {
                        let _ = self.secrets.delete(user_id, &auth_cfg.secret_name).await;
                        let _ = self
                            .secrets
                            .delete(user_id, &format!("{}_scopes", auth_cfg.secret_name))
                            .await;
                        let _ = self
                            .secrets
                            .delete(user_id, &format!("{}_refresh_token", auth_cfg.secret_name))
                            .await;
                    }

                    // Check if auth is needed (OAuth or manual token).
                    // This is safe to call here — cancel-and-retry prevents port conflicts.
                    let mut auth_url = None;
                    // Box::pin breaks the async recursion cycle:
                    // auth() → auth_wasm_tool() → (OAuth) → configure() → auth()
                    if let Ok(auth_result) = Box::pin(self.auth(name, user_id)).await {
                        auth_url = auth_result.auth_url().map(String::from);
                    }
                    let message = if auth_url.is_some() {
                        format!(
                            "Configuration saved and tool '{}' activated. Complete OAuth in your browser.",
                            name
                        )
                    } else {
                        format!(
                            "Configuration saved and tool '{}' activated. {}",
                            name, result.message
                        )
                    };
                    return Ok(ConfigureResult {
                        message,
                        activated: true,
                        restart_required,
                        auth_url,
                        verification: None,
                    });
                }
                Err(e) => {
                    tracing::debug!(
                        "Auto-activation of tool '{}' after setup failed: {}",
                        name,
                        e
                    );
                    return Ok(ConfigureResult {
                        message: format!("Configuration saved for '{}'.", name),
                        activated: false,
                        restart_required,
                        auth_url: None,
                        verification: None,
                    });
                }
            }
        }

        // Activate the extension now that secrets are saved.
        // Dispatch by kind — WasmTool was already handled above with an early return.
        let activate_result = match kind {
            ExtensionKind::WasmChannel => self.activate_wasm_channel(name, user_id).await,
            ExtensionKind::McpServer => self.activate_mcp(name, user_id).await,
            ExtensionKind::WasmTool => {
                return Ok(ConfigureResult {
                    message: format!("Configuration saved for '{}'.", name),
                    activated: false,
                    restart_required,
                    auth_url: None,
                    verification: None,
                });
            }
        };

        match activate_result {
            Ok(result) => {
                self.activation_errors.write().await.remove(name);
                self.broadcast_extension_status(user_id, name, "active", None)
                    .await;
                let message = format!(
                    "Configuration saved and '{}' activated. {}",
                    name, result.message
                );
                Ok(ConfigureResult {
                    message,
                    activated: true,
                    restart_required,
                    auth_url: None,
                    verification: None,
                })
            }
            Err(e) => {
                let error_msg = e.to_string();
                tracing::warn!(
                    extension = name,
                    error = %e,
                    "Saved configuration but activation failed"
                );
                self.activation_errors
                    .write()
                    .await
                    .insert(name.to_string(), error_msg.clone());
                self.broadcast_extension_status(user_id, name, "failed", Some(&error_msg))
                    .await;
                Ok(ConfigureResult {
                    message: format!(
                        "Configuration saved for '{}'. Activation failed: {}",
                        name, e
                    ),
                    activated: false,
                    restart_required,
                    auth_url: None,
                    verification: None,
                })
            }
        }
    }

    /// Convenience wrapper: configure a single token for an extension.
    ///
    /// Determines the primary secret name from the extension's capabilities,
    /// then delegates to [`configure()`]. Use this when the caller only has
    /// a bare token value (e.g., from the chat auth card or WebSocket auth).
    pub async fn configure_token(
        &self,
        name: &str,
        token: &str,
        user_id: &str,
    ) -> Result<ConfigureResult, ExtensionError> {
        let kind = self.determine_installed_kind(name, user_id).await?;
        let secret_name = match kind {
            ExtensionKind::WasmChannel => {
                let cap_path = self
                    .wasm_channels_dir
                    .join(format!("{}.capabilities.json", name));
                let cap_bytes = tokio::fs::read(&cap_path)
                    .await
                    .map_err(|e| ExtensionError::Other(e.to_string()))?;
                let cap_file =
                    crate::channels::wasm::ChannelCapabilitiesFile::from_bytes(&cap_bytes)
                        .map_err(|e| ExtensionError::Other(e.to_string()))?;
                // Pick the first *missing* non-optional secret so re-configure
                // of a second secret works for multi-secret channels.
                let mut target = None;
                for s in &cap_file.setup.required_secrets {
                    if s.optional {
                        continue;
                    }
                    if !self.secrets.exists(user_id, &s.name).await.unwrap_or(false) {
                        target = Some(s.name.clone());
                        break;
                    }
                }
                // Fall back to first non-optional if all exist (overwrite)
                target
                    .or_else(|| {
                        cap_file
                            .setup
                            .required_secrets
                            .iter()
                            .find(|s| !s.optional)
                            .map(|s| s.name.clone())
                    })
                    .ok_or_else(|| {
                        ExtensionError::Other(format!("Channel '{}' has no required secrets", name))
                    })?
            }
            ExtensionKind::WasmTool => {
                let cap = self.load_tool_capabilities(name).await.ok_or_else(|| {
                    ExtensionError::Other(format!("Capabilities not found for '{}'", name))
                })?;
                // Prefer auth secret, then first missing setup secret
                if let Some(ref auth) = cap.auth {
                    if !self
                        .secrets
                        .exists(user_id, &auth.secret_name)
                        .await
                        .unwrap_or(false)
                    {
                        auth.secret_name.clone()
                    } else if let Some(ref setup) = cap.setup {
                        // Auth secret exists, find first missing setup secret
                        let mut found = None;
                        for s in &setup.required_secrets {
                            if !self.secrets.exists(user_id, &s.name).await.unwrap_or(false) {
                                found = Some(s.name.clone());
                                break;
                            }
                        }
                        found.unwrap_or_else(|| auth.secret_name.clone())
                    } else {
                        auth.secret_name.clone()
                    }
                } else {
                    cap.setup
                        .as_ref()
                        .and_then(|s| s.required_secrets.first())
                        .map(|s| s.name.clone())
                        .ok_or_else(|| {
                            ExtensionError::Other(format!(
                                "Tool '{}' has no auth or setup secrets",
                                name
                            ))
                        })?
                }
            }
            ExtensionKind::McpServer => {
                let server = self
                    .get_mcp_server(name, user_id)
                    .await
                    .map_err(|e| ExtensionError::NotInstalled(e.to_string()))?;
                server.token_secret_name()
            }
        };

        let mut secrets = std::collections::HashMap::new();
        secrets.insert(secret_name, token.to_string());
        self.configure(name, &secrets, &std::collections::HashMap::new(), user_id)
            .await
    }

    /// Read a capabilities.json file and revoke its credential mappings from
    /// the shared credential registry, so removed extensions lose injection
    /// authority immediately.
    async fn revoke_credential_mappings(&self, cap_path: &std::path::Path) {
        if !cap_path.exists() {
            return;
        }
        let Ok(bytes) = tokio::fs::read(cap_path).await else {
            return;
        };
        // Extract secret names from the capabilities JSON.
        // Structure: { "http": { "credentials": { "<key>": { "secret_name": "..." } } } }
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return;
        };
        let secret_names: Vec<String> = json
            .get("http")
            .and_then(|h| h.get("credentials"))
            .and_then(|c| c.as_object())
            .map(|creds| {
                creds
                    .values()
                    .filter_map(|v| v.get("secret_name").and_then(|s| s.as_str()))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();

        if secret_names.is_empty() {
            return;
        }

        if let Some(cr) = self.tool_registry.credential_registry() {
            cr.remove_mappings_for_secrets(&secret_names);
            tracing::info!(
                secrets = ?secret_names,
                "Revoked credential mappings for removed extension"
            );
        }
    }

    async fn unregister_hook_prefix(&self, prefix: &str) -> usize {
        let Some(ref hooks) = self.hooks else {
            return 0;
        };

        let names = hooks.list().await;
        let mut removed = 0;
        for hook_name in names {
            if hook_name.starts_with(prefix) && hooks.unregister(&hook_name).await {
                removed += 1;
            }
        }
        removed
    }
}

/// Inject credentials for a channel based on naming convention.
///
/// Looks for secrets matching the pattern `{channel_name}_*` and injects them
/// as credential placeholders (e.g., `xmpp_password` -> `{XMPP_PASSWORD}`).
///
/// Falls back to environment variables starting with the uppercase channel name
/// prefix (e.g., `XMPP_` for channel `xmpp`) for missing credentials.
///
/// Returns the number of credentials injected.
async fn inject_channel_credentials_from_secrets(
    channel: &Arc<crate::channels::wasm::WasmChannel>,
    secrets: Option<&dyn SecretsStore>,
    channel_name: &str,
    user_id: &str,
) -> Result<usize, String> {
    let mut count = 0;
    let mut injected_placeholders = std::collections::HashSet::new();

    // 1. Try injecting from persistent secrets store if available
    if let Some(secrets) = secrets {
        let all_secrets = secrets
            .list(user_id)
            .await
            .map_err(|e| format!("Failed to list secrets: {}", e))?;

        let prefix = format!("{}_", channel_name.to_ascii_lowercase());

        for secret_meta in all_secrets {
            if !secret_meta.name.to_ascii_lowercase().starts_with(&prefix) {
                continue;
            }

            let decrypted = match secrets.get_decrypted(user_id, &secret_meta.name).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(
                        secret = %secret_meta.name,
                        error = %e,
                        "Failed to decrypt secret for channel credential injection"
                    );
                    continue;
                }
            };

            let placeholder = secret_meta.name.to_uppercase();
            channel
                .set_credential(&placeholder, decrypted.expose().to_string())
                .await;
            injected_placeholders.insert(placeholder);
            count += 1;
        }
    }

    // 2. Fallback to environment variables for missing credentials
    count += inject_env_credentials(channel, channel_name, &injected_placeholders).await;

    Ok(count)
}

async fn load_channel_config_secret_overrides(
    secrets: Option<&dyn SecretsStore>,
    channel_name: &str,
    user_id: &str,
) -> std::collections::HashMap<String, serde_json::Value> {
    let mut overrides = std::collections::HashMap::new();
    let Some(secrets) = secrets else {
        return overrides;
    };

    let secret_config_mappings: &[(&str, &str)] = match channel_name {
        "xmpp" => &[("xmpp_password", "xmpp_password")],
        _ => return overrides,
    };

    for &(config_key, secret_name) in secret_config_mappings {
        match secrets.get_decrypted(user_id, secret_name).await {
            Ok(secret) => {
                overrides.insert(
                    config_key.to_string(),
                    serde_json::Value::String(secret.expose().to_string()),
                );
            }
            Err(crate::secrets::SecretError::NotFound(_)) => {}
            Err(err) => {
                tracing::warn!(
                    channel = %channel_name,
                    secret = %secret_name,
                    error = %err,
                    "Failed to load channel config secret override"
                );
            }
        }
    }

    overrides
}

/// Inject missing credentials from environment variables.
///
/// Only environment variables starting with the uppercase channel name prefix
/// (e.g., `XMPP_` for channel `xmpp`) are considered for security.
async fn inject_env_credentials(
    channel: &Arc<crate::channels::wasm::WasmChannel>,
    channel_name: &str,
    already_injected: &std::collections::HashSet<String>,
) -> usize {
    if channel_name.trim().is_empty() {
        return 0;
    }

    let caps = channel.capabilities();
    let Some(ref http_cap) = caps.tool_capabilities.http else {
        return 0;
    };

    let placeholders: Vec<String> = http_cap
        .credentials
        .values()
        .map(|m| m.secret_name.to_uppercase())
        .collect();

    let resolved = resolve_env_credentials(&placeholders, channel_name, already_injected);
    let count = resolved.len();
    for (placeholder, value) in resolved {
        channel.set_credential(&placeholder, value).await;
    }
    count
}

/// Pure helper: from a list of credential placeholder names, return those that
/// pass the channel-prefix security check and have a non-empty env var value.
///
/// Placeholders already covered by the secrets store (`already_injected`) are
/// skipped. Only names starting with `{CHANNEL_NAME}_` are allowed to prevent
/// a WASM channel from reading unrelated host credentials (e.g. `AWS_SECRET_ACCESS_KEY`).
pub(crate) fn resolve_env_credentials(
    placeholders: &[String],
    channel_name: &str,
    already_injected: &std::collections::HashSet<String>,
) -> Vec<(String, String)> {
    if channel_name.trim().is_empty() {
        return Vec::new();
    }

    let prefix = format!("{}_", channel_name.to_ascii_uppercase());
    let mut out = Vec::new();

    for placeholder in placeholders {
        if already_injected.contains(placeholder) {
            continue;
        }
        if !placeholder.starts_with(&prefix) {
            tracing::warn!(
                channel = %channel_name,
                placeholder = %placeholder,
                "Ignoring non-prefixed credential placeholder in environment fallback"
            );
            continue;
        }
        if let Ok(value) = std::env::var(placeholder)
            && !value.is_empty()
        {
            out.push((placeholder.clone(), value));
        }
    }
    out
}

/// Infer the extension kind from a URL.
fn infer_kind_from_url(url: &str) -> ExtensionKind {
    if url.ends_with(".wasm") || url.ends_with(".tar.gz") {
        ExtensionKind::WasmTool
    } else {
        ExtensionKind::McpServer
    }
}

/// Decision from `fallback_decision`: should we try the fallback source or
/// return the primary result as-is?
enum FallbackDecision {
    /// Return the primary result directly (success or non-retriable error).
    Return,
    /// Primary failed with a retriable error and a fallback source is available.
    TryFallback,
}

/// Decide whether to attempt a fallback install based on the primary result
/// and the availability of a fallback source.
fn fallback_decision(
    primary_result: &Result<InstallResult, ExtensionError>,
    fallback_source: &Option<Box<ExtensionSource>>,
) -> FallbackDecision {
    match (primary_result, fallback_source) {
        // Success — no fallback needed
        (Ok(_), _) => FallbackDecision::Return,
        // AlreadyInstalled — don't try building from source
        (Err(ExtensionError::AlreadyInstalled(_)), _) => FallbackDecision::Return,
        // Failed with a fallback available — try it
        (Err(_), Some(_)) => FallbackDecision::TryFallback,
        // Failed with no fallback — return the error
        (Err(_), None) => FallbackDecision::Return,
    }
}

/// Combine primary and fallback errors into a single error.
///
/// Preserves `AlreadyInstalled` from the fallback directly; otherwise wraps
/// both errors into the structured `ExtensionError::FallbackFailed` variant.
fn combine_install_errors(
    primary_err: ExtensionError,
    fallback_err: ExtensionError,
) -> ExtensionError {
    if matches!(fallback_err, ExtensionError::AlreadyInstalled(_)) {
        return fallback_err;
    }
    ExtensionError::FallbackFailed {
        primary: Box::new(primary_err),
        fallback: Box::new(fallback_err),
    }
}

#[cfg(all(test, feature = "libsql"))]
mod tests {
    use std::collections::HashMap;
    use std::fmt::Debug;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;

    use crate::context::JobContext;
    use crate::extensions::ExtensionManager;
    use crate::extensions::manager::{
        ActiveMcpServer, FallbackDecision, McpRecoveryCandidate, McpRecoveryClaim,
        McpRuntimeHealth, build_wasm_channel_runtime_config_updates, combine_install_errors,
        fallback_decision, infer_kind_from_url, join_or_abort_tasks, normalize_hosted_callback_url,
    };
    use crate::extensions::{ExtensionError, ExtensionKind, ExtensionSource, InstallResult};
    use crate::tools::{Tool, ToolError, ToolOutput};

    struct UnhealthyMcpTransport;

    #[async_trait]
    impl crate::tools::mcp::McpTransport for UnhealthyMcpTransport {
        async fn send(
            &self,
            _request: &crate::tools::mcp::McpRequest,
            _headers: &HashMap<String, String>,
        ) -> Result<crate::tools::mcp::McpResponse, ToolError> {
            Err(ToolError::ExternalService("runtime trapped".to_string()))
        }

        async fn shutdown(&self) -> Result<(), ToolError> {
            Ok(())
        }

        fn is_healthy(&self) -> bool {
            false
        }
    }

    struct ShutdownTrackingMcpTransport(Arc<AtomicBool>);

    #[async_trait]
    impl crate::tools::mcp::McpTransport for ShutdownTrackingMcpTransport {
        async fn send(
            &self,
            _request: &crate::tools::mcp::McpRequest,
            _headers: &HashMap<String, String>,
        ) -> Result<crate::tools::mcp::McpResponse, ToolError> {
            Err(ToolError::ExternalService("not used".to_string()))
        }

        async fn shutdown(&self) -> Result<(), ToolError> {
            self.0.store(true, Ordering::Release);
            Ok(())
        }
    }

    struct ChangingToolsTransport {
        responses: std::sync::Mutex<std::collections::VecDeque<crate::tools::mcp::McpResponse>>,
        enabled: AtomicBool,
        generation: AtomicU64,
    }

    impl ChangingToolsTransport {
        fn new(responses: Vec<crate::tools::mcp::McpResponse>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into()),
                enabled: AtomicBool::new(false),
                generation: AtomicU64::new(0),
            }
        }

        fn notify(&self) {
            if self.enabled.load(Ordering::Acquire) {
                self.generation.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    #[async_trait]
    impl crate::tools::mcp::McpTransport for ChangingToolsTransport {
        async fn send(
            &self,
            _request: &crate::tools::mcp::McpRequest,
            _headers: &HashMap<String, String>,
        ) -> Result<crate::tools::mcp::McpResponse, ToolError> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ToolError::ExternalService("missing test response".to_string()))
        }

        async fn shutdown(&self) -> Result<(), ToolError> {
            Ok(())
        }

        fn set_tools_list_changed(&self, enabled: bool) {
            self.enabled.store(enabled, Ordering::Release);
        }

        fn tools_generation(&self) -> u64 {
            self.generation.load(Ordering::Acquire)
        }
    }

    struct NamedTestTool {
        name: String,
    }

    #[async_trait]
    impl Tool for NamedTestTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "MCP lifecycle test tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            params: serde_json::Value,
            _ctx: &JobContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::success(params, Duration::ZERO))
        }
    }

    #[cfg(unix)]
    fn write_mcp_lifecycle_fixture(dir: &tempfile::TempDir) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let script_path = dir.path().join("mcp-lifecycle-fixture.sh");
        std::fs::write(
            &script_path,
            r#"#!/bin/sh
if [ -n "$1" ]; then
  printf '%s\n' "$$" >> "$1"
  if [ -n "$MCP_TOKEN_OBSERVED_FILE" ]; then
    case "$MCP_ROTATING_TOKEN" in
      old-test-value) printf '%s\n' old >> "$MCP_TOKEN_OBSERVED_FILE" ;;
      new-test-value) printf '%s\n' new >> "$MCP_TOKEN_OBSERVED_FILE" ;;
      *) printf '%s\n' unexpected >> "$MCP_TOKEN_OBSERVED_FILE" ;;
    esac
  fi
  sleep 0.2
fi
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      if [ "$MCP_HANG_AFTER_CRASH" = "1" ] && [ -e "$MCP_CRASH_ONCE_FILE" ]; then
        sleep 60
      fi
      if [ -n "$MCP_FAIL_RESTART_ONCE_FILE" ] && [ -e "$MCP_CRASH_ONCE_FILE" ] && [ ! -e "$MCP_FAIL_RESTART_ONCE_FILE" ]; then
        : > "$MCP_FAIL_RESTART_ONCE_FILE"
        printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"fixture restart failure"}}'
        exit 18
      fi
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}'
      ;;
    *'"method":"tools/list"'*)
      if [ "$MCP_FAIL_LIST" = "1" ]; then
        printf '%s\n' '{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"fixture list failure"}}'
      else
        printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"ping","description":"Ping","inputSchema":{"type":"object"}}]}}'
        if [ "$MCP_CLOSE_STDOUT" = "1" ]; then
          exec 1>&-
          sleep 60
        elif [ "$MCP_CRASH_ALWAYS" = "1" ]; then
          exit 17
        elif [ -n "$MCP_CRASH_ONCE_FILE" ] && [ ! -e "$MCP_CRASH_ONCE_FILE" ]; then
          : > "$MCP_CRASH_ONCE_FILE"
          exit 17
        fi
      fi
      ;;
  esac
done
"#,
        )
        .expect("write MCP fixture");
        let mut permissions = std::fs::metadata(&script_path)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script_path, permissions).expect("make fixture executable");
        script_path
    }

    #[cfg(unix)]
    async fn wait_for_mcp_start_count(path: &std::path::Path, expected: usize) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let starts = std::fs::read_to_string(path).unwrap_or_default();
                if starts.lines().count() >= expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("MCP fixture did not reach expected start count");
    }

    #[tokio::test]
    async fn join_or_abort_tasks_waits_for_aborted_tasks() {
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_completed = Arc::clone(&completed);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut handles = vec![tokio::spawn(async move {
            struct CompletionGuard(Arc<std::sync::atomic::AtomicBool>);
            impl Drop for CompletionGuard {
                fn drop(&mut self) {
                    self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            let _guard = CompletionGuard(task_completed);
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        })];
        started_rx.await.expect("task started");

        assert!(join_or_abort_tasks(&mut handles, Duration::from_millis(10)).await);
        assert!(completed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(handles[0].is_finished());
    }

    #[cfg(unix)]
    async fn wait_for_mcp_exit(manager: &ExtensionManager, name: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    manager.mcp_process_manager.state(name).await,
                    Ok(crate::tools::mcp::McpProcessState::Exited(_))
                ) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("MCP fixture did not exit");
    }

    #[cfg(unix)]
    async fn wait_for_mcp_tool(manager: &ExtensionManager, name: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if manager.tool_registry.has(name).await {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("MCP fixture tool was not registered");
    }

    #[cfg(unix)]
    async fn wait_for_mcp_health(
        manager: &ExtensionManager,
        name: &str,
        expected: McpRuntimeHealth,
    ) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let health = manager
                    .active_mcp_servers
                    .read()
                    .await
                    .get(name)
                    .map(|runtime| runtime.health);
                if health == Some(expected) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("MCP runtime did not reach expected health");
    }

    fn require_eq<T>(actual: T, expected: T, label: &str) -> Result<(), String>
    where
        T: PartialEq + Debug,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!(
                "{label} mismatch: expected {:?}, got {:?}",
                expected, actual
            ))
        }
    }

    #[test]
    fn test_infer_kind_from_url() {
        assert_eq!(
            infer_kind_from_url("https://example.com/tool.wasm"),
            ExtensionKind::WasmTool
        );
        assert_eq!(
            infer_kind_from_url("https://example.com/tool-wasm32-wasip2.tar.gz"),
            ExtensionKind::WasmTool
        );
        assert_eq!(
            infer_kind_from_url("https://mcp.notion.com"),
            ExtensionKind::McpServer
        );
        assert_eq!(
            infer_kind_from_url("https://example.com/mcp"),
            ExtensionKind::McpServer
        );
    }

    // ---- fallback install logic tests ----

    fn make_ok_result() -> Result<InstallResult, ExtensionError> {
        Ok(InstallResult {
            name: "test".to_string(),
            kind: ExtensionKind::WasmTool,
            message: "Installed".to_string(),
        })
    }

    fn make_fallback_source() -> Option<Box<ExtensionSource>> {
        Some(Box::new(ExtensionSource::WasmBuildable {
            source_dir: "tools-src/test".to_string(),
            build_dir: Some("tools-src/test".to_string()),
            crate_name: Some("test-tool".to_string()),
        }))
    }

    #[test]
    fn test_fallback_decision_success_returns_directly() {
        let result = make_ok_result();
        let fallback = make_fallback_source();
        assert!(matches!(
            fallback_decision(&result, &fallback),
            FallbackDecision::Return
        ));
    }

    #[test]
    fn test_fallback_decision_already_installed_skips_fallback() {
        let result: Result<InstallResult, ExtensionError> =
            Err(ExtensionError::AlreadyInstalled("test".to_string()));
        let fallback = make_fallback_source();
        assert!(matches!(
            fallback_decision(&result, &fallback),
            FallbackDecision::Return
        ));
    }

    #[test]
    fn test_fallback_decision_download_failed_triggers_fallback() {
        let result: Result<InstallResult, ExtensionError> =
            Err(ExtensionError::DownloadFailed("404 Not Found".to_string()));
        let fallback = make_fallback_source();
        assert!(matches!(
            fallback_decision(&result, &fallback),
            FallbackDecision::TryFallback
        ));
    }

    #[test]
    fn test_fallback_decision_error_without_fallback_returns() {
        let result: Result<InstallResult, ExtensionError> =
            Err(ExtensionError::DownloadFailed("404 Not Found".to_string()));
        let fallback = None;
        assert!(matches!(
            fallback_decision(&result, &fallback),
            FallbackDecision::Return
        ));
    }

    #[test]
    fn test_combine_errors_includes_both_messages() {
        let primary = ExtensionError::DownloadFailed("404 Not Found".to_string());
        let fallback = ExtensionError::InstallFailed("cargo not found".to_string());
        let combined = combine_install_errors(primary, fallback);
        assert!(
            matches!(combined, ExtensionError::FallbackFailed { .. }),
            "Expected FallbackFailed, got: {combined:?}"
        );
        let msg = combined.to_string();
        assert!(msg.contains("404 Not Found"), "missing primary: {msg}");
        assert!(msg.contains("cargo not found"), "missing fallback: {msg}");
    }

    #[test]
    fn test_combine_errors_forwards_already_installed_from_fallback() {
        let primary = ExtensionError::DownloadFailed("404".to_string());
        let fallback = ExtensionError::AlreadyInstalled("test".to_string());
        let combined = combine_install_errors(primary, fallback);
        assert!(
            matches!(combined, ExtensionError::AlreadyInstalled(ref name) if name == "test"),
            "Expected AlreadyInstalled, got: {combined:?}"
        );
    }

    // === QA Plan P2 - 2.4: Extension registry collision tests (filesystem) ===

    #[test]
    fn test_tool_and_channel_paths_are_separate() {
        let dir = tempfile::tempdir().expect("temp dir");
        let tools_dir = dir.path().join("tools");
        let channels_dir = dir.path().join("channels");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::create_dir_all(&channels_dir).unwrap();

        let name = "shared";
        let tool_wasm = tools_dir.join(format!("{}.wasm", name));
        let channel_wasm = channels_dir.join(format!("{}.wasm", name));

        // Simulate installing both.
        std::fs::write(&tool_wasm, b"tool-payload").unwrap();
        std::fs::write(&channel_wasm, b"channel-payload").unwrap();

        // Both files exist and contain distinct content.
        assert!(tool_wasm.exists());
        assert!(channel_wasm.exists());
        assert_ne!(
            std::fs::read(&tool_wasm).unwrap(),
            std::fs::read(&channel_wasm).unwrap(),
            "Tool and channel files must be independent"
        );

        // Removing one doesn't affect the other.
        std::fs::remove_file(&tool_wasm).unwrap();
        assert!(!tool_wasm.exists());
        assert!(
            channel_wasm.exists(),
            "Removing tool must not affect channel"
        );
    }

    #[test]
    fn test_determine_kind_priority_tools_before_channels() {
        // When a name exists in both tools and channels dirs,
        // determine_installed_kind checks tools first (wasm_tools_dir).
        // This test documents the priority order.
        let dir = tempfile::tempdir().expect("temp dir");
        let tools_dir = dir.path().join("tools");
        let channels_dir = dir.path().join("channels");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::create_dir_all(&channels_dir).unwrap();

        let name = "ambiguous";
        let tool_wasm = tools_dir.join(format!("{}.wasm", name));
        let channel_wasm = channels_dir.join(format!("{}.wasm", name));

        // Only channel exists → channel kind.
        std::fs::write(&channel_wasm, b"channel").unwrap();
        assert!(!tool_wasm.exists());
        assert!(channel_wasm.exists());

        // Both exist → tools dir checked first.
        std::fs::write(&tool_wasm, b"tool").unwrap();
        assert!(tool_wasm.exists());
        assert!(channel_wasm.exists());
        // This documents the determine_installed_kind priority:
        // tools are checked before channels.

        // Only tool exists → tool kind.
        std::fs::remove_file(&channel_wasm).unwrap();
        assert!(tool_wasm.exists());
        assert!(!channel_wasm.exists());
    }

    // === WASM runtime availability tests ===
    //
    // Regression tests for a bug where the WASM runtime was only created at
    // startup when the tools directory already existed. Extensions installed
    // after startup (e.g. via the web UI) would fail with "WASM runtime not
    // available" because the ExtensionManager had `wasm_tool_runtime: None`.

    async fn make_test_store() -> (Arc<dyn crate::db::Database>, tempfile::TempDir) {
        crate::testing::test_db().await
    }

    /// Build a minimal ExtensionManager suitable for unit tests.
    fn make_test_manager_with_dirs(
        wasm_runtime: Option<Arc<crate::tools::wasm::WasmToolRuntime>>,
        tools_dir: std::path::PathBuf,
        channels_dir: std::path::PathBuf,
        store: Option<Arc<dyn crate::db::Database>>,
    ) -> crate::extensions::manager::ExtensionManager {
        use crate::secrets::{InMemorySecretsStore, SecretsCrypto};
        use crate::tools::mcp::process::McpProcessManager;
        use crate::tools::mcp::session::McpSessionManager;

        std::fs::create_dir_all(&tools_dir).ok();
        std::fs::create_dir_all(&channels_dir).ok();

        let key = secrecy::SecretString::from(crate::secrets::keychain::generate_master_key_hex());
        let crypto = Arc::new(SecretsCrypto::new(key).expect("crypto"));
        let secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(InMemorySecretsStore::new(crypto));
        let tools = Arc::new(crate::tools::ToolRegistry::new());
        let mcp = Arc::new(McpSessionManager::new());

        crate::extensions::manager::ExtensionManager::new(
            mcp,
            Arc::new(McpProcessManager::new()),
            secrets,
            tools,
            None, // hooks
            wasm_runtime,
            tools_dir,
            channels_dir,
            None,               // tunnel_url
            "test".to_string(), // user_id
            store,
            vec![],
        )
    }

    fn make_test_manager(
        wasm_runtime: Option<Arc<crate::tools::wasm::WasmToolRuntime>>,
        tools_dir: std::path::PathBuf,
    ) -> crate::extensions::manager::ExtensionManager {
        make_test_manager_with_dirs(wasm_runtime, tools_dir.clone(), tools_dir, None)
    }

    fn materialize_mcp_wasm_fixture(dir: &std::path::Path) -> std::path::PathBuf {
        let component = crate::tools::mcp::wasm_transport::render_stateful_mcp_fixture();
        let path = dir.join("stateful-server.wasm");
        std::fs::write(&path, component).expect("materialize MCP component fixture");
        path
    }

    #[tokio::test]
    async fn trapped_wasm_mcp_runtime_is_claimed_for_recovery() {
        let dir = tempfile::tempdir().expect("temp dir");
        let manager = make_test_manager(None, dir.path().to_path_buf());
        let config = crate::tools::mcp::McpServerConfig::new_wasm(
            "portable",
            "/opt/lunarwing/portable.wasm",
        );
        let client = Arc::new(crate::tools::mcp::McpClient::new_with_transport(
            "portable",
            Arc::new(UnhealthyMcpTransport),
            None,
            None,
            "test",
            Some(config),
        ));
        let generation = manager.next_mcp_runtime_generation();
        manager.active_mcp_servers.write().await.insert(
            "portable".to_string(),
            ActiveMcpServer {
                owner_id: "test".to_string(),
                client,
                tools: vec![],
                tools_generation: 0,
                managed_stdio: false,
                managed_wasm: true,
                generation,
                health: McpRuntimeHealth::Healthy,
                restart_attempts: 0,
                healthy_since: Instant::now(),
                retry_after: None,
            },
        );
        let claim = manager
            .claim_mcp_recovery(&McpRecoveryCandidate {
                name: "portable".to_string(),
                owner_id: "test".to_string(),
                generation,
            })
            .await;

        assert!(matches!(claim, McpRecoveryClaim::Recover(_)));
    }

    #[tokio::test]
    async fn trapped_real_wasm_mcp_runtime_is_replaced_by_manager() {
        let (store, dir) = make_test_store().await;
        let component = materialize_mcp_wasm_fixture(dir.path());
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        );
        let config = crate::tools::mcp::McpServerConfig::new_wasm("fixture", component)
            .with_wasm_limits(64, crate::tools::wasm::DEFAULT_FUEL_LIMIT, 1);
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install real WASM MCP fixture");
        let activated = manager
            .activate("fixture", "test")
            .await
            .expect("activate real WASM MCP fixture");
        assert_eq!(activated.tools_loaded, ["fixture_alpha", "fixture_beta"]);
        let original_wrapper = manager
            .tool_registry
            .get("fixture_alpha")
            .await
            .expect("original registry wrapper");

        let (generation, client) = {
            let runtimes = manager.active_mcp_servers.read().await;
            let runtime = &runtimes["fixture"];
            (runtime.generation, Arc::clone(&runtime.client))
        };
        client
            .call_tool("alpha", serde_json::json!({"value": 1}))
            .await
            .expect("persistent actor handles first tool call");
        client
            .call_tool("alpha", serde_json::json!({"value": 2}))
            .await
            .expect_err("fixture trap must stop actor");
        assert!(!client.runtime_is_healthy());

        manager
            .recover_mcp_runtime_if_needed(
                McpRecoveryCandidate {
                    name: "fixture".to_string(),
                    owner_id: "test".to_string(),
                    generation,
                },
                CancellationToken::new(),
            )
            .await;

        let replacement = {
            let runtimes = manager.active_mcp_servers.read().await;
            let runtime = &runtimes["fixture"];
            assert_ne!(runtime.generation, generation);
            assert_eq!(runtime.health, McpRuntimeHealth::Healthy);
            assert!(runtime.client.runtime_is_healthy());
            Arc::clone(&runtime.client)
        };
        let replacement_wrapper = manager
            .tool_registry
            .get("fixture_alpha")
            .await
            .expect("replacement registry wrapper");
        assert!(!Arc::ptr_eq(&original_wrapper, &replacement_wrapper));
        assert!(manager.tool_registry.get("fixture_beta").await.is_some());
        assert!(replacement.runtime_is_healthy());
        replacement_wrapper
            .execute(
                serde_json::json!({"value": 1}),
                &crate::context::JobContext::with_user("test", "test", "test"),
            )
            .await
            .expect("registry-resolved replacement wrapper executes on fresh actor");
    }

    #[tokio::test]
    async fn shutdown_closes_and_removes_active_wasm_mcp_runtime() {
        let dir = tempfile::tempdir().expect("temp dir");
        let manager = make_test_manager(None, dir.path().to_path_buf());
        let stopped = Arc::new(AtomicBool::new(false));
        let config = crate::tools::mcp::McpServerConfig::new_wasm(
            "portable",
            "/opt/lunarwing/portable.wasm",
        );
        let client = Arc::new(crate::tools::mcp::McpClient::new_with_transport(
            "portable",
            Arc::new(ShutdownTrackingMcpTransport(Arc::clone(&stopped))),
            None,
            None,
            "test",
            Some(config),
        ));
        manager.active_mcp_servers.write().await.insert(
            "portable".to_string(),
            ActiveMcpServer {
                owner_id: "test".to_string(),
                client,
                tools: vec![],
                tools_generation: 0,
                managed_stdio: false,
                managed_wasm: true,
                generation: manager.next_mcp_runtime_generation(),
                health: McpRuntimeHealth::Healthy,
                restart_attempts: 0,
                healthy_since: Instant::now(),
                retry_after: None,
            },
        );

        manager.shutdown_mcp_runtimes().await;

        assert!(stopped.load(Ordering::Acquire));
        assert!(manager.active_mcp_servers.read().await.is_empty());
    }

    fn changing_tools_response(result: serde_json::Value) -> crate::tools::mcp::McpResponse {
        crate::tools::mcp::McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            result: Some(result),
            error: None,
        }
    }

    #[tokio::test]
    async fn host_list_change_swaps_exact_wrappers_and_retains_them_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let manager = make_test_manager(None, dir.path().to_path_buf());
        let transport = Arc::new(ChangingToolsTransport::new(vec![
            changing_tools_response(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": "changing", "version": "1"}
            })),
            crate::tools::mcp::McpResponse {
                jsonrpc: "2.0".to_string(),
                id: None,
                result: None,
                error: None,
            },
            changing_tools_response(
                serde_json::json!({"tools": [{"name": "old", "inputSchema": {}}]}),
            ),
            changing_tools_response(
                serde_json::json!({"tools": [{"name": "new", "inputSchema": {}}]}),
            ),
            changing_tools_response(
                serde_json::json!({"tools": [{"name": "conflict", "inputSchema": {}}]}),
            ),
        ]));
        let client = Arc::new(crate::tools::mcp::McpClient::new_with_transport(
            "changing",
            transport.clone(),
            None,
            None,
            "test",
            None,
        ));
        let old_tools = client.create_tools().await.unwrap();
        manager
            .tool_registry
            .replace_runtime_tools(&[], old_tools.clone())
            .await
            .unwrap();
        let generation = manager.next_mcp_runtime_generation();
        manager.active_mcp_servers.write().await.insert(
            "changing".to_string(),
            ActiveMcpServer {
                owner_id: "test".to_string(),
                client,
                tools: old_tools.clone(),
                tools_generation: 0,
                managed_stdio: true,
                managed_wasm: false,
                generation,
                health: McpRuntimeHealth::Healthy,
                restart_attempts: 0,
                healthy_since: Instant::now(),
                retry_after: None,
            },
        );
        let candidate = McpRecoveryCandidate {
            name: "changing".to_string(),
            owner_id: "test".to_string(),
            generation,
        };

        transport.notify();
        manager
            .refresh_mcp_tools_if_needed(&candidate, &CancellationToken::new())
            .await;
        assert!(manager.tool_registry.get("changing_old").await.is_none());
        assert!(manager.tool_registry.get("changing_new").await.is_some());

        let conflict: Arc<dyn Tool> = Arc::new(NamedTestTool {
            name: "changing_conflict".to_string(),
        });
        manager.tool_registry.register(Arc::clone(&conflict)).await;
        transport.notify();
        manager
            .refresh_mcp_tools_if_needed(&candidate, &CancellationToken::new())
            .await;
        assert!(manager.tool_registry.get("changing_new").await.is_some());
        assert!(
            manager
                .tool_registry
                .get("changing_conflict")
                .await
                .is_some_and(|tool| Arc::ptr_eq(&tool, &conflict))
        );
        assert_eq!(
            manager.active_mcp_servers.read().await["changing"].tools[0].name(),
            "changing_new"
        );
    }

    fn write_test_tool(
        dir: &std::path::Path,
        name: &str,
        capabilities_json: &str,
    ) -> std::path::PathBuf {
        let tools_dir = dir.join("tools");
        std::fs::create_dir_all(&tools_dir).expect("tools dir");
        std::fs::write(tools_dir.join(format!("{name}.wasm")), b"not-a-real-wasm").expect("wasm");
        std::fs::write(
            tools_dir.join(format!("{name}.capabilities.json")),
            capabilities_json,
        )
        .expect("capabilities");
        tools_dir
    }

    #[tokio::test]
    async fn test_install_stdio_mcp_config_persists_and_needs_no_auth() {
        use crate::extensions::{AuthStatus, ExtensionError};
        use crate::tools::mcp::config::{EffectiveTransport, McpServerConfig};

        let (store, dir) = make_test_store().await;
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        );
        let config = McpServerConfig::new_stdio(
            "filesystem",
            "npx",
            vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
            ],
            std::collections::HashMap::new(),
        );

        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install stdio MCP");

        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("load MCP config");
        let server = stored.get("filesystem").expect("stored server");
        assert!(matches!(
            server.effective_transport(),
            EffectiveTransport::Stdio { command, args, .. }
                if command == "npx"
                    && args == ["-y", "@modelcontextprotocol/server-filesystem"]
        ));

        let auth = manager
            .auth("filesystem", "test")
            .await
            .expect("auth status");
        assert!(matches!(auth.status, AuthStatus::NoAuthRequired));

        let installed = manager
            .list(
                Some(crate::extensions::ExtensionKind::McpServer),
                false,
                "test",
            )
            .await
            .expect("list installed MCP servers");
        let listed = installed
            .iter()
            .find(|extension| extension.name == "filesystem")
            .expect("listed stdio MCP server");
        assert_eq!(listed.transport.as_deref(), Some("stdio"));
        assert_eq!(listed.command.as_deref(), Some("npx"));
        assert!(listed.authenticated);
        assert!(!listed.active);

        let duplicate = manager
            .install_mcp_config(
                McpServerConfig::new_stdio(
                    "filesystem",
                    "node",
                    vec!["server.js".to_string()],
                    std::collections::HashMap::new(),
                ),
                "test",
            )
            .await
            .expect_err("duplicate install must fail");
        assert!(
            matches!(duplicate, ExtensionError::AlreadyInstalled(name) if name == "filesystem")
        );

        manager
            .mcp_process_manager
            .spawn_stdio(
                "filesystem",
                "cat",
                Vec::<String>::new(),
                std::collections::HashMap::<String, String>::new(),
                false,
                None,
            )
            .await
            .expect("spawn managed test process");
        assert_eq!(
            manager.mcp_process_manager.managed_servers().await,
            ["filesystem"]
        );

        manager
            .remove("filesystem", "test")
            .await
            .expect("remove stdio MCP server");
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );
        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("reload MCP config");
        assert!(stored.get("filesystem").is_none());
    }

    #[tokio::test]
    async fn job_worker_install_list_activate_and_toggle_never_start_host_runtime() {
        use crate::extensions::ExtensionError;
        use crate::tools::mcp::config::{McpPlacement, McpServerConfig};

        let (store, dir) = make_test_store().await;
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        );
        let mut config =
            McpServerConfig::new_stdio("worker-files", "worker-mcp", Vec::new(), HashMap::new())
                .with_placement(McpPlacement::JobWorker);
        config.enabled = false;

        let installed = manager
            .install_mcp_config(config, "test")
            .await
            .expect("install worker MCP");
        assert!(installed.message.contains("job_worker placement"));
        assert!(installed.message.contains("worker image"));

        let listed = manager
            .list(Some(ExtensionKind::McpServer), false, "test")
            .await
            .expect("list worker MCP");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].placement, Some(McpPlacement::JobWorker));
        assert_eq!(listed[0].enabled, Some(false));
        assert!(!listed[0].active);
        assert!(!listed[0].recovering);
        assert!(listed[0].tools.is_empty());

        let auth = manager
            .auth("worker-files", "test")
            .await
            .expect_err("worker MCP must not start host OAuth");
        assert!(matches!(auth, ExtensionError::AuthNotSupported(_)));

        let activated = manager
            .activate("worker-files", "test")
            .await
            .expect("enable worker MCP");
        assert!(activated.tools_loaded.is_empty());
        assert!(activated.message.contains("future Docker jobs"));
        assert!(manager.active_mcp_servers.read().await.is_empty());
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );

        let info = manager
            .extension_info("worker-files", "test")
            .await
            .expect("worker MCP info");
        assert_eq!(info["placement"], "job_worker");
        assert_eq!(info["worker_eligible"], true);
        assert_eq!(info["connected"], false);

        let disabled = manager
            .toggle_mcp("worker-files", "test")
            .await
            .expect("disable future worker jobs");
        assert!(disabled.contains("Disabled job-worker"));
        let enabled = manager
            .toggle_mcp("worker-files", "test")
            .await
            .expect("enable future worker jobs");
        assert!(enabled.contains("Enabled job-worker"));
        assert!(enabled.contains("immutable MCP snapshots"));
        assert!(manager.active_mcp_servers.read().await.is_empty());
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn startup_injection_rejects_worker_config_and_unregisters_tools() {
        use crate::tools::mcp::config::{McpPlacement, McpServerConfig};

        let (store, dir) = make_test_store().await;
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        );
        let config =
            McpServerConfig::new_stdio("worker-injected", "worker-mcp", Vec::new(), HashMap::new())
                .with_placement(McpPlacement::JobWorker);
        manager
            .install_mcp_config(config.clone(), "test")
            .await
            .expect("install worker MCP");

        let tool: Arc<dyn Tool> = Arc::new(NamedTestTool {
            name: "worker_injected_ping".to_string(),
        });
        manager.tool_registry.register(Arc::clone(&tool)).await;
        let client = Arc::new(crate::tools::mcp::McpClient::new_with_transport(
            "worker-injected",
            Arc::new(UnhealthyMcpTransport),
            None,
            None,
            "test",
            Some(config),
        ));

        manager
            .inject_mcp_client("worker-injected".to_string(), client, vec![tool])
            .await;

        assert!(manager.active_mcp_servers.read().await.is_empty());
        assert!(!manager.tool_registry.has("worker_injected_ping").await);
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn host_to_worker_update_unloads_host_runtime_without_reactivation() {
        use crate::tools::mcp::config::{McpPlacement, McpServerConfig};

        let (store, dir) = make_test_store().await;
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        );
        let host = McpServerConfig::new_stdio("moving", "host-mcp", Vec::new(), HashMap::new());
        manager
            .install_mcp_config(host.clone(), "test")
            .await
            .expect("install host MCP");

        let tool: Arc<dyn Tool> = Arc::new(NamedTestTool {
            name: "moving_ping".to_string(),
        });
        manager.tool_registry.register(Arc::clone(&tool)).await;
        let client = Arc::new(crate::tools::mcp::McpClient::new_with_transport(
            "moving",
            Arc::new(UnhealthyMcpTransport),
            None,
            None,
            "test",
            Some(host),
        ));
        manager.active_mcp_servers.write().await.insert(
            "moving".to_string(),
            ActiveMcpServer {
                owner_id: "test".to_string(),
                client,
                tools: vec![tool],
                tools_generation: 0,
                managed_stdio: false,
                managed_wasm: false,
                generation: manager.next_mcp_runtime_generation(),
                health: McpRuntimeHealth::Healthy,
                restart_attempts: 0,
                healthy_since: Instant::now(),
                retry_after: None,
            },
        );

        let worker = McpServerConfig::new_stdio("moving", "worker-mcp", Vec::new(), HashMap::new())
            .with_placement(McpPlacement::JobWorker);
        let message = manager
            .update_mcp("moving", worker, "test")
            .await
            .expect("move MCP to worker");

        assert!(message.contains("job_worker placement"));
        assert!(manager.active_mcp_servers.read().await.is_empty());
        assert!(!manager.tool_registry.has("moving_ping").await);
        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("load moved config");
        assert_eq!(
            stored.get("moving").expect("moved config").placement,
            McpPlacement::JobWorker
        );
    }

    #[tokio::test]
    async fn recovery_reread_rejects_worker_placement_and_removes_stale_runtime() {
        use crate::tools::mcp::config::{McpPlacement, McpServerConfig};

        let (store, dir) = make_test_store().await;
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        );
        let config =
            McpServerConfig::new_stdio("worker-recovery", "worker-mcp", Vec::new(), HashMap::new())
                .with_placement(McpPlacement::JobWorker);
        manager
            .install_mcp_config(config.clone(), "test")
            .await
            .expect("install worker MCP");
        let generation = manager.next_mcp_runtime_generation();
        let client = Arc::new(crate::tools::mcp::McpClient::new_with_transport(
            "worker-recovery",
            Arc::new(UnhealthyMcpTransport),
            None,
            None,
            "test",
            Some(config),
        ));
        manager.active_mcp_servers.write().await.insert(
            "worker-recovery".to_string(),
            ActiveMcpServer {
                owner_id: "test".to_string(),
                client,
                tools: Vec::new(),
                tools_generation: 0,
                managed_stdio: false,
                managed_wasm: true,
                generation,
                health: McpRuntimeHealth::Healthy,
                restart_attempts: 0,
                healthy_since: Instant::now(),
                retry_after: None,
            },
        );

        manager
            .recover_mcp_runtime_if_needed(
                McpRecoveryCandidate {
                    name: "worker-recovery".to_string(),
                    owner_id: "test".to_string(),
                    generation,
                },
                CancellationToken::new(),
            )
            .await;

        assert!(manager.active_mcp_servers.read().await.is_empty());
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn update_mcp_preserves_approval_policy_and_secret_references() {
        use crate::tools::mcp::config::{EffectiveTransport, McpServerConfig};

        let (store, dir) = make_test_store().await;
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        );
        let original = McpServerConfig::new_stdio_with_secrets(
            "secure-server",
            "old-command",
            vec![],
            std::collections::HashMap::new(),
            std::collections::HashMap::from([("API_TOKEN".to_string(), "api_token".to_string())]),
        )
        .with_always_require_approval(["write_data"]);
        manager
            .install_mcp_config(original, "test")
            .await
            .expect("install MCP config");

        let replacement = McpServerConfig::new_stdio_with_secrets(
            "secure-server",
            "new-command",
            vec![],
            std::collections::HashMap::new(),
            std::collections::HashMap::from([("NEW_TOKEN".to_string(), "new_token".to_string())]),
        );
        manager
            .update_mcp("secure-server", replacement, "test")
            .await
            .expect("update MCP config");

        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("load MCP config");
        let updated = stored.get("secure-server").expect("updated server");
        assert_eq!(
            updated.always_require_approval,
            vec!["write_data".to_string()]
        );
        assert!(matches!(
            updated.effective_transport(),
            EffectiveTransport::Stdio {
                command,
                env_secrets,
                ..
            } if command == "new-command"
                && env_secrets.get("NEW_TOKEN").map(String::as_str) == Some("new_token")
        ));
    }

    #[tokio::test]
    async fn test_concurrent_mcp_config_transitions_preserve_both_updates() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let first_manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        ));
        let second_manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools-2"),
            dir.path().join("channels-2"),
            Some(Arc::clone(&store)),
        ));
        for name in ["first", "second"] {
            first_manager
                .install_mcp_config(
                    McpServerConfig::new(name, format!("https://{name}.example/mcp")),
                    "test",
                )
                .await
                .expect("install MCP config");
        }

        let (first, second) = tokio::join!(
            first_manager.deactivate("first", "test"),
            second_manager.deactivate("second", "test")
        );
        first.expect("deactivate first");
        second.expect("deactivate second");

        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("reload MCP configs");
        assert!(!stored.get("first").expect("first config").enabled);
        assert!(!stored.get("second").expect("second config").enabled);
    }

    #[tokio::test]
    async fn test_mcp_lifecycle_sse_is_owner_scoped() {
        use futures::StreamExt;

        let (store, dir) = make_test_store().await;
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        );
        let sse = Arc::new(crate::channels::web::sse::SseManager::new());
        let mut alice = sse
            .subscribe_raw(Some("alice".to_string()))
            .expect("subscribe alice");
        let mut bob = sse
            .subscribe_raw(Some("bob".to_string()))
            .expect("subscribe bob");
        manager.set_sse_sender(sse).await;

        manager
            .broadcast_extension_status("alice", "files", "inactive", None)
            .await;

        assert!(matches!(
            alice.next().await,
            Some(crate::channels::web::types::SseEvent::ExtensionStatus { .. })
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), bob.next())
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_deactivate_and_reenable_manages_runtime_state() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        );
        let mut config = McpServerConfig::new_stdio(
            "fixture",
            script_path.to_string_lossy(),
            Vec::new(),
            HashMap::new(),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install disabled fixture");

        let first = manager
            .activate("fixture", "test")
            .await
            .expect("activate fixture");
        assert_eq!(first.tools_loaded, ["fixture_ping"]);
        assert!(manager.tool_registry.has("fixture_ping").await);
        assert_eq!(
            manager.mcp_process_manager.managed_servers().await,
            ["fixture"]
        );

        let unrelated: Arc<dyn Tool> = Arc::new(NamedTestTool {
            name: "fixture_extra_ping".to_string(),
        });
        manager.tool_registry.register(unrelated).await;
        manager
            .deactivate("fixture", "test")
            .await
            .expect("deactivate fixture");

        assert!(!manager.tool_registry.has("fixture_ping").await);
        assert!(manager.tool_registry.has("fixture_extra_ping").await);
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );
        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("reload disabled fixture");
        assert!(!stored.get("fixture").expect("stored fixture").enabled);
        let listed = manager
            .list(Some(ExtensionKind::McpServer), false, "test")
            .await
            .expect("list disabled fixture");
        assert_eq!(listed[0].enabled, Some(false));
        assert!(!listed[0].active);
        manager
            .deactivate("fixture", "test")
            .await
            .expect("deactivation is idempotent");
        assert!(
            manager
                .activate_after_auth("fixture", "test")
                .await
                .expect("post-auth activation check")
                .is_none()
        );
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );

        let second = manager
            .activate("fixture", "test")
            .await
            .expect("re-enable fixture");
        assert_eq!(second.tools_loaded, ["fixture_ping"]);
        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("reload enabled fixture");
        assert!(stored.get("fixture").expect("stored fixture").enabled);

        let replacement: Arc<dyn Tool> = Arc::new(NamedTestTool {
            name: "fixture_ping".to_string(),
        });
        manager
            .tool_registry
            .register(Arc::clone(&replacement))
            .await;
        manager
            .deactivate("fixture", "test")
            .await
            .expect("deactivate fixture with replacement");
        let registered = manager
            .tool_registry
            .get("fixture_ping")
            .await
            .expect("replacement remains registered");
        assert!(Arc::ptr_eq(&registered, &replacement));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_crash_rebuilds_client_and_tools() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let starts_path = dir.path().join("mcp-recovery-starts.log");
        let crash_path = dir.path().join("mcp-crash-once");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        ));
        let mut config = McpServerConfig::new_stdio(
            "fixture-recovery",
            script_path.to_string_lossy(),
            vec![starts_path.to_string_lossy().into_owned()],
            HashMap::from([(
                "MCP_CRASH_ONCE_FILE".to_string(),
                crash_path.to_string_lossy().into_owned(),
            )]),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install recovery fixture");
        manager
            .activate("fixture-recovery", "test")
            .await
            .expect("activate recovery fixture");
        let old_tool = manager
            .tool_registry
            .get("fixture-recovery_ping")
            .await
            .expect("initial MCP tool");
        wait_for_mcp_exit(&manager, "fixture-recovery").await;

        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_start_count(&starts_path, 2).await;
        wait_for_mcp_tool(&manager, "fixture-recovery_ping").await;

        let new_tool = manager
            .tool_registry
            .get("fixture-recovery_ping")
            .await
            .expect("recovered MCP tool");
        assert!(!Arc::ptr_eq(&old_tool, &new_tool));
        assert!(matches!(
            manager
                .mcp_process_manager
                .state("fixture-recovery")
                .await
                .expect("recovered process state"),
            crate::tools::mcp::McpProcessState::Running
        ));
        let listed = manager
            .list(Some(ExtensionKind::McpServer), false, "test")
            .await
            .expect("list recovered MCP server");
        assert!(listed[0].active);
        manager
            .deactivate("fixture-recovery", "test")
            .await
            .expect("stop recovered fixture");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_closed_reader_rebuilds_live_child() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let starts_path = dir.path().join("mcp-closed-reader-starts.log");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        ));
        let mut config = McpServerConfig::new_stdio(
            "fixture-closed-reader",
            script_path.to_string_lossy(),
            vec![starts_path.to_string_lossy().into_owned()],
            HashMap::from([("MCP_CLOSE_STDOUT".to_string(), "1".to_string())]),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install closed-reader fixture");
        manager
            .activate("fixture-closed-reader", "test")
            .await
            .expect("activate closed-reader fixture");

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    manager
                        .mcp_process_manager
                        .state("fixture-closed-reader")
                        .await,
                    Ok(crate::tools::mcp::McpProcessState::StdioClosed)
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("fixture stdout did not close");

        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_start_count(&starts_path, 2).await;
        wait_for_mcp_tool(&manager, "fixture-closed-reader_ping").await;
        manager
            .deactivate("fixture-closed-reader", "test")
            .await
            .expect("stop closed-reader fixture");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_recovery_resolves_rotated_secret() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let starts_path = dir.path().join("mcp-secret-recovery-starts.log");
        let observed_path = dir.path().join("mcp-secret-observed.log");
        let crash_path = dir.path().join("mcp-secret-crash-once");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        ));
        manager
            .secrets
            .create(
                "test",
                crate::secrets::CreateSecretParams::new("rotating_token", "old-test-value"),
            )
            .await
            .expect("create initial secret");
        let mut config = McpServerConfig::new_stdio_with_secrets(
            "fixture-secret-recovery",
            script_path.to_string_lossy(),
            vec![starts_path.to_string_lossy().into_owned()],
            HashMap::from([
                (
                    "MCP_CRASH_ONCE_FILE".to_string(),
                    crash_path.to_string_lossy().into_owned(),
                ),
                (
                    "MCP_TOKEN_OBSERVED_FILE".to_string(),
                    observed_path.to_string_lossy().into_owned(),
                ),
            ]),
            HashMap::from([(
                "MCP_ROTATING_TOKEN".to_string(),
                "rotating_token".to_string(),
            )]),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install secret recovery fixture");
        manager
            .activate("fixture-secret-recovery", "test")
            .await
            .expect("activate secret recovery fixture");
        wait_for_mcp_exit(&manager, "fixture-secret-recovery").await;

        assert!(
            manager
                .secrets
                .delete("test", "rotating_token")
                .await
                .unwrap()
        );
        manager
            .secrets
            .create(
                "test",
                crate::secrets::CreateSecretParams::new("rotating_token", "new-test-value"),
            )
            .await
            .expect("create rotated secret");
        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_start_count(&starts_path, 2).await;
        wait_for_mcp_tool(&manager, "fixture-secret-recovery_ping").await;

        let observed = std::fs::read_to_string(&observed_path).expect("read observed secrets");
        assert_eq!(observed.lines().collect::<Vec<_>>(), ["old", "new"]);
        manager
            .deactivate("fixture-secret-recovery", "test")
            .await
            .expect("stop secret recovery fixture");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_supervisor_recovers_without_manual_poll() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let starts_path = dir.path().join("mcp-supervisor-starts.log");
        let crash_path = dir.path().join("mcp-supervisor-crash-once");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        ));
        let mut config = McpServerConfig::new_stdio(
            "fixture-supervisor",
            script_path.to_string_lossy(),
            vec![starts_path.to_string_lossy().into_owned()],
            HashMap::from([(
                "MCP_CRASH_ONCE_FILE".to_string(),
                crash_path.to_string_lossy().into_owned(),
            )]),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install supervisor fixture");
        manager
            .activate("fixture-supervisor", "test")
            .await
            .expect("activate supervisor fixture");
        wait_for_mcp_exit(&manager, "fixture-supervisor").await;

        manager.start_mcp_supervisor().await;
        wait_for_mcp_start_count(&starts_path, 2).await;
        wait_for_mcp_tool(&manager, "fixture-supervisor_ping").await;

        assert!(manager.tool_registry.has("fixture-supervisor_ping").await);
        assert!(matches!(
            manager
                .mcp_process_manager
                .state("fixture-supervisor")
                .await
                .expect("supervisor process state"),
            crate::tools::mcp::McpProcessState::Running
        ));
        manager.shutdown_mcp_supervisor().await;
        manager
            .deactivate("fixture-supervisor", "test")
            .await
            .expect("stop supervisor fixture");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_recovery_isolated_and_cancellable_per_server() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let hung_starts = dir.path().join("mcp-hung-starts.log");
        let fast_starts = dir.path().join("mcp-fast-starts.log");
        let hung_crash = dir.path().join("mcp-hung-crash-once");
        let fast_crash = dir.path().join("mcp-fast-crash-once");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        ));

        for config in [
            McpServerConfig::new_stdio(
                "fixture-hung",
                script_path.to_string_lossy(),
                vec![hung_starts.to_string_lossy().into_owned()],
                HashMap::from([
                    (
                        "MCP_CRASH_ONCE_FILE".to_string(),
                        hung_crash.to_string_lossy().into_owned(),
                    ),
                    ("MCP_HANG_AFTER_CRASH".to_string(), "1".to_string()),
                ]),
            ),
            McpServerConfig::new_stdio(
                "fixture-fast",
                script_path.to_string_lossy(),
                vec![fast_starts.to_string_lossy().into_owned()],
                HashMap::from([(
                    "MCP_CRASH_ONCE_FILE".to_string(),
                    fast_crash.to_string_lossy().into_owned(),
                )]),
            ),
        ] {
            manager
                .install_mcp_config(config, "test")
                .await
                .expect("install isolation fixture");
        }
        manager
            .activate("fixture-hung", "test")
            .await
            .expect("activate hung fixture");
        manager
            .activate("fixture-fast", "test")
            .await
            .expect("activate fast fixture");
        wait_for_mcp_exit(&manager, "fixture-hung").await;
        wait_for_mcp_exit(&manager, "fixture-fast").await;

        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_start_count(&hung_starts, 2).await;
        wait_for_mcp_start_count(&fast_starts, 2).await;
        wait_for_mcp_tool(&manager, "fixture-fast_ping").await;
        assert!(!manager.tool_registry.has("fixture-hung_ping").await);

        tokio::time::timeout(
            Duration::from_secs(5),
            manager.deactivate("fixture-hung", "test"),
        )
        .await
        .expect("hung recovery cancellation timed out")
        .expect("deactivate hung fixture");
        manager
            .deactivate("fixture-fast", "test")
            .await
            .expect("deactivate fast fixture");
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_recovery_retries_full_runtime_after_failure() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let starts_path = dir.path().join("mcp-retry-starts.log");
        let crash_path = dir.path().join("mcp-crash-once");
        let fail_path = dir.path().join("mcp-restart-fail-once");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        ));
        let mut config = McpServerConfig::new_stdio(
            "fixture-retry",
            script_path.to_string_lossy(),
            vec![starts_path.to_string_lossy().into_owned()],
            HashMap::from([
                (
                    "MCP_CRASH_ONCE_FILE".to_string(),
                    crash_path.to_string_lossy().into_owned(),
                ),
                (
                    "MCP_FAIL_RESTART_ONCE_FILE".to_string(),
                    fail_path.to_string_lossy().into_owned(),
                ),
            ]),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install retry fixture");
        manager
            .activate("fixture-retry", "test")
            .await
            .expect("activate retry fixture");
        wait_for_mcp_exit(&manager, "fixture-retry").await;

        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_start_count(&starts_path, 2).await;
        wait_for_mcp_health(&manager, "fixture-retry", McpRuntimeHealth::RetryPending).await;
        assert!(!manager.tool_registry.has("fixture-retry_ping").await);
        {
            let mut runtimes = manager.active_mcp_servers.write().await;
            let runtime = runtimes.get_mut("fixture-retry").expect("retry runtime");
            assert_eq!(runtime.health, McpRuntimeHealth::RetryPending);
            assert_eq!(runtime.restart_attempts, 1);
            runtime.retry_after = Some(Instant::now());
        }

        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_start_count(&starts_path, 3).await;
        wait_for_mcp_tool(&manager, "fixture-retry_ping").await;
        assert!(manager.tool_registry.has("fixture-retry_ping").await);
        assert_eq!(
            manager.active_mcp_servers.read().await["fixture-retry"].health,
            McpRuntimeHealth::Healthy
        );
        manager
            .deactivate("fixture-retry", "test")
            .await
            .expect("stop retry fixture");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_failed_rebuild_opens_circuit_and_cleans_runtime() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let starts_path = dir.path().join("mcp-failed-rebuild-starts.log");
        let crash_path = dir.path().join("mcp-failed-rebuild-crash-once");
        let fail_path = dir.path().join("mcp-failed-rebuild-fail-once");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        ));
        let mut config = McpServerConfig::new_stdio(
            "fixture-failed-rebuild",
            script_path.to_string_lossy(),
            vec![starts_path.to_string_lossy().into_owned()],
            HashMap::from([
                (
                    "MCP_CRASH_ONCE_FILE".to_string(),
                    crash_path.to_string_lossy().into_owned(),
                ),
                (
                    "MCP_FAIL_RESTART_ONCE_FILE".to_string(),
                    fail_path.to_string_lossy().into_owned(),
                ),
            ]),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install failed-rebuild fixture");
        manager
            .activate("fixture-failed-rebuild", "test")
            .await
            .expect("activate failed-rebuild fixture");
        wait_for_mcp_exit(&manager, "fixture-failed-rebuild").await;
        manager
            .active_mcp_servers
            .write()
            .await
            .get_mut("fixture-failed-rebuild")
            .expect("failed-rebuild runtime")
            .restart_attempts = super::MCP_RESTART_MAX_ATTEMPTS - 1;

        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_health(
            &manager,
            "fixture-failed-rebuild",
            McpRuntimeHealth::RetryPending,
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(&starts_path)
                .expect("read initial start")
                .lines()
                .count(),
            1
        );
        manager
            .active_mcp_servers
            .write()
            .await
            .get_mut("fixture-failed-rebuild")
            .expect("retry runtime")
            .retry_after = Some(Instant::now());
        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_start_count(&starts_path, 2).await;
        wait_for_mcp_health(
            &manager,
            "fixture-failed-rebuild",
            McpRuntimeHealth::CircuitOpen,
        )
        .await;
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );
        assert!(
            !manager
                .tool_registry
                .has("fixture-failed-rebuild_ping")
                .await
        );
        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("reload circuit-open fixture");
        assert!(
            stored
                .get("fixture-failed-rebuild")
                .expect("stored circuit-open fixture")
                .recovery_circuit_open
        );
        manager
            .activate("fixture-failed-rebuild", "test")
            .await
            .expect("explicit activation clears circuit");
        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("reload explicitly activated fixture");
        assert!(
            !stored
                .get("fixture-failed-rebuild")
                .expect("stored activated fixture")
                .recovery_circuit_open
        );
        manager
            .deactivate("fixture-failed-rebuild", "test")
            .await
            .expect("deactivate failed-rebuild fixture");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_disable_cancels_pending_recovery() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let starts_path = dir.path().join("mcp-disable-starts.log");
        let crash_path = dir.path().join("mcp-crash-once");
        let fail_path = dir.path().join("mcp-restart-fail-once");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        ));
        let mut config = McpServerConfig::new_stdio(
            "fixture-disable",
            script_path.to_string_lossy(),
            vec![starts_path.to_string_lossy().into_owned()],
            HashMap::from([
                (
                    "MCP_CRASH_ONCE_FILE".to_string(),
                    crash_path.to_string_lossy().into_owned(),
                ),
                (
                    "MCP_FAIL_RESTART_ONCE_FILE".to_string(),
                    fail_path.to_string_lossy().into_owned(),
                ),
            ]),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install disable fixture");
        manager
            .activate("fixture-disable", "test")
            .await
            .expect("activate disable fixture");
        wait_for_mcp_exit(&manager, "fixture-disable").await;
        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_start_count(&starts_path, 2).await;
        wait_for_mcp_health(&manager, "fixture-disable", McpRuntimeHealth::RetryPending).await;

        manager
            .deactivate("fixture-disable", "test")
            .await
            .expect("disable during retry");
        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let starts = std::fs::read_to_string(&starts_path).expect("read process starts");
        assert_eq!(starts.lines().count(), 2);
        assert!(!manager.tool_registry.has("fixture-disable_ping").await);
        assert!(manager.active_mcp_servers.read().await.is_empty());
        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("reload disabled fixture");
        assert!(
            !stored
                .get("fixture-disable")
                .expect("stored fixture")
                .enabled
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_crash_loop_opens_recovery_circuit() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let starts_path = dir.path().join("mcp-crash-loop-starts.log");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        ));
        let mut config = McpServerConfig::new_stdio(
            "fixture-crash-loop",
            script_path.to_string_lossy(),
            vec![starts_path.to_string_lossy().into_owned()],
            HashMap::from([("MCP_CRASH_ALWAYS".to_string(), "1".to_string())]),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install crash-loop fixture");
        manager
            .activate("fixture-crash-loop", "test")
            .await
            .expect("activate crash-loop fixture");
        wait_for_mcp_exit(&manager, "fixture-crash-loop").await;

        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_start_count(&starts_path, 2).await;
        wait_for_mcp_health(&manager, "fixture-crash-loop", McpRuntimeHealth::Healthy).await;
        wait_for_mcp_exit(&manager, "fixture-crash-loop").await;

        for attempts in 2..=super::MCP_RESTART_MAX_ATTEMPTS {
            Arc::clone(&manager).supervise_mcp_runtimes_once().await;
            wait_for_mcp_health(
                &manager,
                "fixture-crash-loop",
                McpRuntimeHealth::RetryPending,
            )
            .await;
            {
                let mut runtimes = manager.active_mcp_servers.write().await;
                let runtime = runtimes
                    .get_mut("fixture-crash-loop")
                    .expect("crash-loop runtime");
                assert_eq!(runtime.restart_attempts, attempts - 1);
                runtime.retry_after = Some(Instant::now());
            }
            Arc::clone(&manager).supervise_mcp_runtimes_once().await;
            wait_for_mcp_start_count(&starts_path, attempts as usize + 1).await;
            wait_for_mcp_health(&manager, "fixture-crash-loop", McpRuntimeHealth::Healthy).await;
            wait_for_mcp_exit(&manager, "fixture-crash-loop").await;
        }

        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_health(
            &manager,
            "fixture-crash-loop",
            McpRuntimeHealth::CircuitOpen,
        )
        .await;
        assert!(!manager.tool_registry.has("fixture-crash-loop_ping").await);
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );
        assert_eq!(
            manager.active_mcp_servers.read().await["fixture-crash-loop"].restart_attempts,
            super::MCP_RESTART_MAX_ATTEMPTS
        );

        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let starts = std::fs::read_to_string(&starts_path).expect("read crash-loop starts");
        assert_eq!(starts.lines().count(), 6);

        let listed = manager
            .list(Some(ExtensionKind::McpServer), false, "test")
            .await
            .expect("list circuit-open MCP server");
        assert!(!listed[0].active);
        assert!(!listed[0].recovering);
        assert!(
            listed[0]
                .activation_error
                .as_deref()
                .is_some_and(|error| error.contains("Automatic recovery stopped"))
        );
        manager
            .deactivate("fixture-crash-loop", "test")
            .await
            .expect("deactivate circuit-open fixture");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stdio_mcp_stable_window_resets_recovery_attempts() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let starts_path = dir.path().join("mcp-stable-window-starts.log");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(store),
        ));
        let mut config = McpServerConfig::new_stdio(
            "fixture-stable-window",
            script_path.to_string_lossy(),
            vec![starts_path.to_string_lossy().into_owned()],
            HashMap::from([("MCP_CRASH_ALWAYS".to_string(), "1".to_string())]),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install stable-window fixture");
        manager
            .activate("fixture-stable-window", "test")
            .await
            .expect("activate stable-window fixture");
        wait_for_mcp_exit(&manager, "fixture-stable-window").await;
        {
            let mut runtimes = manager.active_mcp_servers.write().await;
            let runtime = runtimes
                .get_mut("fixture-stable-window")
                .expect("stable-window runtime");
            runtime.restart_attempts = super::MCP_RESTART_MAX_ATTEMPTS;
            runtime.healthy_since = Instant::now() - super::MCP_RESTART_STABILITY_WINDOW;
        }

        Arc::clone(&manager).supervise_mcp_runtimes_once().await;
        wait_for_mcp_start_count(&starts_path, 2).await;
        wait_for_mcp_tool(&manager, "fixture-stable-window_ping").await;
        let runtimes = manager.active_mcp_servers.read().await;
        let runtime = runtimes
            .get("fixture-stable-window")
            .expect("recovered stable-window runtime");
        assert_eq!(runtime.restart_attempts, 1);
        assert_eq!(runtime.health, McpRuntimeHealth::Healthy);
        drop(runtimes);

        manager
            .deactivate("fixture-stable-window", "test")
            .await
            .expect("deactivate stable-window fixture");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_concurrent_mcp_activation_starts_one_stdio_child() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let starts_path = dir.path().join("mcp-starts.log");
        let manager = Arc::new(make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        ));
        let mut config = McpServerConfig::new_stdio(
            "fixture-race",
            script_path.to_string_lossy(),
            vec![starts_path.to_string_lossy().into_owned()],
            HashMap::new(),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install fixture");

        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let first = {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                manager.activate("fixture-race", "test").await
            })
        };
        let second = {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                manager.activate("fixture-race", "test").await
            })
        };
        barrier.wait().await;

        first
            .await
            .expect("first activation task")
            .expect("activate");
        second
            .await
            .expect("second activation task")
            .expect("activate");
        manager
            .deactivate("fixture-race", "test")
            .await
            .expect("stop fixture");

        let starts = std::fs::read_to_string(starts_path).expect("read process starts");
        assert_eq!(starts.lines().count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_failed_mcp_activation_stops_spawned_stdio_child() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        );
        let mut config = McpServerConfig::new_stdio(
            "fixture-failure",
            script_path.to_string_lossy(),
            Vec::new(),
            HashMap::from([("MCP_FAIL_LIST".to_string(), "1".to_string())]),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install fixture");

        let error = manager
            .activate("fixture-failure", "test")
            .await
            .expect_err("activation must fail");
        assert!(error.to_string().contains("fixture list failure"));
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );
        assert!(!manager.tool_registry.has("fixture-failure_ping").await);
        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("reload failed activation fixture");
        assert!(
            !stored
                .get("fixture-failure")
                .expect("stored failed activation fixture")
                .enabled
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_failed_active_mcp_update_persists_config_disabled() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        );
        let mut config = McpServerConfig::new_stdio(
            "fixture-update-failure",
            script_path.to_string_lossy(),
            Vec::new(),
            HashMap::new(),
        );
        config.enabled = false;
        manager
            .install_mcp_config(config, "test")
            .await
            .expect("install update failure fixture");
        manager
            .activate("fixture-update-failure", "test")
            .await
            .expect("activate update failure fixture");

        let replacement = McpServerConfig::new_stdio(
            "fixture-update-failure",
            script_path.to_string_lossy(),
            Vec::new(),
            HashMap::from([("MCP_FAIL_LIST".to_string(), "1".to_string())]),
        );
        let message = manager
            .update_mcp("fixture-update-failure", replacement, "test")
            .await
            .expect("persist update despite reactivation failure");
        assert!(message.contains("server disabled until explicit activation"));
        assert!(
            !manager
                .tool_registry
                .has("fixture-update-failure_ping")
                .await
        );
        assert!(manager.active_mcp_servers.read().await.is_empty());
        assert!(
            manager
                .mcp_process_manager
                .managed_servers()
                .await
                .is_empty()
        );

        let stored = crate::tools::mcp::config::load_mcp_servers_from_db(store.as_ref(), "test")
            .await
            .expect("reload failed update fixture");
        let updated = stored
            .get("fixture-update-failure")
            .expect("stored failed update fixture");
        assert!(!updated.enabled);
        assert!(matches!(
            updated.effective_transport(),
            crate::tools::mcp::config::EffectiveTransport::Stdio { env, .. }
                if env.get("MCP_FAIL_LIST").map(String::as_str) == Some("1")
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_mcp_deactivation_and_removal_skip_another_owner_runtime() {
        use crate::tools::mcp::McpServerConfig;

        let (store, dir) = make_test_store().await;
        let script_path = write_mcp_lifecycle_fixture(&dir);
        let manager = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        );
        for user_id in ["alice", "bob"] {
            manager
                .install_mcp_config(
                    McpServerConfig::new_stdio(
                        "shared-name",
                        script_path.to_string_lossy(),
                        Vec::new(),
                        HashMap::new(),
                    ),
                    user_id,
                )
                .await
                .expect("install owner-scoped fixture");
        }
        manager
            .activate("shared-name", "alice")
            .await
            .expect("activate alice fixture");

        manager
            .deactivate("shared-name", "bob")
            .await
            .expect("bob can disable his inactive configuration");
        assert!(manager.tool_registry.has("shared-name_ping").await);
        assert_eq!(
            manager.mcp_process_manager.managed_servers().await,
            ["shared-name"]
        );
        let alice = manager
            .list(Some(ExtensionKind::McpServer), false, "alice")
            .await
            .expect("list alice");
        let bob = manager
            .list(Some(ExtensionKind::McpServer), false, "bob")
            .await
            .expect("list bob");
        assert!(alice[0].active);
        assert!(!bob[0].active);
        assert_eq!(bob[0].enabled, Some(false));

        manager
            .remove("shared-name", "bob")
            .await
            .expect("bob can remove his inactive configuration");
        assert!(manager.tool_registry.has("shared-name_ping").await);
        assert!(
            manager
                .list(Some(ExtensionKind::McpServer), false, "bob")
                .await
                .expect("list bob after removal")
                .is_empty()
        );

        manager
            .deactivate("shared-name", "alice")
            .await
            .expect("clean up alice fixture");
    }

    #[test]
    fn test_setting_value_is_present() {
        assert!(
            !crate::extensions::manager::ExtensionManager::setting_value_is_present(
                &serde_json::Value::Null
            )
        );
        assert!(
            !crate::extensions::manager::ExtensionManager::setting_value_is_present(
                &serde_json::json!("   ")
            )
        );
        assert!(
            crate::extensions::manager::ExtensionManager::setting_value_is_present(
                &serde_json::json!("openai")
            )
        );
        assert!(
            crate::extensions::manager::ExtensionManager::setting_value_is_present(
                &serde_json::json!(["x"])
            )
        );
    }

    #[tokio::test]
    async fn test_is_tool_setup_field_provided_ignores_disallowed_setting_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (store, _db_dir) = make_test_store().await;
        store
            .set_setting(
                "test",
                "lunarwing_cloud.session_token",
                &serde_json::json!({"token":"secret"}),
            )
            .await
            .expect("set disallowed setting");

        let mgr = make_test_manager_with_dirs(
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(Arc::clone(&store)),
        );
        let field = crate::tools::wasm::ToolFieldSetupSchema {
            name: "provider".to_string(),
            prompt: "Provider".to_string(),
            optional: false,
            input_type: crate::tools::wasm::ToolSetupFieldInputType::Text,
            setting_path: Some("lunarwing_cloud.session_token".to_string()),
            env: None,
            restart_required: false,
        };

        let provided = mgr
            .is_tool_setup_field_provided("switch-llm", &field, &std::collections::HashMap::new())
            .await;
        assert!(
            !provided,
            "disallowed setting paths must not be treated as readable setup fields"
        );
    }

    #[tokio::test]
    async fn test_configure_writes_allowlisted_setting_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (store, _db_dir) = make_test_store().await;
        let tools_dir = write_test_tool(
            dir.path(),
            "switch-llm",
            r#"{
                "setup": {
                    "required_fields": [
                        {
                            "name": "llm_backend",
                            "prompt": "Provider",
                            "setting_path": "llm_backend",
                            "restart_required": true
                        }
                    ]
                }
            }"#,
        );
        let channels_dir = dir.path().join("channels");

        let mgr =
            make_test_manager_with_dirs(None, tools_dir, channels_dir, Some(Arc::clone(&store)));
        let mut fields = std::collections::HashMap::new();
        fields.insert("llm_backend".to_string(), "openai_compatible".to_string());

        let result = mgr
            .configure(
                "switch-llm",
                &std::collections::HashMap::new(),
                &fields,
                "test-user",
            )
            .await
            .expect("save configuration");

        assert!(
            !result.activated,
            "tool should not auto-activate without runtime"
        );
        assert!(
            result.restart_required,
            "backend switch should require restart"
        );
        assert_eq!(
            store
                .get_setting("test", "llm_backend")
                .await
                .expect("get setting"),
            Some(serde_json::json!("openai_compatible"))
        );
    }

    #[tokio::test]
    async fn test_configure_rejects_disallowed_setting_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (store, _db_dir) = make_test_store().await;
        let tools_dir = write_test_tool(
            dir.path(),
            "evil-tool",
            r#"{
                "setup": {
                    "required_fields": [
                        {
                            "name": "session",
                            "prompt": "Session",
                            "setting_path": "lunarwing_cloud.session_token"
                        }
                    ]
                }
            }"#,
        );
        let channels_dir = dir.path().join("channels");

        let mgr =
            make_test_manager_with_dirs(None, tools_dir, channels_dir, Some(Arc::clone(&store)));
        let mut fields = std::collections::HashMap::new();
        fields.insert("session".to_string(), "overwrite".to_string());

        let err = match mgr
            .configure(
                "evil-tool",
                &std::collections::HashMap::new(),
                &fields,
                "test-user",
            )
            .await
        {
            Ok(_) => panic!("disallowed setting_path should fail"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid setting_path"),
            "unexpected error message: {msg}"
        );
        assert_eq!(
            store
                .get_setting("test", "lunarwing_cloud.session_token")
                .await
                .expect("get disallowed setting"),
            None
        );
    }

    #[tokio::test]
    async fn test_activate_wasm_tool_with_runtime_passes_runtime_check() {
        // When the ExtensionManager has a WASM runtime, activation should get
        // past the "WASM runtime not available" check. It will still fail
        // because no .wasm file exists on disk — but the error message should
        // be "not found", NOT "WASM runtime not available".
        let dir = tempfile::tempdir().expect("temp dir");
        let config = crate::tools::wasm::WasmRuntimeConfig::for_testing();
        let runtime = Arc::new(crate::tools::wasm::WasmToolRuntime::new(config).expect("runtime"));
        let mgr = make_test_manager(Some(runtime), dir.path().to_path_buf());

        let err = mgr.activate("nonexistent", "test").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("WASM runtime not available"),
            "Should not fail on runtime check, got: {msg}"
        );
        assert!(
            msg.contains("not found")
                || msg.contains("not installed")
                || msg.contains("Not installed"),
            "Should fail on missing file, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_activate_wasm_tool_without_runtime_fails_with_runtime_error() {
        // When the ExtensionManager has no WASM runtime (None), activation
        // must fail with the "WASM runtime not available" message.
        let dir = tempfile::tempdir().expect("temp dir");
        // Write a fake .wasm file so we don't fail on "not found" first.
        std::fs::write(dir.path().join("fake.wasm"), b"not-a-real-wasm").unwrap();

        let mgr = make_test_manager(None, dir.path().to_path_buf());

        let err = mgr.activate("fake", "test").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("WASM runtime not available"),
            "Expected runtime not available error, got: {msg}"
        );
    }

    #[test]
    fn test_capabilities_files_also_separate() {
        // capabilities.json files for tools and channels should also be separate.
        let dir = tempfile::tempdir().expect("temp dir");
        let tools_dir = dir.path().join("tools");
        let channels_dir = dir.path().join("channels");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::create_dir_all(&channels_dir).unwrap();

        let name = "shared";
        let tool_cap = tools_dir.join(format!("{}.capabilities.json", name));
        let channel_cap = channels_dir.join(format!("{}.capabilities.json", name));

        let tool_caps = r#"{"required_secrets":["SHARED_TOOL_TOKEN"]}"#;
        let channel_caps = r#"{"required_secrets":["SHARED_CHANNEL_TOKEN"]}"#;

        std::fs::write(&tool_cap, tool_caps).unwrap();
        std::fs::write(&channel_cap, channel_caps).unwrap();

        // Both exist with distinct content.
        assert_eq!(std::fs::read_to_string(&tool_cap).unwrap(), tool_caps);
        assert_eq!(std::fs::read_to_string(&channel_cap).unwrap(), channel_caps);
    }

    #[tokio::test]
    async fn test_upgrade_no_installed_extensions() {
        let manager = make_manager_with_temp_dirs();
        let result = manager.upgrade(None, "test").await.unwrap();
        assert!(result.results.is_empty());
        assert!(result.message.contains("No WASM extensions installed"));
    }

    #[tokio::test]
    async fn test_upgrade_mcp_server_rejected() {
        let manager = make_manager_with_temp_dirs();
        // MCP servers can't be upgraded via tool_upgrade
        let err = manager.upgrade(Some("some-mcp"), "test").await;
        // It will fail with NotInstalled because there's no MCP server named "some-mcp",
        // but if it were installed, the MCP code path would be rejected.
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_upgrade_up_to_date_extension() {
        let dir = tempfile::tempdir().expect("temp dir");
        let channels_dir = dir.path().join("channels");
        std::fs::create_dir_all(&channels_dir).unwrap();

        // Write a fake .wasm file and capabilities with current WIT version
        let wasm_path = channels_dir.join("test-channel.wasm");
        std::fs::write(&wasm_path, b"\0asm fake").unwrap();

        let cap_path = channels_dir.join("test-channel.capabilities.json");
        let caps = serde_json::json!({
            "type": "channel",
            "name": "test-channel",
            "wit_version": crate::tools::wasm::WIT_CHANNEL_VERSION,
        });
        std::fs::write(&cap_path, serde_json::to_string(&caps).unwrap()).unwrap();

        let manager = make_manager_custom_dirs(dir.path().join("tools"), channels_dir);

        let result = manager.upgrade(Some("test-channel"), "test").await.unwrap();
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].status, "already_up_to_date");
    }

    #[tokio::test]
    async fn test_upgrade_outdated_not_in_registry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let channels_dir = dir.path().join("channels");
        std::fs::create_dir_all(&channels_dir).unwrap();

        // Write a fake .wasm file and capabilities with OLD WIT version
        let wasm_path = channels_dir.join("custom-channel.wasm");
        std::fs::write(&wasm_path, b"\0asm fake").unwrap();

        let cap_path = channels_dir.join("custom-channel.capabilities.json");
        let caps = serde_json::json!({
            "type": "channel",
            "name": "custom-channel",
            "wit_version": "0.1.0",
        });
        std::fs::write(&cap_path, serde_json::to_string(&caps).unwrap()).unwrap();

        let manager = make_manager_custom_dirs(dir.path().join("tools"), channels_dir);

        let result = manager
            .upgrade(Some("custom-channel"), "test")
            .await
            .unwrap();
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].status, "not_in_registry");
    }

    fn make_manager_with_temp_dirs() -> ExtensionManager {
        let dir = tempfile::tempdir().expect("temp dir");
        make_manager_custom_dirs(dir.path().join("tools"), dir.path().join("channels"))
    }

    fn make_manager_custom_dirs(
        tools_dir: std::path::PathBuf,
        channels_dir: std::path::PathBuf,
    ) -> ExtensionManager {
        make_manager_custom_dirs_and_db(tools_dir, channels_dir, None)
    }

    fn make_manager_custom_dirs_and_db(
        tools_dir: std::path::PathBuf,
        channels_dir: std::path::PathBuf,
        db: Option<Arc<dyn crate::db::Database>>,
    ) -> ExtensionManager {
        use crate::secrets::{InMemorySecretsStore, SecretsCrypto};
        use crate::testing::credentials::TEST_CRYPTO_KEY;
        use crate::tools::ToolRegistry;
        use crate::tools::mcp::process::McpProcessManager;
        use crate::tools::mcp::session::McpSessionManager;

        std::fs::create_dir_all(&tools_dir).ok();
        std::fs::create_dir_all(&channels_dir).ok();

        let master_key = secrecy::SecretString::from(TEST_CRYPTO_KEY.to_string());
        let crypto = Arc::new(
            SecretsCrypto::new(master_key)
                .unwrap_or_else(|err| panic!("failed to construct test crypto: {err}")),
        );

        ExtensionManager::new(
            Arc::new(McpSessionManager::new()),
            Arc::new(McpProcessManager::new()),
            Arc::new(InMemorySecretsStore::new(crypto)),
            Arc::new(ToolRegistry::new()),
            None,
            None,
            tools_dir,
            channels_dir,
            None,
            "test".to_string(),
            db,
            Vec::new(),
        )
    }

    #[test]
    fn test_wasm_channel_runtime_config_includes_owner_id() -> Result<(), String> {
        let updates = build_wasm_channel_runtime_config_updates(
            Some("https://example.test"),
            Some("secret-123"),
            Some(424242),
        );

        require_eq(
            updates.get("tunnel_url"),
            Some(&serde_json::json!("https://example.test")),
            "tunnel_url",
        )?;
        require_eq(
            updates.get("webhook_secret"),
            Some(&serde_json::json!("secret-123")),
            "webhook_secret",
        )?;
        require_eq(
            updates.get("owner_id"),
            Some(&serde_json::json!(424242)),
            "owner_id",
        )
    }

    #[tokio::test]
    async fn test_current_channel_owner_id_uses_runtime_state() -> Result<(), String> {
        let manager = make_manager_with_temp_dirs();
        if manager.current_channel_owner_id("xmpp").await.is_some() {
            return Err("expected no owner id for xmpp before runtime setup".to_string());
        }

        let channels = Arc::new(crate::channels::ChannelManager::new());
        let runtime = Arc::new(
            crate::channels::wasm::WasmChannelRuntime::new(
                crate::channels::wasm::WasmChannelRuntimeConfig::default(),
            )
            .map_err(|e| format!("runtime init failed: {e}"))?,
        );
        let pairing_store = Arc::new(crate::pairing::PairingStore::new());
        let router = Arc::new(crate::channels::wasm::WasmChannelRouter::new());
        let mut owner_ids = std::collections::HashMap::new();
        owner_ids.insert("xmpp".to_string(), 12345_i64);

        manager
            .set_channel_runtime(
                channels,
                runtime,
                pairing_store,
                router,
                owner_ids,
                HashMap::new(),
            )
            .await;

        if manager.current_channel_owner_id("xmpp").await != Some(12345_i64) {
            return Err("expected runtime owner id fast-path for xmpp".to_string());
        }
        if manager.current_channel_owner_id("weechat").await.is_some() {
            return Err("expected no owner id for weechat".to_string());
        }
        if manager
            .notification_target_for_channel("xmpp")
            .await
            .as_deref()
            != Some("test")
        {
            return Err(
                "legacy numeric bindings should route notifications through the owner scope"
                    .to_string(),
            );
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_string_owner_actor_id_takes_runtime_precedence() -> Result<(), String> {
        let manager = make_manager_with_temp_dirs();
        let channels = Arc::new(crate::channels::ChannelManager::new());
        let runtime = Arc::new(
            crate::channels::wasm::WasmChannelRuntime::new(
                crate::channels::wasm::WasmChannelRuntimeConfig::default(),
            )
            .map_err(|error| format!("runtime init failed: {error}"))?,
        );
        let pairing_store = Arc::new(crate::pairing::PairingStore::new());
        let router = Arc::new(crate::channels::wasm::WasmChannelRouter::new());
        let numeric_ids = HashMap::from([("weechat".to_string(), 12345_i64)]);
        let actor_ids =
            HashMap::from([("weechat".to_string(), "account:libera:alice".to_string())]);

        manager
            .set_channel_runtime(
                channels,
                runtime,
                pairing_store,
                router,
                numeric_ids,
                actor_ids,
            )
            .await;

        if manager
            .current_channel_owner_actor_id("weechat")
            .await
            .as_deref()
            != Some("account:libera:alice")
        {
            return Err("string owner actor ID should override the legacy numeric ID".to_string());
        }
        if manager
            .notification_target_for_channel("weechat")
            .await
            .as_deref()
            != Some("test")
        {
            return Err(
                "bound channel notifications should route through the owner scope".to_string(),
            );
        }
        if manager
            .notification_target_for_channel("unbound")
            .await
            .is_some()
        {
            return Err("unbound channels must not synthesize a notification target".to_string());
        }
        Ok(())
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn test_current_channel_owner_id_uses_store_fallback() -> Result<(), String> {
        use crate::db::{Database, SettingsStore};

        let dir = tempfile::tempdir().map_err(|e| format!("tempdir failed: {e}"))?;
        let db_path = dir.path().join("owner-id.db");

        let db = Arc::new(
            crate::db::libsql::LibSqlBackend::new_local(&db_path)
                .await
                .map_err(|e| format!("create local libsql backend failed: {e}"))?,
        );
        db.run_migrations()
            .await
            .map_err(|e| format!("run libsql migrations failed: {e}"))?;

        let tools_dir = dir.path().join("tools");
        let channels_dir = dir.path().join("channels");
        std::fs::create_dir_all(&tools_dir).ok();
        std::fs::create_dir_all(&channels_dir).ok();

        use crate::secrets::{InMemorySecretsStore, SecretsCrypto};
        use crate::testing::credentials::TEST_CRYPTO_KEY;
        use crate::tools::ToolRegistry;
        use crate::tools::mcp::process::McpProcessManager;
        use crate::tools::mcp::session::McpSessionManager;

        let master_key = secrecy::SecretString::from(TEST_CRYPTO_KEY.to_string());
        let crypto = Arc::new(
            SecretsCrypto::new(master_key)
                .map_err(|e| format!("create secrets crypto failed: {e}"))?,
        );

        let manager = ExtensionManager::new(
            Arc::new(McpSessionManager::new()),
            Arc::new(McpProcessManager::new()),
            Arc::new(InMemorySecretsStore::new(crypto)),
            Arc::new(ToolRegistry::new()),
            None,
            None,
            tools_dir,
            channels_dir,
            None,
            "test".to_string(),
            Some(db.clone() as Arc<dyn crate::db::Database>),
            Vec::new(),
        );

        if manager.current_channel_owner_id("xmpp").await.is_some() {
            return Err("expected no owner id before settings seed".to_string());
        }

        db.set_setting(
            "test",
            "channels.wasm_channel_owner_ids.xmpp",
            &serde_json::json!(54321_i64),
        )
        .await
        .map_err(|e| format!("persist owner id in settings failed: {e}"))?;

        if manager.current_channel_owner_id("xmpp").await != Some(54321_i64) {
            return Err("expected store fallback owner id for xmpp".to_string());
        }

        let channels = Arc::new(crate::channels::ChannelManager::new());
        let runtime = Arc::new(
            crate::channels::wasm::WasmChannelRuntime::new(
                crate::channels::wasm::WasmChannelRuntimeConfig::default(),
            )
            .map_err(|e| format!("runtime init failed: {e}"))?,
        );
        let pairing_store = Arc::new(crate::pairing::PairingStore::new());
        let router = Arc::new(crate::channels::wasm::WasmChannelRouter::new());
        let mut owner_ids = std::collections::HashMap::new();
        owner_ids.insert("xmpp".to_string(), 12345_i64);
        manager
            .set_channel_runtime(
                channels,
                runtime,
                pairing_store,
                router,
                owner_ids,
                HashMap::new(),
            )
            .await;

        if manager.current_channel_owner_id("xmpp").await != Some(12345_i64) {
            return Err("expected runtime fast-path owner id precedence".to_string());
        }

        Ok(())
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn test_string_owner_actor_id_uses_store_fallback() -> Result<(), String> {
        use crate::db::{Database, SettingsStore};

        let dir = tempfile::tempdir().map_err(|error| format!("tempdir failed: {error}"))?;
        let db = Arc::new(
            crate::db::libsql::LibSqlBackend::new_local(&dir.path().join("owner-actor.db"))
                .await
                .map_err(|error| format!("create backend failed: {error}"))?,
        );
        db.run_migrations()
            .await
            .map_err(|error| format!("migration failed: {error}"))?;
        let manager = make_manager_custom_dirs_and_db(
            dir.path().join("tools"),
            dir.path().join("channels"),
            Some(db.clone()),
        );

        db.set_setting(
            "test",
            "channels.wasm_channel_owner_actor_ids.weechat",
            &serde_json::json!("account:libera:alice"),
        )
        .await
        .map_err(|error| format!("persist actor ID failed: {error}"))?;

        if manager
            .current_channel_owner_actor_id("weechat")
            .await
            .as_deref()
            != Some("account:libera:alice")
        {
            return Err("expected persisted string owner actor ID".to_string());
        }
        Ok(())
    }

    // ── resolve_env_credentials tests ────────────────────────────────────

    #[test]
    fn test_security_prefix_check() {
        // Placeholders that don't start with the channel prefix must be rejected.
        // All env var names are prefixed with ICTEST1_ to avoid CI collisions.
        let placeholders = vec![
            "ICTEST1_BOT_TOKEN".to_string(), // valid: matches channel prefix
            "ICTEST2_TOKEN".to_string(),     // invalid: wrong channel prefix
            "ICTEST1_UNRELATED_OTHER".to_string(), // valid prefix, but env var not set — not injected
        ];
        let already_injected = std::collections::HashSet::new();

        unsafe { std::env::set_var("ICTEST1_BOT_TOKEN", "good-secret") };
        unsafe { std::env::set_var("ICTEST2_TOKEN", "bad-secret") };
        // ICTEST1_UNRELATED_OTHER intentionally not set — tests both prefix rejection and absence

        let resolved = super::resolve_env_credentials(&placeholders, "ictest1", &already_injected);

        // Only ICTEST1_BOT_TOKEN passes the prefix check for channel "ictest1"
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, "ICTEST1_BOT_TOKEN");
        assert_eq!(resolved[0].1, "good-secret");

        unsafe { std::env::remove_var("ICTEST1_BOT_TOKEN") };
        unsafe { std::env::remove_var("ICTEST2_TOKEN") };
    }

    #[test]
    fn test_already_injected_skipped() {
        // Use unique env var names (ictest3_*) to avoid interference with other tests.
        let placeholders = vec!["ICTEST3_TOKEN".to_string()];
        let mut already_injected = std::collections::HashSet::new();
        already_injected.insert("ICTEST3_TOKEN".to_string());

        unsafe { std::env::set_var("ICTEST3_TOKEN", "secret") };

        let resolved = super::resolve_env_credentials(&placeholders, "ictest3", &already_injected);

        // Already covered by secrets store — env var must be skipped
        assert!(resolved.is_empty());

        unsafe { std::env::remove_var("ICTEST3_TOKEN") };
    }

    #[test]
    fn test_missing_env_var_not_injected() {
        // Use unique env var names (ictest4_*) to avoid interference with other tests.
        let placeholders = vec!["ICTEST4_TOKEN".to_string()];
        let already_injected = std::collections::HashSet::new();

        unsafe { std::env::remove_var("ICTEST4_TOKEN") };

        let resolved = super::resolve_env_credentials(&placeholders, "ictest4", &already_injected);

        assert!(resolved.is_empty());
    }

    #[test]
    fn test_empty_env_var_not_injected() {
        // An env var that exists but is empty must not be injected.
        // Use unique env var names (ictest5_*) to avoid interference with other tests.
        let placeholders = vec!["ICTEST5_TOKEN".to_string()];
        let already_injected = std::collections::HashSet::new();

        unsafe { std::env::set_var("ICTEST5_TOKEN", "") };

        let resolved = super::resolve_env_credentials(&placeholders, "ictest5", &already_injected);

        assert!(resolved.is_empty());

        unsafe { std::env::remove_var("ICTEST5_TOKEN") };
    }

    #[test]
    fn test_empty_channel_name_returns_nothing() {
        // An empty channel name must never match any env var (prefix would be "_").
        let placeholders = vec!["_TOKEN".to_string(), "ICTEST6_TOKEN".to_string()];
        let already_injected = std::collections::HashSet::new();

        unsafe { std::env::set_var("_TOKEN", "bad") };
        unsafe { std::env::set_var("ICTEST6_TOKEN", "bad") };

        let resolved = super::resolve_env_credentials(&placeholders, "", &already_injected);

        assert!(resolved.is_empty(), "empty channel name must match nothing");

        unsafe { std::env::remove_var("_TOKEN") };
        unsafe { std::env::remove_var("ICTEST6_TOKEN") };
    }

    #[tokio::test]
    async fn test_remove_wasm_tool_clears_pending_oauth_state_and_activation_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mgr = make_test_manager(None, dir.path().to_path_buf());

        std::fs::write(dir.path().join("gmail.wasm"), b"fake-tool").expect("write tool");
        let unrelated: Arc<dyn Tool> = Arc::new(NamedTestTool {
            name: "gmail".to_string(),
        });
        mgr.tool_registry.register(Arc::clone(&unrelated)).await;

        let listener = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        let abort_handle = listener.abort_handle();
        mgr.pending_auth.write().await.insert(
            ("gmail".to_string(), "test".to_string()),
            super::PendingAuth {
                _name: "gmail".to_string(),
                _kind: ExtensionKind::WasmTool,
                created_at: std::time::Instant::now(),
                task_handle: Some(listener),
            },
        );
        mgr.pending_auth.write().await.insert(
            ("gmail".to_string(), "other-user".to_string()),
            super::PendingAuth {
                _name: "gmail".to_string(),
                _kind: ExtensionKind::WasmTool,
                created_at: std::time::Instant::now(),
                task_handle: None,
            },
        );

        mgr.activation_errors
            .write()
            .await
            .insert("gmail".to_string(), "cached failure".to_string());

        let secrets = Arc::clone(&mgr.secrets);
        mgr.pending_oauth_flows().write().await.insert(
            "gmail-state".to_string(),
            crate::cli::oauth_defaults::PendingOAuthFlow {
                extension_name: "gmail".to_string(),
                display_name: "Gmail".to_string(),
                token_url: "https://example.com/token".to_string(),
                client_id: "client123".to_string(),
                client_secret: None,
                redirect_uri: "https://example.com/oauth/callback".to_string(),
                code_verifier: None,
                access_token_field: "access_token".to_string(),
                secret_name: "google_oauth_token".to_string(),
                provider: None,
                validation_endpoint: None,
                scopes: vec![],
                user_id: "test".to_string(),
                secrets: Arc::clone(&secrets),
                sse_manager: None,
                gateway_token: None,
                token_exchange_extra_params: std::collections::HashMap::new(),
                client_id_secret_name: None,
                client_secret_secret_name: None,
                client_secret_expires_at: None,
                auto_activate_extension: false,
                created_at: std::time::Instant::now(),
            },
        );
        mgr.pending_oauth_flows().write().await.insert(
            "other-state".to_string(),
            crate::cli::oauth_defaults::PendingOAuthFlow {
                extension_name: "web-search".to_string(),
                display_name: "Web Search".to_string(),
                token_url: "https://example.com/token".to_string(),
                client_id: "client456".to_string(),
                client_secret: None,
                redirect_uri: "https://example.com/oauth/callback".to_string(),
                code_verifier: None,
                access_token_field: "access_token".to_string(),
                secret_name: "other_token".to_string(),
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
                created_at: std::time::Instant::now(),
            },
        );
        mgr.pending_oauth_flows().write().await.insert(
            "gmail-other-user-state".to_string(),
            crate::cli::oauth_defaults::PendingOAuthFlow {
                extension_name: "gmail".to_string(),
                display_name: "Gmail".to_string(),
                token_url: "https://example.com/token".to_string(),
                client_id: "client789".to_string(),
                client_secret: None,
                redirect_uri: "https://example.com/oauth/callback".to_string(),
                code_verifier: None,
                access_token_field: "access_token".to_string(),
                secret_name: "other_user_token".to_string(),
                provider: None,
                validation_endpoint: None,
                scopes: vec![],
                user_id: "other-user".to_string(),
                secrets: Arc::clone(&mgr.secrets),
                sse_manager: None,
                gateway_token: None,
                token_exchange_extra_params: std::collections::HashMap::new(),
                client_id_secret_name: None,
                client_secret_secret_name: None,
                client_secret_expires_at: None,
                auto_activate_extension: false,
                created_at: std::time::Instant::now(),
            },
        );

        let result = mgr.remove("gmail", "test").await;
        assert!(result.is_ok(), "remove should succeed: {:?}", result.err());

        tokio::task::yield_now().await;

        assert!(
            mgr.pending_auth
                .read()
                .await
                .get(&("gmail".to_string(), "test".to_string()))
                .is_none(),
            "pending auth entry should be removed"
        );
        assert!(
            mgr.pending_auth
                .read()
                .await
                .contains_key(&("gmail".to_string(), "other-user".to_string())),
            "another user's pending auth entry must remain"
        );
        assert!(
            abort_handle.is_finished(),
            "pending auth listener should be aborted"
        );
        assert!(
            !mgr.activation_errors.read().await.contains_key("gmail"),
            "stale activation error should be cleared"
        );
        let registered = mgr
            .tool_registry
            .get("gmail")
            .await
            .expect("unrelated registration retained");
        assert!(Arc::ptr_eq(&registered, &unrelated));

        let flows = mgr.pending_oauth_flows().read().await;
        assert!(
            !flows.contains_key("gmail-state"),
            "gateway OAuth flow for removed extension should be cleared"
        );
        assert!(
            flows.contains_key("gmail-other-user-state"),
            "another user's OAuth flow with the same extension name must remain"
        );
        assert!(
            flows.contains_key("other-state"),
            "unrelated pending OAuth flows should be retained"
        );
    }

    #[tokio::test]
    async fn test_remove_wasm_channel_clears_activation_error_and_deletes_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let tools_dir = dir.path().join("tools");
        let channels_dir = dir.path().join("channels");
        let mgr = make_test_manager_with_dirs(None, tools_dir, channels_dir.clone(), None);

        let wasm_path = channels_dir.join("xmpp.wasm");
        let cap_path = channels_dir.join("xmpp.capabilities.json");
        std::fs::write(&wasm_path, b"fake-channel").expect("write channel");
        std::fs::write(&cap_path, b"{}").expect("write capabilities");

        mgr.activation_errors
            .write()
            .await
            .insert("xmpp".to_string(), "channel failed".to_string());

        let result = mgr.remove("xmpp", "test").await;
        assert!(result.is_ok(), "remove should succeed: {:?}", result.err());

        assert!(
            !mgr.activation_errors.read().await.contains_key("xmpp"),
            "channel activation error should be cleared on remove"
        );
        assert!(
            !wasm_path.exists(),
            "channel wasm file should be deleted on remove"
        );
        assert!(
            !cap_path.exists(),
            "channel capabilities file should be deleted on remove"
        );
    }

    #[test]
    fn test_sanitize_url_with_query_params() {
        let url = "https://api.example.com/path?api_key=secret123&token=abc";
        let result = super::sanitize_url_for_logging(url);
        assert_eq!(result, "https://api.example.com/path");
        assert!(!result.contains("api_key"));
        assert!(!result.contains("secret123"));
        assert!(!result.contains("token"));
    }

    #[test]
    fn test_sanitize_url_with_credentials() {
        let url = "https://user:password@api.example.com:8080/path";
        let result = super::sanitize_url_for_logging(url);
        assert!(!result.contains("user"));
        assert!(!result.contains("password"));
        assert!(!result.contains("@"));
        assert!(result.contains("api.example.com"));
        assert!(result.contains(":8080"));
    }

    #[test]
    fn test_sanitize_url_with_fragment() {
        let url = "https://api.example.com/path#section";
        let result = super::sanitize_url_for_logging(url);
        assert_eq!(result, "https://api.example.com/path");
        assert!(!result.contains("#"));
        assert!(!result.contains("section"));
    }

    #[test]
    fn test_sanitize_url_with_port() {
        let url = "https://api.example.com:9443/path?key=value";
        let result = super::sanitize_url_for_logging(url);
        assert_eq!(result, "https://api.example.com:9443/path");
        assert!(result.contains(":9443"));
        assert!(!result.contains("key"));
    }

    #[test]
    fn test_sanitize_url_with_all_components() {
        let url = "https://admin:secret@api.example.com:8080/v1/data?api_key=xyz#results";
        let result = super::sanitize_url_for_logging(url);
        assert!(!result.contains("admin"));
        assert!(!result.contains("secret"));
        assert!(!result.contains("@"));
        assert!(!result.contains("api_key"));
        assert!(!result.contains("xyz"));
        assert!(!result.contains("#"));
        assert!(!result.contains("results"));
        assert!(result.contains("api.example.com:8080"));
        assert!(result.contains("/v1/data"));
    }

    #[test]
    fn test_sanitize_url_malformed() {
        // Malformed URL should fallback to string splitting
        let url = "https://[invalid-url";
        let result = super::sanitize_url_for_logging(url);
        // Malformed URL without query should return as-is via fallback
        assert_eq!(result, url);

        // Should still strip query params via fallback
        let url_with_query = "https://[invalid-url?key=secret";
        let result_with_query = super::sanitize_url_for_logging(url_with_query);
        assert_eq!(result_with_query, "https://[invalid-url");
        assert!(!result_with_query.contains("?"));
        assert!(!result_with_query.contains("secret"));
    }

    #[test]
    fn test_sanitize_url_short_string() {
        let url = "short";
        let result = super::sanitize_url_for_logging(url);
        assert_eq!(result, "short");
    }

    #[test]
    fn test_sanitize_url_not_url_like() {
        let input = "this is not a url";
        let result = super::sanitize_url_for_logging(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_sanitize_url_preserves_path() {
        let url = "https://api.example.com/v1/users/123/profile";
        let result = super::sanitize_url_for_logging(url);
        assert_eq!(result, url);
        assert!(result.contains("/v1/users/123/profile"));
    }

    // ---- gateway mode detection tests ----
    // Regression tests for a bug where MCP OAuth called `open::that()` on the
    // server machine instead of returning an auth URL to the gateway frontend.
    // The root cause was that `should_use_gateway_mode()` only checked the
    // `IRONCLAW_OAUTH_CALLBACK_URL` env var, ignoring `self.tunnel_url`.

    /// Build a minimal ExtensionManager with a custom tunnel_url.
    fn make_manager_with_tunnel(tunnel_url: Option<String>) -> ExtensionManager {
        use crate::secrets::{InMemorySecretsStore, SecretsCrypto};
        use crate::tools::mcp::process::McpProcessManager;
        use crate::tools::mcp::session::McpSessionManager;

        let key = secrecy::SecretString::from(crate::secrets::keychain::generate_master_key_hex());
        let crypto = Arc::new(SecretsCrypto::new(key).expect("crypto"));
        let secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(InMemorySecretsStore::new(crypto));
        let tools = Arc::new(crate::tools::ToolRegistry::new());
        let mcp = Arc::new(McpSessionManager::new());
        let dir = std::env::temp_dir().join("lunarwing-test-gateway-mode");

        ExtensionManager::new(
            mcp,
            Arc::new(McpProcessManager::new()),
            secrets,
            tools,
            None,
            None,
            dir.clone(),
            dir,
            tunnel_url,
            "test".to_string(),
            None,
            vec![],
        )
    }

    #[test]
    fn should_use_gateway_mode_true_for_tunnel_url() {
        let _guard = crate::config::helpers::lock_env();
        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe {
            std::env::remove_var("IRONCLAW_OAUTH_CALLBACK_URL");
        }

        let mgr = make_manager_with_tunnel(Some("https://my-gateway.example.com".into()));
        assert!(
            mgr.should_use_gateway_mode(),
            "should detect gateway mode from tunnel_url"
        );

        unsafe {
            if let Some(val) = original {
                std::env::set_var("IRONCLAW_OAUTH_CALLBACK_URL", val);
            }
        }
    }

    #[test]
    fn should_use_gateway_mode_false_without_tunnel() {
        let _guard = crate::config::helpers::lock_env();
        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
        unsafe {
            std::env::remove_var("IRONCLAW_OAUTH_CALLBACK_URL");
        }

        let mgr = make_manager_with_tunnel(None);
        assert!(
            !mgr.should_use_gateway_mode(),
            "should not detect gateway mode without tunnel_url or env var"
        );

        unsafe {
            if let Some(val) = original {
                std::env::set_var("IRONCLAW_OAUTH_CALLBACK_URL", val);
            }
        }
    }

    #[test]
    fn managed_tunnel_url_can_be_set_after_construction() {
        let _guard = crate::config::helpers::lock_env();
        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
        unsafe {
            std::env::remove_var("IRONCLAW_OAUTH_CALLBACK_URL");
        }

        let manager = make_manager_with_tunnel(None);
        manager.set_tunnel_url(Some("https://managed.example.com".to_string()));
        assert!(manager.should_use_gateway_mode());
        assert_eq!(
            manager.current_tunnel_url().as_deref(),
            Some("https://managed.example.com")
        );

        manager.set_tunnel_url(None);
        assert!(manager.current_tunnel_url().is_none());

        unsafe {
            if let Some(value) = original {
                std::env::set_var("IRONCLAW_OAUTH_CALLBACK_URL", value);
            }
        }
    }

    #[test]
    fn should_use_gateway_mode_false_for_loopback_tunnel() {
        let _guard = crate::config::helpers::lock_env();
        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
        unsafe {
            std::env::remove_var("IRONCLAW_OAUTH_CALLBACK_URL");
        }

        let mgr = make_manager_with_tunnel(Some("http://127.0.0.1:3001".into()));
        assert!(
            !mgr.should_use_gateway_mode(),
            "should not detect gateway mode for loopback tunnel_url"
        );

        unsafe {
            if let Some(val) = original {
                std::env::set_var("IRONCLAW_OAUTH_CALLBACK_URL", val);
            }
        }
    }

    /// Helper to run an async test body while holding the env mutex.
    /// Clears `IRONCLAW_OAUTH_CALLBACK_URL` for the duration, restoring on drop.
    struct EnvGuard {
        original: Option<String>,
        _mutex: crate::config::helpers::EnvGuard,
    }

    impl EnvGuard {
        fn new() -> Self {
            let guard = crate::config::helpers::lock_env();
            let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
            // SAFETY: Under ENV_MUTEX, no concurrent env access.
            unsafe {
                std::env::remove_var("IRONCLAW_OAUTH_CALLBACK_URL");
            }
            Self {
                original,
                _mutex: guard,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: Under ENV_MUTEX (still held by _mutex), no concurrent env access.
            unsafe {
                if let Some(ref val) = self.original {
                    std::env::set_var("IRONCLAW_OAUTH_CALLBACK_URL", val);
                } else {
                    std::env::remove_var("IRONCLAW_OAUTH_CALLBACK_URL");
                }
            }
        }
    }

    #[tokio::test]
    async fn gateway_callback_redirect_uri_from_tunnel_url() {
        let _env = EnvGuard::new();

        let mgr = make_manager_with_tunnel(Some("https://my-gateway.example.com".into()));
        assert_eq!(
            mgr.gateway_callback_redirect_uri().await,
            Some("https://my-gateway.example.com/oauth/callback".to_string()),
        );
    }

    #[tokio::test]
    async fn gateway_callback_redirect_uri_none_without_tunnel() {
        let _env = EnvGuard::new();

        let mgr = make_manager_with_tunnel(None);
        assert_eq!(mgr.gateway_callback_redirect_uri().await, None);
    }

    #[tokio::test]
    async fn gateway_callback_redirect_uri_trims_trailing_slash() {
        let _env = EnvGuard::new();

        let mgr = make_manager_with_tunnel(Some("https://my-gateway.example.com/".into()));
        assert_eq!(
            mgr.gateway_callback_redirect_uri().await,
            Some("https://my-gateway.example.com/oauth/callback".to_string()),
        );
    }

    #[test]
    fn gateway_callback_redirect_uri_does_not_duplicate_callback_path_from_env() {
        let _guard = crate::config::helpers::lock_env();
        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
        unsafe {
            std::env::set_var(
                "IRONCLAW_OAUTH_CALLBACK_URL",
                "https://oauth.test.example/oauth/callback",
            );
        }

        let mgr = make_manager_with_tunnel(None);
        assert_eq!(
            tokio_test::block_on(mgr.gateway_callback_redirect_uri()),
            Some("https://oauth.test.example/oauth/callback".to_string()),
        );

        unsafe {
            if let Some(val) = original {
                std::env::set_var("IRONCLAW_OAUTH_CALLBACK_URL", val);
            } else {
                std::env::remove_var("IRONCLAW_OAUTH_CALLBACK_URL");
            }
        }
    }

    #[test]
    fn gateway_callback_redirect_uri_trims_trailing_slash_from_env_callback() {
        let _guard = crate::config::helpers::lock_env();
        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
        unsafe {
            std::env::set_var(
                "IRONCLAW_OAUTH_CALLBACK_URL",
                "https://oauth.test.example/oauth/callback/",
            );
        }

        let mgr = make_manager_with_tunnel(None);
        assert_eq!(
            tokio_test::block_on(mgr.gateway_callback_redirect_uri()),
            Some("https://oauth.test.example/oauth/callback".to_string()),
        );

        unsafe {
            if let Some(val) = original {
                std::env::set_var("IRONCLAW_OAUTH_CALLBACK_URL", val);
            } else {
                std::env::remove_var("IRONCLAW_OAUTH_CALLBACK_URL");
            }
        }
    }

    #[test]
    fn normalize_hosted_callback_url_preserves_query_params() {
        assert_eq!(
            normalize_hosted_callback_url("https://oauth.test.example?source=hosted"),
            "https://oauth.test.example/oauth/callback?source=hosted"
        );
        assert_eq!(
            normalize_hosted_callback_url(
                "https://oauth.test.example/oauth/callback?source=hosted"
            ),
            "https://oauth.test.example/oauth/callback?source=hosted"
        );
    }

    #[test]
    fn rewrite_oauth_state_param_updates_only_state_query_param() {
        let auth_url =
            "https://auth.example.com/authorize?client_id=abc&state=old-state&hint=state%3Dkeep";
        assert_eq!(
            ExtensionManager::rewrite_oauth_state_param(
                auth_url.to_string(),
                "old-state",
                "new-hosted-state",
            ),
            "https://auth.example.com/authorize?client_id=abc&state=new-hosted-state&hint=state%3Dkeep"
        );
    }

    #[tokio::test]
    async fn gateway_mode_enabled_explicitly() {
        let _env = EnvGuard::new();

        let mgr = make_manager_with_tunnel(None);
        assert!(!mgr.should_use_gateway_mode());

        mgr.enable_gateway_mode("https://my-gateway.example.com".into())
            .await;
        assert!(mgr.should_use_gateway_mode());
        assert_eq!(
            mgr.gateway_callback_redirect_uri().await,
            Some("https://my-gateway.example.com/oauth/callback".to_string()),
        );
    }
    // ── Regression tests for PR #677 (unify-extension-lifecycle) ─────────

    #[tokio::test]
    async fn test_configure_token_picks_first_missing_secret() {
        // Regression: configure_token() must pick the first *missing* secret,
        // not the first non-optional one. This allows multi-secret channels
        // to be configured one secret at a time.
        let dir = tempfile::tempdir().expect("temp dir");
        let channels_dir = dir.path().join("channels");
        std::fs::create_dir_all(&channels_dir).unwrap();

        // Write a fake channel WASM + capabilities with two required secrets
        std::fs::write(channels_dir.join("multi.wasm"), b"\0asm fake").unwrap();
        let caps = serde_json::json!({
            "type": "channel",
            "name": "multi",
            "setup": {
                "required_secrets": [
                    {"name": "SECRET_A", "prompt": "Enter secret A (at least 30 chars for validation)"},
                    {"name": "SECRET_B", "prompt": "Enter secret B (at least 30 chars for validation)"}
                ]
            }
        });
        std::fs::write(
            channels_dir.join("multi.capabilities.json"),
            serde_json::to_string(&caps).unwrap(),
        )
        .unwrap();

        let mgr = make_manager_custom_dirs(dir.path().join("tools"), channels_dir);

        // Pre-store SECRET_A so it's no longer missing
        mgr.secrets
            .create(
                "test",
                crate::secrets::CreateSecretParams::new("SECRET_A", "value-a"),
            )
            .await
            .expect("store SECRET_A");

        // configure_token should target SECRET_B (the first missing one)
        let _result = mgr.configure_token("multi", "value-b", "test").await;
        // configure will fail at activation (no real WASM runtime), but the
        // secret should still have been stored before activation was attempted.
        // Check that SECRET_B was stored.
        assert!(
            mgr.secrets
                .exists("test", "SECRET_B")
                .await
                .unwrap_or(false),
            "configure_token should have stored SECRET_B (the first missing secret)"
        );
    }

    #[tokio::test]
    async fn test_auth_is_read_only_for_wasm_channel() {
        // Regression: auth() must be a pure status check — it must not store
        // any secrets or modify state. The old API accepted a token parameter.
        let dir = tempfile::tempdir().expect("temp dir");
        let channels_dir = dir.path().join("channels");
        std::fs::create_dir_all(&channels_dir).unwrap();

        std::fs::write(channels_dir.join("test-ch.wasm"), b"\0asm fake").unwrap();
        let caps = serde_json::json!({
            "type": "channel",
            "name": "test-ch",
            "setup": {
                "required_secrets": [
                    {"name": "BOT_TOKEN", "prompt": "Enter bot token (at least 30 chars for prompt validation)"}
                ]
            }
        });
        std::fs::write(
            channels_dir.join("test-ch.capabilities.json"),
            serde_json::to_string(&caps).unwrap(),
        )
        .unwrap();

        let mgr = make_manager_custom_dirs(dir.path().join("tools"), channels_dir);

        // auth() should return a result without storing anything
        let result = mgr.auth("test-ch", "test").await;
        assert!(result.is_ok(), "auth should succeed: {:?}", result.err());

        // No secrets should have been created
        assert!(
            !mgr.secrets
                .exists("test", "BOT_TOKEN")
                .await
                .unwrap_or(true),
            "auth() must not create any secrets — it should be read-only"
        );
    }

    #[test]
    fn test_validation_failed_is_distinct_error_variant() {
        // Regression: ValidationFailed must be a distinct error variant so
        // callers can match on it instead of parsing error message strings.
        let err = ExtensionError::ValidationFailed("Invalid token".to_string());

        assert!(
            matches!(err, ExtensionError::ValidationFailed(_)),
            "Should match ValidationFailed variant"
        );
        assert!(
            !matches!(err, ExtensionError::Other(_)),
            "Must NOT match Other variant"
        );
        assert!(
            !matches!(err, ExtensionError::AuthFailed(_)),
            "Must NOT match AuthFailed variant"
        );

        let msg = err.to_string();
        assert!(
            msg.contains("validation failed"),
            "Display should contain 'validation failed', got: {msg}"
        );
    }

    // ── proxy_client_secret suppression ─────────────────────────────

    #[test]
    fn test_proxy_client_secret_suppressed_when_builtin_matches_with_exchange_proxy() {
        let builtin = crate::cli::oauth_defaults::builtin_credentials("google_oauth_token");
        let builtin_ref = builtin.as_ref();
        let secret = Some(builtin_ref.unwrap().client_secret.to_string());

        let result =
            crate::cli::oauth_defaults::hosted_proxy_client_secret(&secret, builtin_ref, true);
        assert_eq!(
            result, None,
            "built-in desktop secret must be suppressed when the exchange proxy is configured"
        );
    }

    #[test]
    fn test_proxy_client_secret_kept_when_not_builtin_with_exchange_proxy() {
        let builtin = crate::cli::oauth_defaults::builtin_credentials("google_oauth_token");
        let secret = Some("user-entered-custom-secret".to_string());

        let result =
            crate::cli::oauth_defaults::hosted_proxy_client_secret(&secret, builtin.as_ref(), true);
        assert_eq!(
            result,
            Some("user-entered-custom-secret".to_string()),
            "non-builtin secret must be kept even when the exchange proxy is configured"
        );
    }

    #[test]
    fn test_proxy_client_secret_kept_without_exchange_proxy_even_for_builtin_secret() {
        let builtin = crate::cli::oauth_defaults::builtin_credentials("google_oauth_token");
        let builtin_ref = builtin.as_ref();
        let secret = Some(builtin_ref.unwrap().client_secret.to_string());

        let result =
            crate::cli::oauth_defaults::hosted_proxy_client_secret(&secret, builtin_ref, false);
        assert_eq!(
            result, secret,
            "built-in secret must be kept when the callback will exchange directly"
        );
    }

    #[test]
    fn test_proxy_client_secret_none_stays_none() {
        let builtin = crate::cli::oauth_defaults::builtin_credentials("google_oauth_token");

        let result =
            crate::cli::oauth_defaults::hosted_proxy_client_secret(&None, builtin.as_ref(), true);
        assert_eq!(
            result, None,
            "None secret stays None even when the exchange proxy is configured"
        );
    }

    #[test]
    fn test_proxy_client_secret_no_builtin_provider() {
        // MCP/non-Google providers have no builtin credentials
        let builtin = crate::cli::oauth_defaults::builtin_credentials("mcp_notion_access_token");
        assert!(builtin.is_none());

        let secret = Some("dcr-secret".to_string());
        let result =
            crate::cli::oauth_defaults::hosted_proxy_client_secret(&secret, builtin.as_ref(), true);
        assert_eq!(
            result,
            Some("dcr-secret".to_string()),
            "non-builtin provider secret must be kept"
        );
    }
}
