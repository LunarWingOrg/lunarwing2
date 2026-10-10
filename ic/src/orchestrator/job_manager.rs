//! Container lifecycle management for sandboxed jobs.
//!
//! # BOUNDARY (do not cross)
//!
//! This is the orchestrator's **live worker path**: persistent per-job worker
//! containers with their own agent loops, created over the Docker-compatible
//! API via bollard and labeled `lunarwing.job_id`. It is NOT the generic
//! sandbox — that is `crate::sandbox` (podman/docker CLI runner, ephemeral
//! per-command containers labeled `lunarwing.sandbox`, consumed by the shell
//! tool). Despite the historical doc note below, this module shares only
//! `connect_docker()` with the sandbox module; never merge the two paths.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use lunarwing_common::MAX_WORKER_ITERATIONS;
use tokio::sync::RwLock;
use uuid::Uuid;

use futures::StreamExt;

use crate::bootstrap::lunarwing_base_dir;
use crate::db::Database;
use crate::error::OrchestratorError;
use crate::orchestrator::auth::{CredentialGrant, McpCredentialGrant, TokenStore};
use crate::orchestrator::harness;
use crate::sandbox::connect_docker;
use crate::secrets::SecretsStore;
use crate::tools::mcp::config::{McpServerConfig, McpServersFile, McpTransportConfig};

pub const MAX_WORKER_MCP_SERVERS: usize = 8;
const MAX_WORKER_MCP_CREDENTIALS: usize = 32;
const MCP_SECRET_MARKER: &str = "worker-grant";

/// Which mode a sandbox container runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobMode {
    /// Standard LunarWing worker with proxied LLM calls.
    Worker,
    /// LunarWing worker with harness capabilities (WI#326): tmpfs secret
    /// mounts, read-only workspace injection, staged write-back.
    Harness,
    /// Delegates to a named external worker (persistent container speaking lunarwing-agent-v1).
    External(String),
}

impl JobMode {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Worker => "worker",
            Self::Harness => "harness",
            Self::External(name) => name.as_str(),
        }
    }

    pub fn is_external(&self) -> bool {
        matches!(self, Self::External(_))
    }

    pub fn db_value(&self) -> String {
        match self {
            Self::Worker => "worker".to_string(),
            Self::Harness => "harness".to_string(),
            Self::External(name) => format!("external:{name}"),
        }
    }

    pub fn from_db_value(s: &str) -> Self {
        if s == "worker" {
            Self::Worker
        } else if s == "harness" {
            Self::Harness
        } else if let Some(name) = s.strip_prefix("external:") {
            Self::External(name.to_string())
        } else {
            Self::External(s.to_string())
        }
    }
}

impl std::fmt::Display for JobMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Worker => write!(f, "worker"),
            Self::Harness => write!(f, "harness"),
            Self::External(name) => write!(f, "external:{name}"),
        }
    }
}

/// Container runtime flavor behind the Docker-compatible API (CV-2).
///
/// Podman and Docker accept different tmpfs mount options: Docker takes
/// `uid=`/`gid=`, while podman's compat API rejects them and instead offers
/// the `U` option (chown the mount to the container user). Detected once per
/// manager from `/version` and cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerRuntimeKind {
    Docker,
    Podman,
}

impl WorkerRuntimeKind {
    /// tmpfs options for the harness secrets mount. Both shapes end with the
    /// tmpfs owned by the container's worker uid (1000) with mode 0700, so
    /// only the job process can read secret files.
    fn secrets_tmpfs_options(self) -> &'static str {
        match self {
            // Verified against Docker's TmpfsOptions (uid/gid supported).
            Self::Docker => "size=4M,mode=0700,uid=1000,gid=1000,noexec,nosuid",
            // Verified against podman 5.8/6.x via the Docker-compat API:
            // `uid=` is rejected ("unknown mount option"), `U` chowns the
            // tmpfs to the container user.
            Self::Podman => "size=4M,mode=0700,noexec,nosuid,U",
        }
    }
}

/// Configuration for the container job manager.
#[derive(Debug, Clone)]
pub struct ContainerJobConfig {
    /// Docker image for worker containers.
    pub image: String,
    /// Default memory limit in MB.
    pub memory_limit_mb: u64,
    /// Default CPU shares.
    pub cpu_shares: u32,
    /// Port the orchestrator internal API listens on.
    pub orchestrator_port: u16,
}

impl Default for ContainerJobConfig {
    fn default() -> Self {
        Self {
            image: "lunarwing-worker:latest".to_string(),
            memory_limit_mb: 2048,
            cpu_shares: 1024,
            orchestrator_port: 50051,
        }
    }
}

/// State of a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerState {
    Creating,
    Running,
    /// Harness write-back only: container stopped, write-back promotion still
    /// running (CW-7). Waiters must not treat the job as finished — and must
    /// not clean up the staging dir — until promotion lands and the state
    /// flips to `Stopped`.
    Finalizing,
    Stopped,
    Failed,
}

impl std::fmt::Display for ContainerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Creating => write!(f, "creating"),
            Self::Running => write!(f, "running"),
            Self::Finalizing => write!(f, "finalizing"),
            Self::Stopped => write!(f, "stopped"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Handle to a running container job.
#[derive(Debug, Clone)]
pub struct ContainerHandle {
    pub job_id: Uuid,
    pub container_id: String,
    pub state: ContainerState,
    pub mode: JobMode,
    pub owner_id: String,
    /// Immutable, sanitized MCP configuration snapshot sent to worker.
    pub mcp_servers: Vec<McpServerConfig>,
    pub created_at: DateTime<Utc>,
    pub project_dir: Option<PathBuf>,
    pub task_description: String,
    /// Last status message reported by the worker (iteration count, progress, etc.).
    pub last_worker_status: Option<String>,
    /// Which iteration the worker is on (updated via status reports).
    pub worker_iteration: u32,
    /// Completion result from the worker (set when the worker reports done).
    pub completion_result: Option<CompletionResult>,
    /// Harness capability state (WI#326); `Some` only for harness-mode jobs.
    pub harness: Option<HarnessJobState>,
    // NOTE: auth_token is intentionally NOT in this struct.
    // It lives only in the TokenStore (never logged, serialized, or persisted).
}

/// Harness state tracked on a job handle: where staged writes land and what
/// the promotion pipeline did with them.
#[derive(Debug, Clone)]
pub struct HarnessJobState {
    /// Host staging dir mounted RW at `/harness/workspace_out`.
    pub staging_dir: Option<PathBuf>,
    /// Active write-back policy (None = no write-back for this job).
    pub write_back: Option<harness::WriteBackPolicy>,
    /// Promotion outcome, filled by `complete_job` after the container stops.
    pub promotion: Option<harness::PromotionSummary>,
}

/// Fully bound request for one core Docker worker container.
pub struct ContainerJobRequest {
    pub job_id: Uuid,
    pub task: String,
    pub project_dir: Option<PathBuf>,
    pub mode: JobMode,
    pub owner_id: String,
    pub credential_grants: Vec<CredentialGrant>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub mcp_credential_grants: Vec<McpCredentialGrant>,
    pub max_iterations: u32,
    /// Harness capabilities (WI#326); must be `Some` when mode is Harness.
    pub harness: Option<HarnessJobRequest>,
}

/// Prepared harness inputs for one job. Secret values are resolved host-side
/// and travel only into the tmpfs archive; they are never logged or stored.
pub struct HarnessJobRequest {
    /// Secret files to mount under the tmpfs secrets dir.
    pub resolved_secrets: Vec<harness::ResolvedSecretFile>,
    /// Prepared injection dir (files + PROVENANCE.json), mounted read-only.
    pub injection_dir: Option<PathBuf>,
    /// Write-back policy; presence enables the writable staging mount.
    pub write_back: Option<harness::WriteBackPolicy>,
    /// Host staging dir for write-back (created by the caller).
    pub staging_dir: Option<PathBuf>,
}

/// Load and sanitize job-worker MCP configuration for one owner.
///
/// Secret values remain encrypted. Secret names move into separate grants and
/// descriptor references become opaque markers preserving required env keys.
pub async fn snapshot_worker_mcp(
    store: Option<&dyn Database>,
    secrets: Option<&(dyn SecretsStore + Send + Sync)>,
    owner_id: &str,
    selected_names: &[String],
) -> Result<(Vec<McpServerConfig>, Vec<McpCredentialGrant>), OrchestratorError> {
    validate_worker_mcp_selection(selected_names)?;
    if selected_names.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let configured = if let Some(store) = store {
        crate::tools::mcp::config::load_mcp_servers_from_db_or_migrate(store, owner_id).await
    } else {
        crate::tools::mcp::config::load_mcp_servers().await
    }
    .map_err(|error| OrchestratorError::ApiError {
        reason: format!("failed to load worker MCP configuration: {error}"),
    })?;
    prepare_worker_mcp(configured, selected_names, secrets, owner_id).await
}

fn validate_worker_mcp_selection(selected_names: &[String]) -> Result<(), OrchestratorError> {
    if selected_names.len() > MAX_WORKER_MCP_SERVERS {
        return Err(OrchestratorError::ApiError {
            reason: format!(
                "worker MCP server limit exceeded ({} selected, max {MAX_WORKER_MCP_SERVERS})",
                selected_names.len()
            ),
        });
    }
    let mut unique = HashSet::new();
    for name in selected_names {
        if name.is_empty() {
            return Err(OrchestratorError::ApiError {
                reason: "worker MCP server names cannot be empty".to_string(),
            });
        }
        if !unique.insert(name) {
            return Err(OrchestratorError::ApiError {
                reason: format!("duplicate worker MCP server selection '{name}'"),
            });
        }
    }
    Ok(())
}

fn select_worker_mcp_servers(
    configured: &McpServersFile,
    selected_names: &[String],
) -> Result<Vec<McpServerConfig>, OrchestratorError> {
    validate_worker_mcp_selection(selected_names)?;
    selected_names
        .iter()
        .map(|name| {
            let server = configured
                .get(name)
                .ok_or_else(|| OrchestratorError::ApiError {
                    reason: format!("selected worker MCP server '{name}' is not configured"),
                })?;
            if !server.enabled {
                return Err(OrchestratorError::ApiError {
                    reason: format!("selected worker MCP server '{name}' is disabled"),
                });
            }
            if server.recovery_circuit_open {
                return Err(OrchestratorError::ApiError {
                    reason: format!(
                        "selected worker MCP server '{name}' has an open recovery circuit"
                    ),
                });
            }
            if server.placement != crate::tools::mcp::config::McpPlacement::JobWorker {
                return Err(OrchestratorError::ApiError {
                    reason: format!("selected MCP server '{name}' is not placed in a job worker"),
                });
            }
            Ok(server.clone())
        })
        .collect()
}

async fn prepare_worker_mcp(
    configured: McpServersFile,
    selected_names: &[String],
    secrets: Option<&(dyn SecretsStore + Send + Sync)>,
    owner_id: &str,
) -> Result<(Vec<McpServerConfig>, Vec<McpCredentialGrant>), OrchestratorError> {
    let mut servers = select_worker_mcp_servers(&configured, selected_names)?;

    let mut grants = Vec::new();
    for server in &mut servers {
        let Some(McpTransportConfig::Stdio { env_secrets, .. }) = &mut server.transport else {
            return Err(OrchestratorError::ApiError {
                reason: "worker MCP snapshot contained non-stdio transport".to_string(),
            });
        };
        if !env_secrets.is_empty() && secrets.is_none() {
            return Err(OrchestratorError::ApiError {
                reason: "worker MCP secret references require configured secrets store".to_string(),
            });
        }
        for (env_var, secret_name) in env_secrets.iter_mut() {
            let secret_store = secrets.ok_or_else(|| OrchestratorError::ApiError {
                reason: "worker MCP secret references require configured secrets store".to_string(),
            })?;
            let exists = secret_store
                .exists(owner_id, secret_name)
                .await
                .map_err(|_| OrchestratorError::ApiError {
                    reason: format!(
                        "failed to validate worker MCP credential for server '{}' env '{}'",
                        server.name, env_var
                    ),
                })?;
            if !exists {
                return Err(OrchestratorError::ApiError {
                    reason: format!(
                        "missing worker MCP credential for server '{}' env '{}'",
                        server.name, env_var
                    ),
                });
            }
            grants.push(McpCredentialGrant {
                server_name: server.name.clone(),
                secret_name: secret_name.clone(),
                env_var: env_var.clone(),
            });
            *secret_name = MCP_SECRET_MARKER.to_string();
        }
    }
    if grants.len() > MAX_WORKER_MCP_CREDENTIALS {
        return Err(OrchestratorError::ApiError {
            reason: format!(
                "worker MCP credential limit exceeded ({} configured, max {MAX_WORKER_MCP_CREDENTIALS})",
                grants.len()
            ),
        });
    }
    Ok((servers, grants))
}

/// Result reported by a worker on completion.
#[derive(Debug, Clone)]
pub struct CompletionResult {
    pub success: bool,
    pub message: Option<String>,
}

/// Validate that a project directory is under `~/.lunarwing/projects/`.
///
/// Returns the canonicalized path if valid. Creates the base directory if
/// it doesn't exist (so the prefix check always runs).
///
/// # TOCTOU note
///
/// There is a time-of-check/time-of-use gap between `canonicalize()` here
/// and the actual Docker `binds.push()` in the caller. In a multi-tenant
/// system a malicious actor could swap a symlink after validation. This is
/// acceptable in LunarWing's single-tenant design where the user controls
/// the filesystem.
fn validate_bind_mount_path(
    dir: &std::path::Path,
    job_id: Uuid,
) -> Result<PathBuf, OrchestratorError> {
    let canonical = dir
        .canonicalize()
        .map_err(|e| OrchestratorError::ContainerCreationFailed {
            job_id,
            reason: format!(
                "failed to canonicalize project dir {}: {}",
                dir.display(),
                e
            ),
        })?;

    let projects_base = lunarwing_base_dir().join("projects");

    if !projects_base.is_absolute() {
        return Err(OrchestratorError::ContainerCreationFailed {
            job_id,
            reason: "base directory is not absolute; cannot safely validate bind mounts".into(),
        });
    }

    // Ensure the base exists so canonicalize always succeeds.
    std::fs::create_dir_all(&projects_base).map_err(|e| {
        OrchestratorError::ContainerCreationFailed {
            job_id,
            reason: format!(
                "failed to create projects base {}: {}",
                projects_base.display(),
                e
            ),
        }
    })?;

    let canonical_base =
        projects_base
            .canonicalize()
            .map_err(|e| OrchestratorError::ContainerCreationFailed {
                job_id,
                reason: format!(
                    "failed to canonicalize projects base {}: {}",
                    projects_base.display(),
                    e
                ),
            })?;

    if !canonical.starts_with(&canonical_base) {
        return Err(OrchestratorError::ContainerCreationFailed {
            job_id,
            reason: format!(
                "project directory {} is outside allowed base {}",
                canonical.display(),
                canonical_base.display()
            ),
        });
    }

    Ok(canonical)
}

/// Validate a harness injection/staging dir for bind-mounting (CW-3).
///
/// Harness state lives under `~/.lunarwing/harness/` — outside the projects
/// base that any job can mount via an explicit `project_dir` — and the
/// canonical path must be exactly `<harness_base>/<job_id>/<leaf>` for THIS
/// job, so one job's mounts can never point at another job's harness state.
fn validate_harness_mount_path(
    dir: &std::path::Path,
    job_id: Uuid,
) -> Result<PathBuf, OrchestratorError> {
    let canonical = dir
        .canonicalize()
        .map_err(|e| OrchestratorError::ContainerCreationFailed {
            job_id,
            reason: format!(
                "failed to canonicalize harness dir {}: {}",
                dir.display(),
                e
            ),
        })?;

    let base = harness::ensure_harness_base_dir().map_err(|e| {
        OrchestratorError::ContainerCreationFailed {
            job_id,
            reason: format!("failed to create harness base: {e}"),
        }
    })?;
    let canonical_base =
        base.canonicalize()
            .map_err(|e| OrchestratorError::ContainerCreationFailed {
                job_id,
                reason: format!(
                    "failed to canonicalize harness base {}: {}",
                    base.display(),
                    e
                ),
            })?;

    let expected_parent = canonical_base.join(job_id.to_string());
    let in_this_job = canonical
        .parent()
        .map(|parent| parent == expected_parent)
        .unwrap_or(false);
    if !in_this_job {
        return Err(OrchestratorError::ContainerCreationFailed {
            job_id,
            reason: format!(
                "harness dir {} is outside this job's harness root {}",
                canonical.display(),
                expected_parent.display()
            ),
        });
    }

    Ok(canonical)
}

fn worker_command(
    job_id: Uuid,
    orchestrator_url: &str,
    max_iterations: u32,
    require_worker_mcp: bool,
) -> Vec<String> {
    let max_iterations = max_iterations.clamp(1, MAX_WORKER_ITERATIONS);
    let mut command = vec![
        "worker".to_string(),
        "--job-id".to_string(),
        job_id.to_string(),
        "--orchestrator-url".to_string(),
        orchestrator_url.to_string(),
        "--max-iterations".to_string(),
        max_iterations.to_string(),
    ];
    if require_worker_mcp {
        command.push("--require-worker-mcp".to_string());
    }
    command
}

fn validate_worker_mcp_request(
    job_id: Uuid,
    mode: &JobMode,
    project_dir: Option<&std::path::Path>,
    servers: &[McpServerConfig],
    grants: &[McpCredentialGrant],
) -> Result<(), OrchestratorError> {
    let invalid = |reason| OrchestratorError::ContainerCreationFailed { job_id, reason };
    if servers.is_empty() && grants.is_empty() {
        return Ok(());
    }
    if mode.is_external() {
        return Err(invalid(
            "MCP snapshots are supported only by core Docker workers".to_string(),
        ));
    }
    if project_dir.is_none() {
        return Err(invalid(
            "Worker-local MCP requires a job workspace mount".to_string(),
        ));
    }
    if servers.len() > MAX_WORKER_MCP_SERVERS {
        return Err(invalid(format!(
            "worker MCP server limit exceeded ({} configured, max {MAX_WORKER_MCP_SERVERS})",
            servers.len()
        )));
    }
    if grants.len() > MAX_WORKER_MCP_CREDENTIALS {
        return Err(invalid(format!(
            "worker MCP credential limit exceeded ({} configured, max {MAX_WORKER_MCP_CREDENTIALS})",
            grants.len()
        )));
    }

    let mut server_names = HashSet::new();
    let mut expected_grants = HashSet::new();
    for server in servers {
        server.validate().map_err(|error| {
            invalid(format!(
                "invalid worker MCP server '{}': {error}",
                server.name
            ))
        })?;
        if !server.should_run_in_job() || !server_names.insert(server.name.as_str()) {
            return Err(invalid(format!(
                "invalid or duplicate worker MCP server '{}'",
                server.name
            )));
        }
        let Some(McpTransportConfig::Stdio { env_secrets, .. }) = server.transport.as_ref() else {
            return Err(invalid(format!(
                "worker MCP server '{}' must use explicit stdio transport",
                server.name
            )));
        };
        for (env_var, marker) in env_secrets {
            if marker != MCP_SECRET_MARKER
                || !expected_grants.insert((server.name.as_str(), env_var.as_str()))
            {
                return Err(invalid(format!(
                    "invalid worker MCP credential descriptor for server '{}'",
                    server.name
                )));
            }
        }
    }

    let mut actual_grants = HashSet::new();
    for grant in grants {
        if grant.secret_name.trim().is_empty()
            || !actual_grants.insert((grant.server_name.as_str(), grant.env_var.as_str()))
        {
            return Err(invalid(
                "invalid or duplicate worker MCP credential grant".to_string(),
            ));
        }
    }
    if actual_grants != expected_grants {
        return Err(invalid(
            "worker MCP grants do not exactly cover descriptor secret environment".to_string(),
        ));
    }
    Ok(())
}

/// Manages the lifecycle of Docker containers for sandboxed job execution.
pub struct ContainerJobManager {
    config: ContainerJobConfig,
    token_store: TokenStore,
    pub(crate) containers: Arc<RwLock<HashMap<Uuid, ContainerHandle>>>,
    /// Cached Docker connection (created on first use).
    docker: Arc<RwLock<Option<bollard::Docker>>>,
    /// Cached container-runtime detection (created on first use, CV-2).
    runtime_kind: Arc<RwLock<Option<WorkerRuntimeKind>>>,
    /// Broadcast channel for emitting job events when a container exits
    /// without calling /complete.
    job_event_tx: Option<
        tokio::sync::broadcast::Sender<(Uuid, String, crate::channels::web::types::SseEvent)>,
    >,
    /// Direct handle to ContextManager for force-completing abandoned jobs.
    context_manager: Option<Arc<crate::context::ContextManager>>,
    /// Workspace resolver for harness write-back promotion (WI#326).
    write_back_workspace: Option<Arc<dyn crate::tools::builtin::memory::WorkspaceResolver>>,
    /// DB handle for the write-back promotion audit event.
    store: Option<Arc<dyn Database>>,
}

impl ContainerJobManager {
    pub fn new(config: ContainerJobConfig, token_store: TokenStore) -> Self {
        Self {
            config,
            token_store,
            containers: Arc::new(RwLock::new(HashMap::new())),
            docker: Arc::new(RwLock::new(None)),
            runtime_kind: Arc::new(RwLock::new(None)),
            job_event_tx: None,
            context_manager: None,
            write_back_workspace: None,
            store: None,
        }
    }

    pub fn with_completion_deps(
        mut self,
        event_tx: tokio::sync::broadcast::Sender<(
            Uuid,
            String,
            crate::channels::web::types::SseEvent,
        )>,
        context_manager: Arc<crate::context::ContextManager>,
    ) -> Self {
        self.job_event_tx = Some(event_tx);
        self.context_manager = Some(context_manager);
        self
    }

    /// Inject write-back promotion dependencies (WI#326). Without a resolver,
    /// staged writes are never promoted and the job result reports why.
    pub fn with_write_back_deps(
        mut self,
        resolver: Arc<dyn crate::tools::builtin::memory::WorkspaceResolver>,
        store: Option<Arc<dyn Database>>,
    ) -> Self {
        self.write_back_workspace = Some(resolver);
        self.store = store;
        self
    }

    /// Get or create a Docker connection.
    async fn docker(&self) -> Result<bollard::Docker, OrchestratorError> {
        {
            let guard = self.docker.read().await;
            if let Some(ref d) = *guard {
                return Ok(d.clone());
            }
        }
        let docker = connect_docker()
            .await
            .map_err(|e| OrchestratorError::Docker {
                reason: e.to_string(),
            })?;
        *self.docker.write().await = Some(docker.clone());
        Ok(docker)
    }

    /// Detect the container runtime behind the Docker-compatible API, cached
    /// per manager (CV-2). Harness secret mounts fail closed when detection
    /// fails: silently guessing tmpfs options could leave secrets unreadable
    /// or delivered into the wrong ownership mode.
    async fn runtime_kind(&self) -> Result<WorkerRuntimeKind, OrchestratorError> {
        {
            let guard = self.runtime_kind.read().await;
            if let Some(kind) = *guard {
                return Ok(kind);
            }
        }
        let docker = self.docker().await?;
        let version = docker
            .version()
            .await
            .map_err(|e| OrchestratorError::Docker {
                reason: format!("failed to detect container runtime version: {e}"),
            })?;
        let is_podman = version.components.as_ref().is_some_and(|components| {
            components
                .iter()
                .any(|c| c.name.to_ascii_lowercase().contains("podman"))
        });
        let kind = if is_podman {
            WorkerRuntimeKind::Podman
        } else {
            WorkerRuntimeKind::Docker
        };
        *self.runtime_kind.write().await = Some(kind);
        tracing::debug!(runtime = ?kind, "Detected container runtime for harness mounts");
        Ok(kind)
    }

    /// Create and start a new container for a job.
    ///
    /// The caller provides the `job_id` so it can be persisted to the database
    /// before the container is created. Credential grants are stored in the
    /// TokenStore and served on-demand via the `/credentials` endpoint.
    /// Returns the auth token for the worker.
    pub async fn create_job(
        &self,
        request: ContainerJobRequest,
    ) -> Result<String, OrchestratorError> {
        let ContainerJobRequest {
            job_id,
            task,
            project_dir,
            mode,
            owner_id,
            credential_grants,
            mcp_servers,
            mcp_credential_grants,
            max_iterations,
            harness,
        } = request;
        let watcher_owner_id = owner_id.clone();
        if mode == JobMode::Harness && harness.is_none() {
            return Err(OrchestratorError::ContainerCreationFailed {
                job_id,
                reason: "harness mode requires harness options".to_string(),
            });
        }
        if mode != JobMode::Harness && harness.is_some() {
            return Err(OrchestratorError::ContainerCreationFailed {
                job_id,
                reason: "harness options require harness mode".to_string(),
            });
        }
        validate_worker_mcp_request(
            job_id,
            &mode,
            project_dir.as_deref(),
            &mcp_servers,
            &mcp_credential_grants,
        )?;
        let require_worker_mcp = !mcp_servers.is_empty();
        // Generate auth token (stored in TokenStore, never logged)
        let token = self.token_store.create_token(job_id).await;

        // Store credential grants (revoked automatically when the token is revoked)
        self.token_store
            .store_grants(job_id, credential_grants)
            .await;
        self.token_store
            .store_mcp_grants(job_id, mcp_credential_grants)
            .await;

        // Record the handle
        let handle = ContainerHandle {
            job_id,
            container_id: String::new(), // set after container creation
            state: ContainerState::Creating,
            mode: mode.clone(),
            owner_id,
            mcp_servers,
            created_at: Utc::now(),
            project_dir: project_dir.clone(),
            task_description: task,
            last_worker_status: None,
            worker_iteration: 0,
            completion_result: None,
            harness: harness.as_ref().map(|req| HarnessJobState {
                staging_dir: req.staging_dir.clone(),
                write_back: req.write_back.clone(),
                promotion: None,
            }),
        };
        self.containers.write().await.insert(job_id, handle);

        // WI#326 CV-2: harness secrets are served to the worker over its
        // authenticated per-job channel (worker pull), never uploaded into
        // the container image layers. The token store holds them until the
        // job finishes so the daemon can also redact them from job events
        // (CV-7) and scan staged writes for them (CW-10); they are cleared
        // on every terminal path.
        if let Some(ref req) = harness
            && !req.resolved_secrets.is_empty()
        {
            let pairs: Vec<(String, secrecy::SecretString)> = req
                .resolved_secrets
                .iter()
                .map(|s| (s.file_name.clone(), s.value_clone()))
                .collect();
            self.token_store.store_harness_secrets(job_id, pairs).await;
        }

        // Run the actual container creation. On any failure, revoke the token
        // and remove the handle so we don't leak resources.
        match self
            .create_job_inner(
                job_id,
                &token,
                project_dir,
                max_iterations,
                require_worker_mcp,
                watcher_owner_id,
                harness,
            )
            .await
        {
            Ok(()) => Ok(token),
            Err(e) => {
                self.token_store.revoke(job_id).await;
                self.containers.write().await.remove(&job_id);
                harness::cleanup_harness_job_dir(job_id);
                Err(e)
            }
        }
    }

    /// Inner implementation of container creation (separated for cleanup).
    #[allow(clippy::too_many_arguments)]
    async fn create_job_inner(
        &self,
        job_id: Uuid,
        token: &str,
        project_dir: Option<PathBuf>,
        max_iterations: u32,
        require_worker_mcp: bool,
        watcher_owner_id: String,
        harness: Option<HarnessJobRequest>,
    ) -> Result<(), OrchestratorError> {
        // Connect to Docker (reuses cached connection)
        let docker = self.docker().await?;

        // Build container configuration
        let orchestrator_host = if cfg!(target_os = "linux") {
            "172.17.0.1"
        } else {
            "host.docker.internal"
        };

        let orchestrator_url = format!(
            "http://{}:{}",
            orchestrator_host, self.config.orchestrator_port
        );

        // LUNARWING_* is the primary contract; the IRONCLAW_* duplicates are
        // legacy aliases kept for one release so containers built from
        // pre-rename worker images still start.
        let mut env_vec = vec![
            format!("LUNARWING_WORKER_TOKEN={}", token),
            format!("LUNARWING_JOB_ID={}", job_id),
            format!("LUNARWING_ORCHESTRATOR_URL={}", orchestrator_url),
            format!("IRONCLAW_WORKER_TOKEN={}", token),
            format!("IRONCLAW_JOB_ID={}", job_id),
            format!("IRONCLAW_ORCHESTRATOR_URL={}", orchestrator_url),
        ];

        // Build volume mounts (validate project_dir stays within ~/.lunarwing/projects/)
        let mut binds = Vec::new();
        if let Some(ref dir) = project_dir {
            let canonical = validate_bind_mount_path(dir, job_id)?;
            binds.push(format!("{}:/workspace:rw", canonical.display()));
            env_vec.push("LUNARWING_WORKSPACE=/workspace".to_string());
            // Legacy alias for pre-rename worker images.
            env_vec.push("IRONCLAW_WORKSPACE=/workspace".to_string());
        }

        // WI#326 harness mounts: read-only injection, RW write-back staging.
        // Both live under ~/.lunarwing/harness/<job_id>/ — deliberately
        // outside the bind-mountable projects base (CW-3) — and validate
        // against the harness base.
        if let Some(ref harness_req) = harness {
            env_vec.push("LUNARWING_HARNESS_MODE=1".to_string());
            if let Some(ref injection_dir) = harness_req.injection_dir {
                let canonical = validate_harness_mount_path(injection_dir, job_id)?;
                binds.push(format!(
                    "{}:{}:ro",
                    canonical.display(),
                    harness::HARNESS_INJECTION_CONTAINER_DIR
                ));
                env_vec.push(format!(
                    "LUNARWING_INJECTED_DIR={}",
                    harness::HARNESS_INJECTION_CONTAINER_DIR
                ));
            }
            if harness_req.write_back.is_some()
                && let Some(ref staging_dir) = harness_req.staging_dir
            {
                let canonical = validate_harness_mount_path(staging_dir, job_id)?;
                binds.push(format!(
                    "{}:{}:rw",
                    canonical.display(),
                    harness::HARNESS_WRITE_BACK_CONTAINER_DIR
                ));
                env_vec.push(format!(
                    "LUNARWING_WRITE_BACK_DIR={}",
                    harness::HARNESS_WRITE_BACK_CONTAINER_DIR
                ));
            }
            if !harness_req.resolved_secrets.is_empty() {
                env_vec.push(format!(
                    "LUNARWING_SECRETS_DIR={}",
                    harness::HARNESS_SECRETS_CONTAINER_DIR
                ));
                // CV-2: the worker pulls values over its authenticated
                // channel and fails closed unless exactly this set arrives.
                // Names only — values never enter the container env.
                let names: Vec<&str> = harness_req
                    .resolved_secrets
                    .iter()
                    .map(|s| s.file_name.as_str())
                    .collect();
                env_vec.push(format!("LUNARWING_SECRET_FILES={}", names.join(",")));
            }
        }

        let memory_mb = self.config.memory_limit_mb;

        // Create the container
        use bollard::container::{Config, CreateContainerOptions};
        use bollard::models::HostConfig;

        let has_harness_secrets = harness
            .as_ref()
            .is_some_and(|req| !req.resolved_secrets.is_empty());
        let tmpfs_mounts: HashMap<String, String> = if has_harness_secrets {
            // Docker-secrets pattern: tmpfs is volatile, noexec, and owned by
            // the worker uid so only the job process can read secret files.
            // Options are runtime-specific (CV-2): podman's compat API
            // rejects uid=/gid= and needs `U`; Docker takes uid/gid.
            let kind = self.runtime_kind().await?;
            [
                ("/tmp".to_string(), "size=512M".to_string()),
                (
                    harness::HARNESS_SECRETS_CONTAINER_DIR.to_string(),
                    kind.secrets_tmpfs_options().to_string(),
                ),
            ]
            .into_iter()
            .collect()
        } else {
            [("/tmp".to_string(), "size=512M".to_string())]
                .into_iter()
                .collect()
        };

        let host_config = HostConfig {
            binds: if binds.is_empty() { None } else { Some(binds) },
            memory: Some((memory_mb * 1024 * 1024) as i64),
            cpu_shares: Some(self.config.cpu_shares as i64),
            network_mode: Some("bridge".to_string()),
            extra_hosts: Some(vec!["host.docker.internal:host-gateway".to_string()]),
            cap_drop: Some(vec!["ALL".to_string()]),
            cap_add: Some(vec!["CHOWN".to_string()]),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            tmpfs: Some(tmpfs_mounts),
            ..Default::default()
        };

        let cmd = worker_command(
            job_id,
            &orchestrator_url,
            max_iterations,
            require_worker_mcp,
        );

        // Add Docker labels for reaper identification and orphan detection
        let mut labels = std::collections::HashMap::new();
        labels.insert("lunarwing.job_id".to_string(), job_id.to_string());
        labels.insert(
            "lunarwing.created_at".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );

        let container_config = Config {
            image: Some(self.config.image.clone()),
            cmd: Some(cmd),
            env: Some(env_vec),
            host_config: Some(host_config),
            user: Some("1000:1000".to_string()),
            working_dir: Some("/workspace".to_string()),
            labels: Some(labels),
            ..Default::default()
        };

        let container_name = format!("lunarwing-worker-{}", job_id);
        let options = CreateContainerOptions {
            name: container_name,
            ..Default::default()
        };

        let response = docker
            .create_container(Some(options), container_config)
            .await
            .map_err(|e| OrchestratorError::ContainerCreationFailed {
                job_id,
                reason: e.to_string(),
            })?;

        let container_id = response.id;

        // Start the container
        docker
            .start_container::<String>(&container_id, None)
            .await
            .map_err(|e| OrchestratorError::ContainerCreationFailed {
                job_id,
                reason: format!("failed to start container: {}", e),
            })?;

        // WI#326 CV-2: harness secrets are NOT uploaded into the container.
        // The tmpfs mount above is empty at start; the worker pulls its
        // secret values over the authenticated per-job channel and writes
        // them into the tmpfs itself (0440), failing closed — exiting before
        // any work — unless the delivered set exactly matches the expected
        // LUNARWING_SECRET_FILES list. This works identically on Docker and
        // podman and never lands values in overlay layers on host disk.

        // Clone container_id before moving it into the handle.
        let watcher_container_id = container_id.clone();

        // Update handle with container ID
        if let Some(handle) = self.containers.write().await.get_mut(&job_id) {
            handle.container_id = container_id;
            handle.state = ContainerState::Running;
        }

        tracing::info!(
            job_id = %job_id,
            max_iterations = max_iterations.clamp(1, MAX_WORKER_ITERATIONS),
            "Created and started worker container"
        );

        // Spawn a background watcher that detects container exit.
        // If the container exits without calling /complete (e.g. crash,
        // OOM kill, network failure), this watcher force-completes the
        // job so it doesn't stay InProgress forever.
        let watcher_containers = Arc::clone(&self.containers);
        let watcher_docker = Arc::clone(&self.docker);
        let watcher_token_store = self.token_store.clone();
        let watcher_event_tx = self.job_event_tx.clone();
        let watcher_context_manager = self.context_manager.clone();
        tokio::spawn(async move {
            let docker = {
                let guard = watcher_docker.read().await;
                match guard.as_ref() {
                    Some(d) => d.clone(),
                    None => return,
                }
            };

            let wait_result = docker
                .wait_container::<String>(&watcher_container_id, None)
                .next()
                .await;

            // Grace period: let /complete finish processing if it's in flight.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            let already_completed = {
                let containers = watcher_containers.read().await;
                containers
                    .get(&job_id)
                    .map(|h| {
                        // /complete sets the result first, then (for harness
                        // write-back jobs) keeps the handle in Finalizing
                        // until promotion lands — both mean "being completed",
                        // so the watcher must not force-complete over it
                        // (CW-7).
                        h.completion_result.is_some()
                            || matches!(
                                h.state,
                                ContainerState::Stopped | ContainerState::Finalizing
                            )
                    })
                    .unwrap_or(true)
            };

            if already_completed {
                return;
            }

            let exit_info = match wait_result {
                Some(Ok(resp)) => format!("exit code {}", resp.status_code),
                Some(Err(e)) => format!("docker wait error: {}", e),
                None => "container vanished".to_string(),
            };

            tracing::warn!(
                job_id = %job_id,
                info = %exit_info,
                "Container exited without calling /complete — force-completing job"
            );

            {
                let mut containers = watcher_containers.write().await;
                if let Some(handle) = containers.get_mut(&job_id) {
                    if handle.completion_result.is_none() {
                        handle.completion_result = Some(CompletionResult {
                            success: false,
                            message: Some(format!(
                                "Container exited without reporting completion ({})",
                                exit_info
                            )),
                        });
                    }
                    handle.state = ContainerState::Stopped;
                }
            }

            // Clean up container and revoke token
            let _ = docker
                .remove_container(
                    &watcher_container_id,
                    Some(bollard::container::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
            watcher_token_store.revoke(job_id).await;
            // CW-8: a crashed harness job never reaches /complete, so its
            // injection snapshot and staging dir (and any mounted secret
            // values held for redaction) are cleaned up here.
            watcher_token_store.clear_harness_secrets(job_id).await;
            harness::cleanup_harness_job_dir(job_id);

            // Update ContextManager so job_status shows the real state.
            if let Some(ref cm) = watcher_context_manager {
                let _ = cm
                    .update_context(job_id, |ctx| {
                        let _ = ctx.transition_to(
                            crate::context::JobState::Failed,
                            Some(format!(
                                "Container exited without reporting completion ({})",
                                exit_info
                            )),
                        );
                    })
                    .await;
            }

            // Broadcast JobResult so the job monitor can also react.
            if let Some(ref tx) = watcher_event_tx {
                let _ = tx.send((
                    job_id,
                    watcher_owner_id,
                    crate::channels::web::types::SseEvent::JobResult {
                        job_id: job_id.to_string(),
                        status: "failed".to_string(),
                        session_id: None,
                        fallback_deliverable: None,
                    },
                ));
            }
        });

        Ok(())
    }

    /// Stop a running container job.
    pub async fn stop_job(&self, job_id: Uuid) -> Result<(), OrchestratorError> {
        let container_id = {
            let containers = self.containers.read().await;
            containers
                .get(&job_id)
                .map(|h| h.container_id.clone())
                .ok_or(OrchestratorError::ContainerNotFound { job_id })?
        };

        if container_id.is_empty() {
            return Err(OrchestratorError::InvalidContainerState {
                job_id,
                state: "creating (no container ID yet)".to_string(),
            });
        }

        let docker = self.docker().await?;

        // Stop the container (10 second grace period)
        if let Err(e) = docker
            .stop_container(
                &container_id,
                Some(bollard::container::StopContainerOptions { t: 10 }),
            )
            .await
        {
            tracing::warn!(job_id = %job_id, error = %e, "Failed to stop container (may already be stopped)");
        }

        // Remove the container
        if let Err(e) = docker
            .remove_container(
                &container_id,
                Some(bollard::container::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            tracing::warn!(job_id = %job_id, error = %e, "Failed to remove container (may require manual cleanup)");
        }

        // Update state
        if let Some(handle) = self.containers.write().await.get_mut(&job_id) {
            handle.state = ContainerState::Stopped;
        }

        // Revoke the auth token
        self.token_store.revoke(job_id).await;

        // CW-8: stop/cancel is a terminal path — clean up harness state.
        self.token_store.clear_harness_secrets(job_id).await;
        harness::cleanup_harness_job_dir(job_id);

        tracing::info!(job_id = %job_id, "Stopped worker container");

        Ok(())
    }

    /// Mark a job as complete with a result. The container is stopped but the
    /// handle is kept so `CreateJobTool` can read the completion message.
    ///
    /// Harness write-back jobs (WI#326) transition through
    /// `ContainerState::Finalizing`: promotion runs detached after container
    /// teardown is confirmed, and only then does the state flip to `Stopped`
    /// — so a `wait=true` waiter can never race the promotion or clean up the
    /// staging dir mid-walk (CW-7). Guards are always dropped before any
    /// `.write().await`; the MR !1510 shape held a read guard across the
    /// promotion write lock and deadlocked the whole container map (CV-1).
    pub async fn complete_job(
        &self,
        job_id: Uuid,
        result: CompletionResult,
    ) -> Result<(), OrchestratorError> {
        let job_succeeded = result.success;

        // Store the result before stopping.
        let needs_promotion = {
            let mut containers = self.containers.write().await;
            match containers.get_mut(&job_id) {
                Some(handle) => {
                    handle.completion_result = Some(result);
                    let promote = handle
                        .harness
                        .as_ref()
                        .is_some_and(|h| h.write_back.is_some() && h.staging_dir.is_some());
                    handle.state = if promote {
                        ContainerState::Finalizing
                    } else {
                        ContainerState::Stopped
                    };
                    promote
                }
                None => false,
            }
        };

        // Stop container and revoke token (but keep handle in map).
        // Teardown must be confirmed before promotion reads the staging dir:
        // a live container could keep mutating it (CW-3/CW-7).
        let container_id = {
            let containers = self.containers.read().await;
            containers.get(&job_id).map(|h| h.container_id.clone())
        };
        let mut teardown_confirmed = true;
        if let Some(cid) = container_id
            && !cid.is_empty()
        {
            match self.docker().await {
                Ok(docker) => {
                    if let Err(e) = docker
                        .stop_container(
                            &cid,
                            Some(bollard::container::StopContainerOptions { t: 5 }),
                        )
                        .await
                    {
                        teardown_confirmed = false;
                        tracing::warn!(job_id = %job_id, error = %e, "Failed to stop completed container");
                    }
                    if let Err(e) = docker
                        .remove_container(
                            &cid,
                            Some(bollard::container::RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await
                    {
                        teardown_confirmed = false;
                        tracing::warn!(job_id = %job_id, error = %e, "Failed to remove completed container");
                    }
                }
                Err(e) => {
                    teardown_confirmed = false;
                    tracing::warn!(job_id = %job_id, error = %e, "Failed to connect to Docker for container cleanup");
                }
            }
        }
        self.token_store.revoke(job_id).await;

        if needs_promotion {
            let deps = PromotionDeps {
                containers: Arc::clone(&self.containers),
                token_store: self.token_store.clone(),
                workspace: self.write_back_workspace.clone(),
                store: self.store.clone(),
            };
            tokio::spawn(async move {
                finalize_harness_job(deps, job_id, job_succeeded, teardown_confirmed).await;
            });
        }

        tracing::info!(job_id = %job_id, "Completed worker container");
        Ok(())
    }

    /// Remove a completed job handle from memory (called after result is read).
    pub async fn cleanup_job(&self, job_id: Uuid) {
        self.containers.write().await.remove(&job_id);
        self.token_store.clear_harness_secrets(job_id).await;
        harness::cleanup_harness_job_dir(job_id);
    }

    /// Update the worker-reported status for a job.
    pub async fn update_worker_status(
        &self,
        job_id: Uuid,
        message: Option<String>,
        iteration: u32,
    ) {
        if let Some(handle) = self.containers.write().await.get_mut(&job_id) {
            handle.last_worker_status = message;
            handle.worker_iteration = iteration;
        }
    }

    /// Get the handle for a job.
    pub async fn get_handle(&self, job_id: Uuid) -> Option<ContainerHandle> {
        self.containers.read().await.get(&job_id).cloned()
    }

    /// List all active container jobs.
    pub async fn list_jobs(&self) -> Vec<ContainerHandle> {
        self.containers.read().await.values().cloned().collect()
    }

    /// Get a reference to the token store.
    pub fn token_store(&self) -> &TokenStore {
        &self.token_store
    }

    /// Sweep harness dirs that belong to no live container (CW-8 boot path).
    /// Fail-safe: when the runtime cannot be listed, nothing is removed.
    pub async fn sweep_orphaned_harness_dirs(&self) {
        let docker = match self.docker().await {
            Ok(docker) => docker,
            Err(e) => {
                tracing::warn!("Harness dir sweep skipped, no container runtime: {e}");
                return;
            }
        };
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec!["lunarwing.job_id".to_string()]);
        let options = bollard::container::ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };
        let summaries = match docker.list_containers(Some(options)).await {
            Ok(summaries) => summaries,
            Err(e) => {
                tracing::warn!("Harness dir sweep skipped, container list failed: {e}");
                return;
            }
        };
        let live: std::collections::HashSet<Uuid> = summaries
            .iter()
            .filter_map(|s| s.labels.as_ref())
            .filter_map(|labels| labels.get("lunarwing.job_id"))
            .filter_map(|value| Uuid::parse_str(value).ok())
            .collect();
        let removed =
            harness::sweep_stale_harness_dirs(&live, std::time::Duration::from_secs(3600));
        if removed > 0 {
            tracing::info!(removed, "Swept orphaned harness dirs at startup");
        }
    }
}

/// Cloned dependencies for the detached harness finalization task (CW-7).
struct PromotionDeps {
    containers: Arc<RwLock<HashMap<Uuid, ContainerHandle>>>,
    token_store: TokenStore,
    workspace: Option<Arc<dyn crate::tools::builtin::memory::WorkspaceResolver>>,
    store: Option<Arc<dyn Database>>,
}

/// Detached finalization for a harness write-back job (CW-7/8/9): gate on
/// job success and confirmed container teardown, run the promotion pipeline,
/// ALWAYS persist the `harness_write_back` audit event, then flip the handle
/// to `Stopped` and release the job's mounted secret values.
async fn finalize_harness_job(
    deps: PromotionDeps,
    job_id: Uuid,
    job_succeeded: bool,
    teardown_confirmed: bool,
) {
    // Read guard scoped to this block — never held across a write (CV-1).
    let state = {
        let containers = deps.containers.read().await;
        containers.get(&job_id).and_then(|handle| {
            let harness = handle.harness.as_ref()?;
            Some((
                harness.write_back.clone()?,
                harness.staging_dir.clone()?,
                handle.owner_id.clone(),
            ))
        })
    };
    let Some((policy, staging, owner_id)) = state else {
        let mut containers = deps.containers.write().await;
        if let Some(handle) = containers.get_mut(&job_id) {
            handle.state = ContainerState::Stopped;
        }
        return;
    };

    let mut summary = harness::PromotionSummary::default();
    if !job_succeeded {
        // CW-7: write-back is a success path; a failed job's staging never
        // reaches tenant memory.
        summary.skipped = true;
        summary.skip_reason = Some("job did not complete successfully".to_string());
    } else if !teardown_confirmed {
        summary.skipped = true;
        summary.skip_reason =
            Some("container teardown could not be confirmed; write-back refused".to_string());
    } else {
        summary = run_promotion(&deps, job_id, &owner_id, &staging, &policy).await;
    }

    tracing::info!(
        job_id = %job_id,
        promoted = summary.records.len(),
        rejected = summary.rejections.len(),
        skipped = summary.skipped,
        "Harness write-back promotion finished"
    );

    // CW-9: every promotion run — accepted, rejection-only, zero-accept, or
    // skipped — writes an audit event with the full write set and reasons.
    if let Some(store) = &deps.store
        && let Err(e) = store
            .save_job_event(
                job_id,
                "harness_write_back",
                &serde_json::json!({
                    "promoted": summary.records,
                    "rejected": summary.rejections,
                    "dropped_rejections": summary.dropped_rejections,
                    "skipped": summary.skipped,
                    "skip_reason": summary.skip_reason,
                }),
            )
            .await
    {
        tracing::warn!(job_id = %job_id, "Failed to persist write-back audit event: {e}");
    }

    {
        let mut containers = deps.containers.write().await;
        if let Some(handle) = containers.get_mut(&job_id) {
            if let Some(harness_state) = &mut handle.harness {
                harness_state.promotion = Some(summary);
            }
            handle.state = ContainerState::Stopped;
        }
    }
    // The job is fully terminal: release the mounted secret values held for
    // delivery/redaction/scanning.
    deps.token_store.clear_harness_secrets(job_id).await;
}

/// Run the write-back promotion pipeline: screen staged files, then write
/// accepted ones into the tenant workspace, recording the previous content
/// hash for rollback (CW-9).
async fn run_promotion(
    deps: &PromotionDeps,
    job_id: Uuid,
    owner_id: &str,
    staging: &std::path::Path,
    policy: &harness::WriteBackPolicy,
) -> harness::PromotionSummary {
    let mut summary = harness::PromotionSummary::default();
    // CW-10: scan staged content for the job's own mounted secret values in
    // addition to the pattern-based leak scan.
    let redactor = deps.token_store.harness_secret_redactor(job_id).await;
    let plan = match harness::plan_promotion(staging, policy, redactor.as_ref()) {
        Ok(plan) => plan,
        Err(e) => {
            summary.skipped = true;
            summary.skip_reason = Some(format!("staging scan failed: {e}"));
            return summary;
        }
    };
    summary.rejections = plan.rejections;
    summary.dropped_rejections = plan.dropped_rejections;
    if plan.accepted.is_empty() {
        return summary;
    }

    let Some(resolver) = &deps.workspace else {
        summary.skipped = true;
        summary.skip_reason = Some("no workspace available for write-back promotion".to_string());
        return summary;
    };
    let workspace = resolver.resolve(owner_id).await;
    for write in plan.accepted {
        let previous_sha256 = workspace
            .read(&write.relative_path)
            .await
            .ok()
            .map(|doc| harness::sha256_hex(doc.content.as_bytes()));
        match workspace.write(&write.relative_path, &write.content).await {
            Ok(_) => summary.records.push(harness::PromotionRecord {
                relative_path: write.relative_path,
                sha256: write.sha256,
                previous_sha256,
                size_bytes: write.size_bytes,
                job_id,
                owner_id: owner_id.to_string(),
                promoted_at: Utc::now(),
            }),
            Err(e) => summary.rejections.push(harness::PromotionRejection {
                relative_path: write.relative_path,
                reason: format!("workspace write rejected: {e}"),
            }),
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::config::{EffectiveTransport, McpPlacement, McpServerConfig};

    #[test]
    fn test_container_job_config_default() {
        let config = ContainerJobConfig::default();
        assert_eq!(config.orchestrator_port, 50051);
        assert_eq!(config.memory_limit_mb, 2048);
    }

    #[test]
    fn test_container_state_display() {
        assert_eq!(ContainerState::Running.to_string(), "running");
        assert_eq!(ContainerState::Stopped.to_string(), "stopped");
    }

    #[test]
    fn worker_command_propagates_and_clamps_iteration_limit() {
        let job_id = Uuid::new_v4();
        let command = worker_command(job_id, "http://orchestrator", 300, false);
        assert_eq!(command.last().map(String::as_str), Some("300"));

        let clamped = worker_command(job_id, "http://orchestrator", u32::MAX, true);
        assert!(
            clamped
                .iter()
                .any(|value| value == &MAX_WORKER_ITERATIONS.to_string())
        );
        assert_eq!(
            clamped.last().map(String::as_str),
            Some("--require-worker-mcp")
        );
    }

    #[test]
    fn test_validate_bind_mount_valid_path() {
        let base = crate::bootstrap::compute_lunarwing_base_dir().join("projects");
        std::fs::create_dir_all(&base).unwrap();

        let test_dir = base.join("test_validate_bind");
        std::fs::create_dir_all(&test_dir).unwrap();

        let result = validate_bind_mount_path(&test_dir, Uuid::new_v4());
        assert!(result.is_ok());
        let canonical = result.unwrap();
        assert!(canonical.starts_with(base.canonicalize().unwrap()));

        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_validate_bind_mount_rejects_outside_base() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().to_path_buf();

        let result = validate_bind_mount_path(&outside, Uuid::new_v4());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("outside allowed base"),
            "expected 'outside allowed base', got: {}",
            err
        );
    }

    #[test]
    fn test_validate_bind_mount_rejects_nonexistent() {
        let nonexistent = PathBuf::from("/no/such/path/at/all");
        let result = validate_bind_mount_path(&nonexistent, Uuid::new_v4());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("canonicalize"),
            "expected canonicalize error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_update_worker_status() {
        let store = TokenStore::new();
        let mgr = ContainerJobManager::new(ContainerJobConfig::default(), store);
        let job_id = Uuid::new_v4();

        // Insert a handle
        {
            let mut containers = mgr.containers.write().await;
            containers.insert(
                job_id,
                ContainerHandle {
                    job_id,
                    container_id: "test".to_string(),
                    state: ContainerState::Running,
                    mode: JobMode::Worker,
                    owner_id: "owner".to_string(),
                    mcp_servers: Vec::new(),
                    created_at: chrono::Utc::now(),
                    project_dir: None,
                    task_description: "test job".to_string(),
                    last_worker_status: None,
                    worker_iteration: 0,
                    completion_result: None,
                    harness: None,
                },
            );
        }

        mgr.update_worker_status(job_id, Some("Iteration 3".to_string()), 3)
            .await;

        let handle = mgr.get_handle(job_id).await.unwrap();
        assert_eq!(handle.worker_iteration, 3);
        assert_eq!(handle.last_worker_status.as_deref(), Some("Iteration 3"));
    }

    #[tokio::test]
    async fn worker_mcp_snapshot_filters_sanitizes_and_validates_owner() {
        use crate::testing::credentials::test_secrets_store;
        use secrecy::SecretString;

        let secrets = test_secrets_store();
        secrets
            .create(
                "owner-a",
                crate::secrets::CreateSecretParams {
                    name: "real-secret-name".to_string(),
                    value: SecretString::from("value".to_string()),
                    provider: None,
                    expires_at: None,
                },
            )
            .await
            .unwrap();
        let host = McpServerConfig::new_stdio("host", "cat", Vec::new(), HashMap::new());
        let worker = McpServerConfig::new_stdio_with_secrets(
            "worker",
            "cat",
            Vec::new(),
            HashMap::new(),
            HashMap::from([("TOKEN".to_string(), "real-secret-name".to_string())]),
        )
        .with_placement(McpPlacement::JobWorker);
        let mut disabled = worker.clone();
        disabled.name = "disabled".to_string();
        disabled.enabled = false;
        let configured = McpServersFile {
            servers: vec![host, worker, disabled],
            ..Default::default()
        };

        let selected = vec!["worker".to_string()];
        let (snapshot, grants) =
            prepare_worker_mcp(configured.clone(), &selected, Some(&secrets), "owner-a")
                .await
                .unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].secret_name, "real-secret-name");
        let EffectiveTransport::Stdio { env_secrets, .. } = snapshot[0].effective_transport()
        else {
            panic!("worker snapshot must remain stdio");
        };
        assert_eq!(
            env_secrets.get("TOKEN").map(String::as_str),
            Some(MCP_SECRET_MARKER)
        );
        assert!(
            !serde_json::to_string(&snapshot)
                .unwrap()
                .contains("real-secret-name")
        );

        assert!(
            prepare_worker_mcp(configured, &selected, Some(&secrets), "owner-b")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn worker_mcp_snapshot_rejects_overflow() {
        let servers: Vec<_> = (0..=MAX_WORKER_MCP_SERVERS)
            .map(|index| {
                McpServerConfig::new_stdio(
                    format!("worker-{index}"),
                    "cat",
                    Vec::new(),
                    HashMap::new(),
                )
                .with_placement(McpPlacement::JobWorker)
            })
            .collect();
        let selected = servers
            .iter()
            .map(|server| server.name.clone())
            .collect::<Vec<_>>();
        let error = prepare_worker_mcp(
            McpServersFile {
                servers,
                ..Default::default()
            },
            &selected,
            None,
            "owner",
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("limit exceeded"));
    }

    #[tokio::test]
    async fn empty_worker_mcp_selection_grants_none_without_loading_config() {
        let (servers, grants) = snapshot_worker_mcp(None, None, "owner", &[]).await.unwrap();

        assert!(servers.is_empty());
        assert!(grants.is_empty());
    }

    #[test]
    fn worker_mcp_selection_is_explicit_ordered_and_fail_closed() {
        let worker = |name: &str| {
            McpServerConfig::new_stdio(name, "cat", Vec::new(), HashMap::new())
                .with_placement(McpPlacement::JobWorker)
        };
        let mut disabled = worker("disabled");
        disabled.enabled = false;
        let mut circuit_open = worker("circuit-open");
        circuit_open.recovery_circuit_open = true;
        let host = McpServerConfig::new_stdio("host", "cat", Vec::new(), HashMap::new());
        let configured = McpServersFile {
            servers: vec![
                worker("first"),
                worker("second"),
                disabled,
                circuit_open,
                host,
            ],
            ..Default::default()
        };

        let selected = vec!["second".to_string(), "first".to_string()];
        let servers = select_worker_mcp_servers(&configured, &selected).unwrap();
        assert_eq!(
            servers
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );
        assert!(
            select_worker_mcp_servers(&configured, &[])
                .unwrap()
                .is_empty()
        );
        assert!(
            select_worker_mcp_servers(&configured, &["missing".to_string()])
                .unwrap_err()
                .to_string()
                .contains("not configured")
        );
        assert!(
            select_worker_mcp_servers(&configured, &["disabled".to_string()])
                .unwrap_err()
                .to_string()
                .contains("disabled")
        );
        assert!(
            select_worker_mcp_servers(&configured, &["host".to_string()])
                .unwrap_err()
                .to_string()
                .contains("not placed")
        );
        assert!(
            select_worker_mcp_servers(&configured, &["circuit-open".to_string()])
                .unwrap_err()
                .to_string()
                .contains("open recovery circuit")
        );
        assert!(
            select_worker_mcp_servers(&configured, &["first".to_string(), "first".to_string()])
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[tokio::test]
    async fn complete_job_with_harness_write_back_does_not_deadlock() {
        // CV-1 (MR !1510): the let-chain scrutinee held a containers read
        // guard across the promotion write lock; the first write-back
        // completion wedged the whole container map. The timeout turns that
        // regression into a test failure.
        let mgr = ContainerJobManager::new(ContainerJobConfig::default(), TokenStore::new());
        let job_id = Uuid::new_v4();
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let staging = tmp.path().join("workspace_out");
        std::fs::create_dir_all(staging.join("projects")).unwrap(); // safety: test
        std::fs::write(staging.join("projects/out.md"), "result").unwrap(); // safety: test

        {
            let mut containers = mgr.containers.write().await;
            containers.insert(
                job_id,
                ContainerHandle {
                    job_id,
                    // Empty container id: teardown is skipped, no Docker needed.
                    container_id: String::new(),
                    state: ContainerState::Running,
                    mode: JobMode::Harness,
                    owner_id: "owner".to_string(),
                    mcp_servers: Vec::new(),
                    created_at: chrono::Utc::now(),
                    project_dir: None,
                    task_description: "harness test".to_string(),
                    last_worker_status: None,
                    worker_iteration: 0,
                    completion_result: None,
                    harness: Some(HarnessJobState {
                        staging_dir: Some(staging.clone()),
                        write_back: Some(harness::WriteBackPolicy::default()),
                        promotion: None,
                    }),
                },
            );
        }

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            mgr.complete_job(
                job_id,
                CompletionResult {
                    success: true,
                    message: None,
                },
            ),
        )
        .await
        .expect("complete_job must not deadlock") // safety: regression guard
        .unwrap(); // safety: test

        // Promotion runs detached (CW-7): wait for the handle to settle.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let summary = loop {
            let handle = mgr.get_handle(job_id).await.unwrap(); // safety: test
            if handle.state == ContainerState::Stopped
                && let Some(harness_state) = &handle.harness
                && let Some(summary) = &harness_state.promotion
            {
                break summary.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detached promotion did not finish"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        };

        // No workspace resolver is configured in this manager, so the run is
        // skipped with a recorded reason rather than silently dropped (CW-9).
        assert!(summary.skipped);
        assert!(
            summary
                .skip_reason
                .as_deref()
                .unwrap_or("")
                .contains("no workspace")
        );
    }

    #[tokio::test]
    async fn failed_harness_job_skips_write_back_promotion() {
        // CW-7: promotion is a success-only path; failed jobs keep their
        // staging out of tenant memory and record the skip.
        let mgr = ContainerJobManager::new(ContainerJobConfig::default(), TokenStore::new());
        let job_id = Uuid::new_v4();
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let staging = tmp.path().join("workspace_out");
        std::fs::create_dir_all(staging.join("projects")).unwrap(); // safety: test
        std::fs::write(staging.join("projects/out.md"), "result").unwrap(); // safety: test

        {
            let mut containers = mgr.containers.write().await;
            containers.insert(
                job_id,
                ContainerHandle {
                    job_id,
                    container_id: String::new(),
                    state: ContainerState::Running,
                    mode: JobMode::Harness,
                    owner_id: "owner".to_string(),
                    mcp_servers: Vec::new(),
                    created_at: chrono::Utc::now(),
                    project_dir: None,
                    task_description: "harness test".to_string(),
                    last_worker_status: None,
                    worker_iteration: 0,
                    completion_result: None,
                    harness: Some(HarnessJobState {
                        staging_dir: Some(staging.clone()),
                        write_back: Some(harness::WriteBackPolicy::default()),
                        promotion: None,
                    }),
                },
            );
        }

        mgr.complete_job(
            job_id,
            CompletionResult {
                success: false,
                message: Some("worker reported failure".to_string()),
            },
        )
        .await
        .unwrap(); // safety: test

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let summary = loop {
            let handle = mgr.get_handle(job_id).await.unwrap(); // safety: test
            if handle.state == ContainerState::Stopped
                && let Some(harness_state) = &handle.harness
                && let Some(summary) = &harness_state.promotion
            {
                break summary.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detached finalization did not finish"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        };
        assert!(summary.skipped);
        assert!(
            summary
                .skip_reason
                .as_deref()
                .unwrap_or("")
                .contains("did not complete successfully")
        );
        assert!(summary.records.is_empty());
    }
}
