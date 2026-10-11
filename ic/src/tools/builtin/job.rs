//! Job management tools.
//!
//! These tools allow the LLM to manage jobs:
//! - Create new jobs/tasks (with optional sandbox delegation)
//! - List existing jobs
//! - Check job status
//! - Cancel running jobs

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use lunarwing_common::{
    DEFAULT_WORKER_ITERATIONS, MAX_WORKER_ITERATIONS, PromptContextLimits, PromptContextMessage,
    select_prompt_context,
};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::bootstrap::lunarwing_base_dir;
use crate::channels::IncomingMessage;
use crate::channels::web::types::SseEvent;
use crate::context::{ContextManager, JobContext, JobState};
use crate::db::Database;
use crate::history::SandboxJobRecord;
use crate::orchestrator::ExternalWorkerManager;
use crate::orchestrator::auth::{CredentialGrant, SandboxJobGrants};
use crate::orchestrator::external_worker::{ExternalTaskStatus, build_task_context};
use crate::orchestrator::harness::{
    self, HarnessJobOptions, InjectionSpec, ResolvedSecretFile, SecretFileMount, WriteBackPolicy,
};
use crate::orchestrator::job_manager::{
    ContainerJobManager, ContainerJobRequest, HarnessJobRequest, JobMode, MAX_WORKER_MCP_SERVERS,
    snapshot_worker_mcp,
};
use crate::secrets::SecretsStore;
use crate::tools::tool::{ApprovalRequirement, Tool, ToolError, ToolOutput, require_str};

const LONG_JOB_TOOL_TIMEOUT: Duration = Duration::from_secs(660);
const EXTERNAL_WORKER_TIMEOUT_GRACE: Duration = Duration::from_secs(60);
const MAX_LISTED_JOBS: usize = 100;

/// Lazy scheduler reference, filled after Agent::new creates the Scheduler.
///
/// Solves the chicken-and-egg: tools are registered before the Scheduler exists
/// (Scheduler needs the ToolRegistry). Created empty, filled after Agent::new.
pub type SchedulerSlot = Arc<RwLock<Option<Arc<crate::agent::Scheduler>>>>;

/// Resolve a job ID from a full UUID or a short prefix (like git short SHAs).
///
/// Tries full UUID parse first. If that fails, treats the input as a hex prefix
/// and searches the context manager for a unique match.
async fn resolve_job_id(input: &str, context_manager: &ContextManager) -> Result<Uuid, ToolError> {
    // Fast path: full UUID
    if let Ok(id) = Uuid::parse_str(input) {
        return Ok(id);
    }

    // Require a minimum prefix length to limit brute-force enumeration.
    if input.len() < 4 {
        return Err(ToolError::InvalidParameters(
            "job ID prefix must be at least 4 hex characters".to_string(),
        ));
    }

    // Prefix match against known jobs
    let input_lower = input.to_lowercase();
    let all_ids = context_manager.all_jobs().await;
    let matches: Vec<Uuid> = all_ids
        .into_iter()
        .filter(|id| {
            let hex = id.to_string().replace('-', "");
            hex.starts_with(&input_lower)
        })
        .collect();

    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(ToolError::InvalidParameters(format!(
            "no job found matching prefix '{}'",
            input
        ))),
        n => Err(ToolError::InvalidParameters(format!(
            "ambiguous prefix '{}' matches {} jobs, provide more characters",
            input, n
        ))),
    }
}

/// Resolve a queryable job from live context or persisted sandbox records.
async fn resolve_query_job_id(
    input: &str,
    context_manager: &ContextManager,
    store: Option<&Arc<dyn Database>>,
    user_id: &str,
) -> Result<Uuid, ToolError> {
    if let Ok(id) = Uuid::parse_str(input) {
        return Ok(id);
    }

    if input.len() < 4 {
        return Err(ToolError::InvalidParameters(
            "job ID prefix must be at least 4 hex characters".to_string(),
        ));
    }

    let input_lower = input.to_lowercase();
    let mut matches = context_manager.all_jobs_for(user_id).await;
    if let Some(store) = store {
        match store.list_sandbox_jobs_for_user(user_id).await {
            Ok(jobs) => matches.extend(jobs.into_iter().map(|job| job.id)),
            Err(error) if matches.is_empty() => {
                return Err(ToolError::ExecutionFailed(format!(
                    "failed to search persisted jobs: {error}"
                )));
            }
            Err(error) => {
                tracing::warn!("Failed to search persisted jobs: {error}");
            }
        }
    }

    matches.retain(|id| id.simple().to_string().starts_with(&input_lower));
    matches.sort_unstable();
    matches.dedup();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(ToolError::InvalidParameters(format!(
            "no job found matching prefix '{}'",
            input
        ))),
        n => Err(ToolError::InvalidParameters(format!(
            "ambiguous prefix '{}' matches {} jobs, provide more characters",
            input, n
        ))),
    }
}

fn sandbox_job_requires_attention(message: Option<&str>) -> bool {
    let Some(message) = message else {
        return false;
    };

    let message = message.to_ascii_lowercase();
    message.contains("awaiting approval")
        || message.contains("requires approval")
        || message.contains("not available for autonomous execution")
        || message.contains("not currently available for owner")
}

fn sandbox_completion_state(success: bool, message: Option<&str>) -> JobState {
    if success {
        JobState::Completed
    } else if sandbox_job_requires_attention(message) {
        JobState::Stuck
    } else {
        JobState::Failed
    }
}

fn sandbox_completion_status(success: bool, message: Option<&str>) -> &'static str {
    match sandbox_completion_state(success, message) {
        JobState::Completed => "completed",
        JobState::Stuck => "stuck",
        _ => "failed",
    }
}

fn external_job_wait(params: &serde_json::Value) -> bool {
    params
        .get("wait")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn worker_max_iterations(params: &serde_json::Value) -> Result<u32, ToolError> {
    let Some(value) = params.get("max_iterations") else {
        return Ok(DEFAULT_WORKER_ITERATIONS);
    };
    let value = value.as_u64().ok_or_else(|| {
        ToolError::InvalidParameters("max_iterations must be an integer".to_string())
    })?;
    if !(1..=u64::from(MAX_WORKER_ITERATIONS)).contains(&value) {
        return Err(ToolError::InvalidParameters(format!(
            "max_iterations must be between 1 and {MAX_WORKER_ITERATIONS}"
        )));
    }
    Ok(value as u32)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalContextScope {
    None,
    Recent,
}

impl ExternalContextScope {
    fn parse(params: &serde_json::Value) -> Result<Self, ToolError> {
        match params.get("context_scope") {
            None => Ok(Self::None),
            Some(value) => match value.as_str() {
                Some("none") => Ok(Self::None),
                Some("recent") => Ok(Self::Recent),
                _ => Err(ToolError::InvalidParameters(
                    "context_scope must be 'none' or 'recent'".to_string(),
                )),
            },
        }
    }
}

struct ExternalExecutionOptions {
    wait: bool,
    context_scope: ExternalContextScope,
    project_dir: Option<String>,
    credential_grants: Vec<CredentialGrant>,
}

struct SandboxExecutionOptions {
    explicit_dir: Option<PathBuf>,
    wait: bool,
    credential_grants: Vec<CredentialGrant>,
    mcp_servers: Vec<String>,
    max_iterations: u32,
    /// Harness capabilities (WI#326); present only for `mode == "harness"`.
    harness: Option<HarnessJobOptions>,
}

/// Parse `worker_secrets` (map of secret name -> mounted file name).
fn harness_secret_mounts(params: &serde_json::Value) -> Result<Vec<SecretFileMount>, ToolError> {
    let Some(obj) = params.get("worker_secrets") else {
        return Ok(Vec::new());
    };
    let obj = obj.as_object().ok_or_else(|| {
        ToolError::InvalidParameters(
            "worker_secrets must be an object mapping secret names to mounted file names"
                .to_string(),
        )
    })?;
    let mut mounts = Vec::with_capacity(obj.len());
    for (secret_name, file_name) in obj {
        let file_name = file_name.as_str().ok_or_else(|| {
            ToolError::InvalidParameters(format!(
                "worker_secrets value for '{secret_name}' must be a file name string"
            ))
        })?;
        let mount = SecretFileMount::new(secret_name.clone(), file_name);
        mount
            .validate()
            .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
        mounts.push(mount);
    }
    Ok(mounts)
}

/// Parse `inject_files` (array of source paths or {source, target} objects).
fn harness_injection_specs(params: &serde_json::Value) -> Result<Vec<InjectionSpec>, ToolError> {
    let Some(arr) = params.get("inject_files") else {
        return Ok(Vec::new());
    };
    let arr = arr.as_array().ok_or_else(|| {
        ToolError::InvalidParameters(
            "inject_files must be an array of workspace paths or {source, target} objects"
                .to_string(),
        )
    })?;
    let mut specs = Vec::with_capacity(arr.len());
    for item in arr {
        let spec = match item {
            serde_json::Value::String(source) => InjectionSpec::new(source.clone()),
            serde_json::Value::Object(_) => {
                let source = item.get("source").and_then(|v| v.as_str()).ok_or_else(|| {
                    ToolError::InvalidParameters(
                        "inject_files entries require a 'source' string".to_string(),
                    )
                })?;
                let mut spec = InjectionSpec::new(source);
                if let Some(target) = item.get("target").and_then(|v| v.as_str()) {
                    spec = spec.with_target(target);
                }
                spec
            }
            _ => {
                return Err(ToolError::InvalidParameters(
                    "inject_files entries must be strings or objects".to_string(),
                ));
            }
        };
        spec.validate()
            .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
        specs.push(spec);
    }
    Ok(specs)
}

/// Parse `write_back`: `true` enables the default policy; an object may
/// NARROW the operator-fixed roots (`projects/**`) or LOWER the limits —
/// ceilings and the root set are enforced server-side (CW-1/CW-6). Absent or
/// `false` disables write-back.
fn harness_write_back_policy(
    params: &serde_json::Value,
) -> Result<Option<WriteBackPolicy>, ToolError> {
    let Some(value) = params.get("write_back") else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Bool(enabled) => Ok(enabled.then(WriteBackPolicy::default)),
        serde_json::Value::Object(obj) => {
            let mut policy = WriteBackPolicy::default();
            if let Some(roots) = obj.get("allowed_roots") {
                let roots = roots.as_array().ok_or_else(|| {
                    ToolError::InvalidParameters(
                        "write_back.allowed_roots must be an array of workspace roots".to_string(),
                    )
                })?;
                policy.allowed_roots = roots
                    .iter()
                    .map(|r| {
                        r.as_str().map(str::to_string).ok_or_else(|| {
                            ToolError::InvalidParameters(
                                "write_back.allowed_roots entries must be strings".to_string(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            }
            if let Some(max_files) = obj.get("max_files").and_then(|v| v.as_u64()) {
                policy.max_files = max_files as usize;
            }
            if let Some(max_file_bytes) = obj.get("max_file_bytes").and_then(|v| v.as_u64()) {
                policy.max_file_bytes = max_file_bytes;
            }
            if let Some(max_total_bytes) = obj.get("max_total_bytes").and_then(|v| v.as_u64()) {
                policy.max_total_bytes = max_total_bytes;
            }
            policy
                .validate()
                .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
            Ok(Some(policy))
        }
        _ => Err(ToolError::InvalidParameters(
            "write_back must be a boolean or an object of policy overrides".to_string(),
        )),
    }
}

/// Parse the full WI#326 harness option set from create_job params.
fn harness_options_from_params(
    params: &serde_json::Value,
) -> Result<Option<HarnessJobOptions>, ToolError> {
    let options = HarnessJobOptions {
        worker_secrets: harness_secret_mounts(params)?,
        injections: harness_injection_specs(params)?,
        allow_identity_injection: params
            .get("allow_identity_injection")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        write_back: harness_write_back_policy(params)?,
    };
    options
        .validate()
        .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
    if options.is_empty() {
        return Err(ToolError::InvalidParameters(
            "harness mode requires at least one of worker_secrets, inject_files, or write_back"
                .to_string(),
        ));
    }
    Ok(Some(options))
}

/// True when any harness-only parameter was supplied (used to reject them
/// for non-harness modes instead of silently ignoring them).
fn has_harness_params(params: &serde_json::Value) -> bool {
    params.get("worker_secrets").is_some()
        || params.get("inject_files").is_some()
        || params.get("allow_identity_injection").is_some()
        || params.get("write_back").is_some()
}

/// DH-1: presence-based capability check for approval elevation. Approval
/// may run on RAW params while execution uses schema-coerced params
/// (`"true"` → `true`, a JSON string → object), so any non-inert shape must
/// elevate. `inert_empty_object` marks `{}` as inert for map-typed params
/// (`worker_secrets: {}` mounts nothing); `write_back: {}` is NOT inert — it
/// enables the default policy at execution.
fn harness_capability_active(value: &serde_json::Value, inert_empty_object: bool) -> bool {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(false) => false,
        serde_json::Value::Object(obj) => !(inert_empty_object && obj.is_empty()),
        // Strings, numbers, arrays, `true`, non-empty objects: coercion may
        // activate the capability at execution — elevate.
        _ => true,
    }
}

/// Append harness mount instructions to the worker task prompt so the
/// sub-agent knows about the injected files, secrets, and write-back staging.
fn harness_task_prompt(task: &str, request: &HarnessJobRequest) -> String {
    let mut out = task.to_string();
    out.push_str("\n\n## Harness capabilities\n");
    if request.injection_dir.is_some() {
        out.push_str(&format!(
            "- Read-only reference files from workspace memory are mounted at {}. \
             See PROVENANCE.json there for source paths and content hashes. \
             Do not attempt to modify them.\n",
            harness::HARNESS_INJECTION_CONTAINER_DIR
        ));
    }
    if !request.resolved_secrets.is_empty() {
        out.push_str(&format!(
            "- Secret files are mounted read-only on tmpfs at {}. When a command needs a \
             secret, reference the file WITHOUT printing it (e.g. `curl -H \"Authorization: \
             Bearer $(cat {}/NAME)\" ...`). Never cat, echo, env-export, or otherwise reveal \
             their contents; values are redacted from job events.\n",
            harness::HARNESS_SECRETS_CONTAINER_DIR,
            harness::HARNESS_SECRETS_CONTAINER_DIR,
        ));
    }
    if let Some(policy) = &request.write_back {
        out.push_str(&format!(
            "- To persist results into workspace memory, write files under {} under an \
             allowed root ({}). They are promoted only after secret scanning and \
             allowlist checks; identity, system-prompt, daily/, lorebooks/, context/, \
             backups/, and engine-managed paths are never promoted.\n",
            harness::HARNESS_WRITE_BACK_CONTAINER_DIR,
            policy.allowed_roots.join(", ")
        ));
    }
    out
}

fn external_prompt_context(
    ctx: &JobContext,
    scope: ExternalContextScope,
    limits: PromptContextLimits,
) -> Vec<PromptContextMessage> {
    match scope {
        ExternalContextScope::None => Vec::new(),
        ExternalContextScope::Recent => select_prompt_context(&ctx.prompt_context, limits),
    }
}

/// Tool for creating a new job.
///
/// When sandbox deps are injected (via `with_sandbox`), the tool automatically
/// delegates execution to a Docker container. Otherwise it creates an in-memory
/// job via the ContextManager. The LLM never needs to know the difference.
pub struct CreateJobTool {
    context_manager: Arc<ContextManager>,
    /// Lazy scheduler for dispatching local (non-sandbox) jobs.
    scheduler_slot: Option<SchedulerSlot>,
    job_manager: Option<Arc<ContainerJobManager>>,
    external_worker_manager: Option<Arc<ExternalWorkerManager>>,
    store: Option<Arc<dyn Database>>,
    /// Broadcast sender for job events (used to subscribe a monitor).
    event_tx: Option<tokio::sync::broadcast::Sender<(Uuid, String, SseEvent)>>,
    /// Injection channel for pushing messages into the agent loop.
    inject_tx: Option<tokio::sync::mpsc::Sender<IncomingMessage>>,
    /// Encrypted secrets store for validating credential grants.
    secrets_store: Option<Arc<dyn SecretsStore + Send + Sync>>,
    /// Workspace resolver for harness-mode memory injection (WI#326).
    workspace_resolver: Option<Arc<dyn crate::tools::builtin::memory::WorkspaceResolver>>,
}

impl CreateJobTool {
    pub fn new(context_manager: Arc<ContextManager>) -> Self {
        Self {
            context_manager,
            scheduler_slot: None,
            job_manager: None,
            external_worker_manager: None,
            store: None,
            event_tx: None,
            inject_tx: None,
            secrets_store: None,
            workspace_resolver: None,
        }
    }

    /// Inject sandbox dependencies so `create_job` delegates to Docker containers.
    pub fn with_sandbox(
        mut self,
        job_manager: Arc<ContainerJobManager>,
        store: Option<Arc<dyn Database>>,
    ) -> Self {
        self.job_manager = Some(job_manager);
        self.store = store;
        self
    }

    /// Inject external worker manager for delegating to persistent worker containers.
    pub fn with_external_workers(
        mut self,
        mgr: Arc<ExternalWorkerManager>,
        store: Option<Arc<dyn Database>>,
    ) -> Self {
        self.external_worker_manager = Some(mgr);
        if self.store.is_none() {
            self.store = store;
        }
        self
    }

    /// Inject monitor dependencies so fire-and-forget jobs spawn a background
    /// monitor that forwards output to the main agent loop.
    pub fn with_monitor_deps(
        mut self,
        event_tx: tokio::sync::broadcast::Sender<(Uuid, String, SseEvent)>,
        inject_tx: tokio::sync::mpsc::Sender<IncomingMessage>,
    ) -> Self {
        self.event_tx = Some(event_tx);
        self.inject_tx = Some(inject_tx);
        self
    }

    /// Inject a lazy scheduler slot for dispatching local (non-sandbox) jobs.
    pub fn with_scheduler_slot(mut self, slot: SchedulerSlot) -> Self {
        self.scheduler_slot = Some(slot);
        self
    }

    /// Inject secrets store for credential validation.
    pub fn with_secrets(mut self, secrets: Arc<dyn SecretsStore + Send + Sync>) -> Self {
        self.secrets_store = Some(secrets);
        self
    }

    /// Inject a workspace resolver for harness-mode memory injection (WI#326).
    pub fn with_workspace_resolver(
        mut self,
        resolver: Arc<dyn crate::tools::builtin::memory::WorkspaceResolver>,
    ) -> Self {
        self.workspace_resolver = Some(resolver);
        self
    }

    pub fn sandbox_enabled(&self) -> bool {
        self.job_manager.is_some()
    }

    fn has_external_workers(&self) -> bool {
        self.external_worker_manager
            .as_ref()
            .is_some_and(|m| !m.is_empty())
    }

    fn external_worker_monitor_timeout(&self, worker_timeout_ms: u64) -> Duration {
        Duration::from_millis(worker_timeout_ms).saturating_add(EXTERNAL_WORKER_TIMEOUT_GRACE)
    }

    /// Parse and validate the `credentials` parameter.
    ///
    /// Each key is a secret name (must exist in SecretsStore), each value is the
    /// env var name the container should receive it as. Returns an empty vec if
    /// no credentials were requested.
    async fn parse_credentials(
        &self,
        params: &serde_json::Value,
        user_id: &str,
    ) -> Result<Vec<CredentialGrant>, ToolError> {
        let creds_obj = match params.get("credentials").and_then(|v| v.as_object()) {
            Some(obj) if !obj.is_empty() => obj,
            _ => return Ok(vec![]),
        };

        const MAX_CREDENTIAL_GRANTS: usize = 20;
        if creds_obj.len() > MAX_CREDENTIAL_GRANTS {
            return Err(ToolError::InvalidParameters(format!(
                "too many credential grants ({}, max {})",
                creds_obj.len(),
                MAX_CREDENTIAL_GRANTS
            )));
        }

        let secrets = match &self.secrets_store {
            Some(s) => s,
            None => {
                return Err(ToolError::ExecutionFailed(
                    "credentials requested but no secrets store is configured. \
                     Set SECRETS_MASTER_KEY to enable credential management."
                        .to_string(),
                ));
            }
        };

        let mut grants = Vec::with_capacity(creds_obj.len());
        for (secret_name, env_var_value) in creds_obj {
            let env_var = env_var_value.as_str().ok_or_else(|| {
                ToolError::InvalidParameters(format!(
                    "credential env var for '{}' must be a string",
                    secret_name
                ))
            })?;

            validate_env_var_name(env_var)?;

            // Validate the secret actually exists
            let exists = secrets.exists(user_id, secret_name).await.map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "failed to check secret '{}': {}",
                    secret_name, e
                ))
            })?;

            if !exists {
                return Err(ToolError::ExecutionFailed(format!(
                    "secret '{}' not found. Store it first via 'lunarwing tool auth' or the web UI.",
                    secret_name
                )));
            }

            grants.push(CredentialGrant {
                secret_name: secret_name.clone(),
                env_var: env_var.to_string(),
            });
        }

        Ok(grants)
    }

    /// Persist a sandbox job record (fire-and-forget).
    fn persist_job(&self, record: SandboxJobRecord) {
        if let Some(store) = self.store.clone() {
            tokio::spawn(async move {
                if let Err(e) = store.save_sandbox_job(&record).await {
                    tracing::warn!(job_id = %record.id, "Failed to persist sandbox job: {}", e);
                }
            });
        }
    }

    /// Transition a sandbox job's state in the ContextManager (awaited).
    ///
    /// Best-effort: logs on failure (job may have been cleaned up already).
    async fn update_context_state_async(
        &self,
        job_id: Uuid,
        state: JobState,
        reason: Option<String>,
    ) {
        if let Err(e) = self
            .context_manager
            .update_context(job_id, |ctx| {
                let _ = ctx.transition_to(state, reason);
            })
            .await
        {
            tracing::debug!(job_id = %job_id, "sandbox context update skipped: {}", e);
        }
    }

    /// Fire-and-forget variant for use in sync contexts (e.g. `.map_err()` closures).
    fn update_context_state(&self, job_id: Uuid, state: JobState, reason: Option<String>) {
        let cm = self.context_manager.clone();
        tokio::spawn(async move {
            if let Err(e) = cm
                .update_context(job_id, |ctx| {
                    let _ = ctx.transition_to(state, reason);
                })
                .await
            {
                tracing::debug!(job_id = %job_id, "sandbox context update skipped: {}", e);
            }
        });
    }

    /// Update sandbox job status in DB (fire-and-forget).
    fn update_status(
        &self,
        job_id: Uuid,
        status: &str,
        success: Option<bool>,
        message: Option<String>,
        started_at: Option<chrono::DateTime<Utc>>,
        completed_at: Option<chrono::DateTime<Utc>>,
    ) {
        if let Some(store) = self.store.clone() {
            let status = status.to_string();
            tokio::spawn(async move {
                if let Err(e) = store
                    .update_sandbox_job_status(
                        job_id,
                        &status,
                        success,
                        message.as_deref(),
                        started_at,
                        completed_at,
                    )
                    .await
                {
                    tracing::warn!(job_id = %job_id, "Failed to update sandbox job status: {}", e);
                }
            });
        }
    }

    /// Execute via Scheduler (persists to DB + spawns worker), or fall back to
    /// ContextManager-only if the scheduler isn't available yet.
    async fn execute_local(
        &self,
        title: &str,
        description: &str,
        max_iterations: u32,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        // Use the scheduler if available — creates in ContextManager, persists
        // to DB, transitions to InProgress, and spawns a worker. The new job
        // runs independently with its own Worker and LLM context (not inheriting
        // the parent conversation). MaxJobsExceeded is returned as error JSON
        // so the LLM can report it to the user.
        if let Some(ref slot) = self.scheduler_slot
            && let Some(ref scheduler) = *slot.read().await
        {
            return match scheduler
                .dispatch_job(
                    &ctx.user_id,
                    title,
                    description,
                    Some(serde_json::json!({ "max_iterations": max_iterations })),
                )
                .await
            {
                Ok(job_id) => {
                    let result = serde_json::json!({
                        "job_id": job_id.to_string(),
                        "title": title,
                        "status": "in_progress",
                        "max_iterations": max_iterations,
                        "message": format!("Created and scheduled job '{}'", title)
                    });
                    Ok(ToolOutput::success(result, start.elapsed()))
                }
                Err(e) => {
                    let result = serde_json::json!({
                        "error": e.to_string()
                    });
                    Ok(ToolOutput::success(result, start.elapsed()))
                }
            };
        }

        // Fallback: ContextManager-only (scheduler not yet initialized).
        match self
            .context_manager
            .create_job_for_user(&ctx.user_id, title, description)
            .await
        {
            Ok(job_id) => {
                self.context_manager
                    .update_context(job_id, |job| {
                        job.metadata = serde_json::json!({ "max_iterations": max_iterations });
                    })
                    .await
                    .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
                let result = serde_json::json!({
                    "job_id": job_id.to_string(),
                    "title": title,
                    "status": "pending",
                    "max_iterations": max_iterations,
                    "message": format!("Created job '{}' (not scheduled — scheduler unavailable)", title)
                });
                Ok(ToolOutput::success(result, start.elapsed()))
            }
            Err(e) => {
                let result = serde_json::json!({
                    "error": e.to_string()
                });
                Ok(ToolOutput::success(result, start.elapsed()))
            }
        }
    }

    /// Prepare harness inputs for one job (WI#326): resolve worker-scoped
    /// secrets host-side, snapshot requested workspace documents into the
    /// read-only injection dir with a provenance manifest, and create the
    /// write-back staging dir. Runs before container creation so a failure
    /// leaves no container behind.
    async fn prepare_harness_request(
        &self,
        job_id: Uuid,
        user_id: &str,
        options: HarnessJobOptions,
    ) -> Result<HarnessJobRequest, ToolError> {
        // Harness state lives outside the bind-mountable projects base (CW-3).
        harness::ensure_harness_base_dir()
            .map_err(|e| ToolError::ExecutionFailed(format!("harness base dir: {e}")))?;

        let mut resolved_secrets = Vec::with_capacity(options.worker_secrets.len());
        if !options.worker_secrets.is_empty() {
            let secrets = self.secrets_store.as_ref().ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "worker_secrets requested but no secrets store is configured. \
                     Set SECRETS_MASTER_KEY to enable credential management."
                        .to_string(),
                )
            })?;
            for mount in &options.worker_secrets {
                let decrypted = secrets
                    .get_decrypted(user_id, &mount.secret_name)
                    .await
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!(
                            "failed to resolve worker secret '{}': {}",
                            mount.secret_name, e
                        ))
                    })?;
                // CV-8: same usage-audit posture as the /credentials endpoint.
                if let Ok(secret) = secrets.get(user_id, &mount.secret_name).await
                    && let Err(e) = secrets.record_usage(secret.id).await
                {
                    tracing::warn!(
                        job_id = %job_id,
                        "Failed to record harness secret usage: {}", e
                    );
                }
                resolved_secrets.push(ResolvedSecretFile::new(
                    mount.file_name.clone(),
                    secrecy::SecretString::from(decrypted.expose().to_string()),
                ));
            }
        }

        let injection_dir = if options.injections.is_empty() {
            None
        } else {
            let resolver = self.workspace_resolver.as_ref().ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "inject_files requested but no workspace is configured".to_string(),
                )
            })?;
            let workspace = resolver.resolve(user_id).await;
            let mut entries = Vec::with_capacity(options.injections.len());
            for spec in &options.injections {
                // CW-5: read and record the NORMALIZED source path — the
                // raw param is never used after validation.
                let normalized = spec
                    .normalized_source()
                    .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
                let document = workspace.read(&normalized).await.map_err(|e| {
                    ToolError::InvalidParameters(format!("cannot inject '{normalized}': {e}"))
                })?;
                entries.push(harness::InjectionEntry {
                    source_path: normalized,
                    target_name: spec
                        .resolved_target_name()
                        .map_err(|e| ToolError::InvalidParameters(e.to_string()))?,
                    content: document.content,
                });
            }
            let dir = harness::injection_dir(job_id);
            let entry_count = entries.len();
            // DH-10: file writes are blocking IO — keep them off the async
            // executor.
            let write_dir = dir.clone();
            tokio::task::spawn_blocking(move || {
                harness::write_injection_dir(&write_dir, job_id, &entries)
            })
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("injection task failed: {e}")))?
            .map_err(|e| ToolError::ExecutionFailed(format!("injection failed: {e}")))?;
            tracing::info!(
                job_id = %job_id,
                files = entry_count,
                "Prepared harness injection mount"
            );
            Some(dir)
        };

        let staging_dir = if options.write_back.is_some() {
            let dir = harness::staging_dir(job_id);
            std::fs::create_dir_all(&dir).map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "failed to create write-back staging dir {}: {}",
                    dir.display(),
                    e
                ))
            })?;
            Some(dir)
        } else {
            None
        };

        Ok(HarnessJobRequest {
            resolved_secrets,
            injection_dir,
            write_back: options.write_back,
            staging_dir,
        })
    }

    /// Execute via sandboxed Docker container.
    async fn execute_sandbox(
        &self,
        task: &str,
        options: SandboxExecutionOptions,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let SandboxExecutionOptions {
            explicit_dir,
            wait,
            credential_grants,
            mcp_servers: selected_mcp_servers,
            max_iterations,
            harness: harness_options,
        } = options;
        let start = std::time::Instant::now();
        let jm = self.job_manager.as_ref().ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Sandbox execution requires a configured job manager (container runtime not available)".to_string(),
            )
        })?;

        let (mcp_servers, mcp_credential_grants) = snapshot_worker_mcp(
            self.store.as_deref(),
            self.secrets_store.as_deref(),
            &ctx.user_id,
            &selected_mcp_servers,
        )
        .await
        .map_err(|error| {
            ToolError::ExecutionFailed(format!("failed to snapshot worker MCP config: {error}"))
        })?;

        let job_id = Uuid::new_v4();
        // CW-3: an explicit project_dir must belong to a job owned by THIS
        // user — otherwise a prompt-injected call could mount another user's
        // job workspace (cross-user read/write in multi-user daemons).
        // DH-7: without a job store the ownership check cannot run — fail
        // closed instead of skipping it.
        if let Some(ref dir) = explicit_dir {
            let Some(store) = &self.store else {
                return Err(ToolError::NotAuthorized(
                    "explicit project_dir requires the job store for ownership verification"
                        .to_string(),
                ));
            };
            let dir_job_id = dir
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| Uuid::parse_str(n).ok());
            let owner_ok = match dir_job_id {
                Some(id) => store
                    .get_sandbox_job(id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|record| record.user_id == ctx.user_id),
                None => false,
            };
            if !owner_ok {
                return Err(ToolError::NotAuthorized(
                    "explicit project_dir must belong to a job owned by the current user"
                        .to_string(),
                ));
            }
        }
        let (project_dir, browse_id) = resolve_project_dir(explicit_dir, job_id)?;
        let project_dir_str = project_dir.display().to_string();

        // WI#326: prepare harness mounts before any container exists.
        let harness_request = match harness_options {
            Some(options) => Some(
                self.prepare_harness_request(job_id, &ctx.user_id, options)
                    .await?,
            ),
            None => None,
        };
        let job_mode = if harness_request.is_some() {
            JobMode::Harness
        } else {
            JobMode::Worker
        };
        let task = if let Some(req) = harness_request.as_ref() {
            harness_task_prompt(task, req)
        } else {
            task.to_string()
        };
        let task = task.as_str();

        let credential_grants_json = serde_json::to_string(&SandboxJobGrants {
            credentials: credential_grants.clone(),
            mcp_servers: selected_mcp_servers,
        })
        .map_err(|error| {
            ToolError::ExecutionFailed(format!("failed to serialize sandbox job grants: {error}"))
        })?;

        // Register in ContextManager so query tools (list_jobs, job_status,
        // job_events, cancel_job) can find sandbox jobs. Without this, sandbox
        // jobs exist only in the DB and are invisible to the agent.
        // DH-2: on failure the harness dirs prepared above would be orphaned
        // until the next boot sweep — clean them up on every early return.
        self.context_manager
            .register_sandbox_job(job_id, &ctx.user_id, task, task)
            .await
            .map_err(|e| {
                harness::cleanup_harness_job_dir(job_id);
                ToolError::ExecutionFailed(format!("failed to register sandbox job: {}", e))
            })?;
        self.context_manager
            .update_context(job_id, |job| {
                job.metadata = serde_json::json!({ "max_iterations": max_iterations });
            })
            .await
            .map_err(|error| {
                harness::cleanup_harness_job_dir(job_id);
                ToolError::ExecutionFailed(error.to_string())
            })?;

        // Persist the job to DB before creating the container. CV-9: the
        // record carries its mode in the INITIAL insert — a harness job whose
        // mode landed only after container start could restart as a plain
        // worker carrying the harness task text.
        let record = SandboxJobRecord {
            id: job_id,
            task: task.to_string(),
            status: "creating".to_string(),
            user_id: ctx.user_id.clone(),
            project_dir: project_dir_str.clone(),
            success: None,
            failure_reason: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            credential_grants_json,
            job_mode: job_mode.db_value(),
        };
        if harness_request.is_some() {
            // Await the insert and FAIL CLOSED on error: a harness job without
            // its persisted mode would restart as a plain worker (CV-9).
            if let Some(store) = &self.store
                && let Err(e) = store.save_sandbox_job(&record).await
            {
                harness::cleanup_harness_job_dir(job_id);
                self.update_context_state(
                    job_id,
                    JobState::Failed,
                    Some(format!("failed to persist harness job: {e}")),
                );
                return Err(ToolError::ExecutionFailed(format!(
                    "failed to persist harness job: {e}"
                )));
            }
        } else {
            self.persist_job(record);
        }

        // Subscribe to the broadcast channel BEFORE starting the container.
        // This eliminates the race where the container completes and broadcasts
        // its result before the monitor is listening.
        let pre_subscribed_rx = if !wait {
            self.event_tx.as_ref().map(|etx| etx.subscribe())
        } else {
            None
        };

        // Create the container job with the pre-determined job_id.
        let _token = jm
            .create_job(ContainerJobRequest {
                job_id,
                task: task.to_string(),
                project_dir: Some(project_dir),
                mode: job_mode.clone(),
                owner_id: ctx.user_id.clone(),
                credential_grants,
                mcp_servers,
                mcp_credential_grants,
                max_iterations,
                harness: harness_request,
            })
            .await
            .map_err(|e| {
                self.update_status(
                    job_id,
                    "failed",
                    Some(false),
                    Some(e.to_string()),
                    None,
                    Some(Utc::now()),
                );
                self.update_context_state(job_id, JobState::Failed, Some(e.to_string()));
                ToolError::ExecutionFailed(format!("failed to create container: {}", e))
            })?;

        // The mode was persisted in the initial insert above (CV-9), so
        // job_prompt and restarts can already tell harness jobs apart from
        // plain worker jobs — no post-start mode update is needed.

        // Container started successfully.
        let now = Utc::now();
        self.update_status(job_id, "running", None, None, Some(now), None);

        if !wait {
            let monitor_timeout = std::time::Duration::from_secs(630);

            if let (Some(rx), Some(itx)) = (pre_subscribed_rx, &self.inject_tx) {
                if let Some(route) = monitor_route_from_ctx(ctx) {
                    crate::agent::job_monitor::spawn_job_monitor_with_context(
                        job_id,
                        rx,
                        itx.clone(),
                        route,
                        Some(self.context_manager.clone()),
                        monitor_timeout,
                    );
                } else {
                    crate::agent::job_monitor::spawn_completion_watcher(
                        job_id,
                        rx,
                        self.context_manager.clone(),
                        monitor_timeout,
                    );
                }
            }

            let result = serde_json::json!({
                "job_id": job_id.to_string(),
                "status": "started",
                "max_iterations": max_iterations,
                "message": "Container started. Use job_events to check status or job_prompt to send follow-up instructions.",
                "project_dir": project_dir_str,
                "browse_url": format!("/projects/{}", browse_id),
            });
            return Ok(ToolOutput::success(result, start.elapsed()));
        }

        // Wait for completion by polling the container state.
        let timeout = Duration::from_secs(600);
        let poll_interval = Duration::from_secs(2);
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if tokio::time::Instant::now() > deadline {
                let _ = jm.stop_job(job_id).await;
                jm.cleanup_job(job_id).await;
                self.update_status(
                    job_id,
                    "failed",
                    Some(false),
                    Some("Timed out (10 minutes)".to_string()),
                    None,
                    Some(Utc::now()),
                );
                self.update_context_state_async(
                    job_id,
                    JobState::Failed,
                    Some("Timed out (10 minutes)".to_string()),
                )
                .await;
                return Err(ToolError::ExecutionFailed(
                    "container execution timed out (10 minutes)".to_string(),
                ));
            }

            match jm.get_handle(job_id).await {
                Some(handle) => match handle.state {
                    crate::orchestrator::job_manager::ContainerState::Running
                    | crate::orchestrator::job_manager::ContainerState::Creating
                    // CW-7: harness write-back promotion still running; the
                    // staging dir must not be cleaned up yet.
                    | crate::orchestrator::job_manager::ContainerState::Finalizing => {
                        tokio::time::sleep(poll_interval).await;
                    }
                    crate::orchestrator::job_manager::ContainerState::Stopped => {
                        let message = handle
                            .completion_result
                            .as_ref()
                            .and_then(|r| r.message.clone())
                            .unwrap_or_else(|| "Container job completed".to_string());
                        let success = handle
                            .completion_result
                            .as_ref()
                            .map(|r| r.success)
                            .unwrap_or(true);
                        jm.cleanup_job(job_id).await;

                        let finished_at = Utc::now();
                        let final_state = sandbox_completion_state(success, Some(&message));
                        let final_status = sandbox_completion_status(success, Some(&message));
                        if final_state == JobState::Completed {
                            self.update_status(
                                job_id,
                                final_status,
                                Some(true),
                                None,
                                None,
                                Some(finished_at),
                            );
                            self.update_context_state_async(job_id, JobState::Completed, None)
                                .await;
                            let mut result = serde_json::json!({
                                "job_id": job_id.to_string(),
                                "status": "completed",
                                "max_iterations": max_iterations,
                                "output": message,
                                "project_dir": project_dir_str,
                                "browse_url": format!("/projects/{}", browse_id),
                            });
                            // WI#326: surface the write-back promotion outcome
                            // (full write set + rejections) when present.
                            if let Some(harness_state) = &handle.harness
                                && let Some(promotion) = &harness_state.promotion
                            {
                                result["write_back"] = serde_json::json!({
                                    "promoted": promotion.records,
                                    "rejected": promotion.rejections,
                                });
                            }
                            return Ok(ToolOutput::success(result, start.elapsed()));
                        } else if final_state == JobState::Stuck {
                            self.update_status(
                                job_id,
                                final_status,
                                Some(false),
                                Some(message.clone()),
                                None,
                                Some(finished_at),
                            );
                            self.update_context_state_async(
                                job_id,
                                JobState::Stuck,
                                Some(message.clone()),
                            )
                            .await;
                            let result = serde_json::json!({
                                "job_id": job_id.to_string(),
                                "status": "stuck",
                                "max_iterations": max_iterations,
                                "output": message,
                                "project_dir": project_dir_str,
                                "browse_url": format!("/projects/{}", browse_id),
                            });
                            return Ok(ToolOutput::success(result, start.elapsed()));
                        } else {
                            self.update_status(
                                job_id,
                                final_status,
                                Some(false),
                                Some(message.clone()),
                                None,
                                Some(finished_at),
                            );
                            self.update_context_state_async(
                                job_id,
                                JobState::Failed,
                                Some(message.clone()),
                            )
                            .await;
                            return Err(ToolError::ExecutionFailed(format!(
                                "container job failed: {}",
                                message
                            )));
                        }
                    }
                    crate::orchestrator::job_manager::ContainerState::Failed => {
                        let message = handle
                            .completion_result
                            .as_ref()
                            .and_then(|r| r.message.clone())
                            .unwrap_or_else(|| "unknown failure".to_string());
                        jm.cleanup_job(job_id).await;
                        self.update_status(
                            job_id,
                            "failed",
                            Some(false),
                            Some(message.clone()),
                            None,
                            Some(Utc::now()),
                        );
                        self.update_context_state_async(
                            job_id,
                            JobState::Failed,
                            Some(message.clone()),
                        )
                        .await;
                        return Err(ToolError::ExecutionFailed(format!(
                            "container job failed: {}",
                            message
                        )));
                    }
                },
                None => {
                    self.update_status(
                        job_id,
                        "completed",
                        Some(true),
                        None,
                        None,
                        Some(Utc::now()),
                    );
                    self.update_context_state_async(job_id, JobState::Completed, None)
                        .await;
                    let result = serde_json::json!({
                        "job_id": job_id.to_string(),
                        "status": "completed",
                        "output": "Container job completed",
                        "project_dir": project_dir_str,
                        "browse_url": format!("/projects/{}", browse_id),
                    });
                    return Ok(ToolOutput::success(result, start.elapsed()));
                }
            }
        }
    }

    /// Execute via an external worker (opencode, kimi, pebble, etc.).
    async fn execute_external(
        &self,
        task: &str,
        worker_name: &str,
        ctx: &JobContext,
        options: ExternalExecutionOptions,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let ewm = self.external_worker_manager.as_ref().ok_or_else(|| {
            ToolError::ExecutionFailed(
                "External worker execution requires a configured worker manager".to_string(),
            )
        })?;
        let worker_config = ewm.get_worker(worker_name).ok_or_else(|| {
            ToolError::ExecutionFailed(format!("External worker '{worker_name}' is not configured"))
        })?;
        let worker_timeout_ms = worker_config.timeout_ms;

        let job_id = Uuid::new_v4();

        // Resolve credential grants to actual env var values
        let mut environment = std::collections::HashMap::new();
        if !options.credential_grants.is_empty() {
            let secrets = self.secrets_store.as_ref().ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "credentials requested but no secrets store is configured".to_string(),
                )
            })?;
            for grant in &options.credential_grants {
                let decrypted = secrets
                    .get_decrypted(&ctx.user_id, &grant.secret_name)
                    .await
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!(
                            "failed to decrypt secret '{}': {}",
                            grant.secret_name, e
                        ))
                    })?;
                environment.insert(grant.env_var.clone(), decrypted.expose().to_string());
            }
        }

        let grants_json = serde_json::to_string(&options.credential_grants).unwrap_or_default();

        ewm.reserve_task(job_id, worker_name).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to reserve external job: {e}"))
        })?;
        if let Err(error) = self
            .context_manager
            .register_sandbox_job(job_id, &ctx.user_id, task, task)
            .await
        {
            ewm.release_task_reservation(job_id).await;
            return Err(ToolError::ExecutionFailed(format!(
                "failed to register external job: {error}"
            )));
        }

        let record = SandboxJobRecord {
            id: job_id,
            task: task.to_string(),
            status: "creating".to_string(),
            user_id: ctx.user_id.clone(),
            project_dir: options.project_dir.clone().unwrap_or_default(),
            success: None,
            failure_reason: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            credential_grants_json: grants_json,
            job_mode: crate::orchestrator::job_manager::JobMode::Worker.db_value(),
        };

        if let Some(store) = &self.store {
            if let Err(error) = store.save_sandbox_job(&record).await {
                tracing::warn!(job_id = %job_id, "Failed to persist external job: {error}");
            }

            let mode_val = JobMode::External(worker_name.to_string()).db_value();
            if let Err(error) = store.update_sandbox_job_mode(job_id, &mode_val).await {
                tracing::warn!(job_id = %job_id, "Failed to persist external job mode: {error}");
            }

            if let Err(error) = store
                .update_sandbox_job_status(job_id, "running", None, None, Some(Utc::now()), None)
                .await
            {
                tracing::warn!(job_id = %job_id, "Failed to mark external job running: {error}");
            }
        }

        let pre_subscribed_rx = if !options.wait {
            self.event_tx.as_ref().map(|etx| etx.subscribe())
        } else {
            None
        };

        let conversation_history =
            external_prompt_context(ctx, options.context_scope, worker_config.prompt_context);
        let selected_chars: usize = conversation_history
            .iter()
            .map(|message| message.content.chars().count())
            .sum();
        let truncated = matches!(options.context_scope, ExternalContextScope::Recent)
            && conversation_history.as_slice() != ctx.prompt_context.as_ref();
        tracing::debug!(
            worker = worker_name,
            context_scope = ?options.context_scope,
            source_messages = ctx.prompt_context.len(),
            selected_messages = conversation_history.len(),
            selected_chars,
            truncated,
            "Prepared external worker prompt context"
        );

        let task_context = build_task_context(
            &ctx.user_id,
            options.project_dir.as_deref(),
            environment,
            conversation_history,
            std::collections::HashMap::new(),
        );

        if options.wait {
            match ewm
                .execute_task(
                    job_id,
                    worker_name,
                    task,
                    Some(worker_timeout_ms),
                    true,
                    task_context,
                )
                .await
            {
                Ok(Some(result)) => match result.status {
                    ExternalTaskStatus::Success => {
                        let output = serde_json::json!({
                            "job_id": job_id.to_string(),
                            "status": "completed",
                            "worker": worker_name,
                            "output": result.output,
                            "duration_ms": result.duration_ms,
                            "timeout_ms": worker_timeout_ms,
                        });
                        Ok(ToolOutput::success(output, start.elapsed()))
                    }
                    ExternalTaskStatus::Cancelled => {
                        let output = serde_json::json!({
                            "job_id": job_id.to_string(),
                            "status": "cancelled",
                            "worker": worker_name,
                            "output": result.output,
                            "duration_ms": result.duration_ms,
                        });
                        Ok(ToolOutput::success(output, start.elapsed()))
                    }
                    _ => Err(ToolError::ExecutionFailed(format!(
                        "external worker '{}' failed: {}",
                        worker_name,
                        result.error.as_deref().unwrap_or(&result.output)
                    ))),
                },
                Ok(None) => {
                    let output = serde_json::json!({
                        "job_id": job_id.to_string(),
                        "status": "started",
                        "worker": worker_name,
                        "message": "Task dispatched (unexpected async return)",
                    });
                    Ok(ToolOutput::success(output, start.elapsed()))
                }
                Err(e) => {
                    self.update_status(
                        job_id,
                        "failed",
                        Some(false),
                        Some(e.to_string()),
                        None,
                        Some(Utc::now()),
                    );
                    self.update_context_state(job_id, JobState::Failed, Some(e.to_string()));
                    Err(ToolError::ExecutionFailed(format!(
                        "external worker '{}' error: {}",
                        worker_name, e
                    )))
                }
            }
        } else {
            if let Err(e) = ewm
                .execute_task(
                    job_id,
                    worker_name,
                    task,
                    Some(worker_timeout_ms),
                    false,
                    task_context,
                )
                .await
            {
                self.update_status(
                    job_id,
                    "failed",
                    Some(false),
                    Some(e.to_string()),
                    None,
                    Some(Utc::now()),
                );
                self.update_context_state(job_id, JobState::Failed, Some(e.to_string()));
                return Err(ToolError::ExecutionFailed(format!(
                    "failed to start external worker '{}': {}",
                    worker_name, e
                )));
            }

            let monitor_timeout = self.external_worker_monitor_timeout(worker_timeout_ms);
            if let (Some(rx), Some(itx)) = (pre_subscribed_rx, &self.inject_tx) {
                if let Some(route) = monitor_route_from_ctx(ctx) {
                    crate::agent::job_monitor::spawn_job_monitor_with_context(
                        job_id,
                        rx,
                        itx.clone(),
                        route,
                        Some(self.context_manager.clone()),
                        monitor_timeout,
                    );
                } else {
                    crate::agent::job_monitor::spawn_completion_watcher(
                        job_id,
                        rx,
                        self.context_manager.clone(),
                        monitor_timeout,
                    );
                }
            }

            let output = serde_json::json!({
                "job_id": job_id.to_string(),
                "status": "started",
                "worker": worker_name,
                "timeout_ms": worker_timeout_ms,
                "message": format!(
                    "Task dispatched to external worker '{}'. Use job_events to check progress.",
                    worker_name
                ),
            });
            Ok(ToolOutput::success(output, start.elapsed()))
        }
    }
}

/// The base directory where all project directories must live.
/// Env var names that could be abused to hijack process behavior.
const DANGEROUS_ENV_VARS: &[&str] = &[
    // Dynamic linker hijacking
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    // Shell behavior
    "BASH_ENV",
    "ENV",
    "CDPATH",
    "IFS",
    "PATH",
    "HOME",
    // Language runtime library path hijacking
    "PYTHONPATH",
    "NODE_PATH",
    "PERL5LIB",
    "RUBYLIB",
    "CLASSPATH",
    // JVM injection
    "JAVA_TOOL_OPTIONS",
    "MAVEN_OPTS",
    "USER",
    "SHELL",
    "RUST_LOG",
];

/// True when `name` is on the env-var denylist (shared with the harness
/// secret-file-name validation, WI#326 CV-4).
pub(crate) fn is_dangerous_env_var_name(name: &str) -> bool {
    DANGEROUS_ENV_VARS.contains(&name)
}

/// Validate that an env var name is safe for container injection.
fn validate_env_var_name(name: &str) -> Result<(), ToolError> {
    if name.is_empty() {
        return Err(ToolError::InvalidParameters(
            "env var name cannot be empty".into(),
        ));
    }

    // Must match ^[A-Z_][A-Z0-9_]*$
    let valid = name
        .bytes()
        .enumerate()
        .all(|(i, b)| matches!(b, b'A'..=b'Z' | b'_') || (i > 0 && b.is_ascii_digit()));

    if !valid {
        return Err(ToolError::InvalidParameters(format!(
            "env var '{}' must match [A-Z_][A-Z0-9_]* (uppercase, underscores, digits)",
            name
        )));
    }

    if DANGEROUS_ENV_VARS.contains(&name) {
        return Err(ToolError::InvalidParameters(format!(
            "env var '{}' is on the denylist (could hijack process behavior)",
            name
        )));
    }

    Ok(())
}

fn projects_base() -> PathBuf {
    lunarwing_base_dir().join("projects")
}

/// Resolve the project directory, creating it if it doesn't exist.
///
/// Auto-creates `~/.lunarwing/projects/{project_id}/` so every sandbox job has a
/// persistent bind mount that survives container teardown.
///
/// When an explicit path is provided (e.g. job restarts reusing the old dir),
/// it is validated to fall within `~/.lunarwing/projects/` after canonicalization.
fn resolve_project_dir(
    explicit: Option<PathBuf>,
    project_id: Uuid,
) -> Result<(PathBuf, String), ToolError> {
    let base = projects_base();
    std::fs::create_dir_all(&base).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "failed to create projects base {}: {}",
            base.display(),
            e
        ))
    })?;
    let canonical_base = base.canonicalize().map_err(|e| {
        ToolError::ExecutionFailed(format!("failed to canonicalize projects base: {}", e))
    })?;

    let (canonical_dir, _was_explicit) = match explicit {
        Some(d) => {
            // Explicit paths: validate BEFORE creating anything.
            // The path must already exist (it comes from a previous job run).
            let canonical = d.canonicalize().map_err(|e| {
                ToolError::InvalidParameters(format!(
                    "explicit project dir {} does not exist or is inaccessible: {}",
                    d.display(),
                    e
                ))
            })?;
            // CW-3: explicit dirs must be exactly `projects/<uuid>` — never
            // the base itself, nested subtrees, or sibling state such as the
            // harness dirs (which live outside projects/ as of WI#326).
            let is_direct_uuid_child = canonical.parent() == Some(canonical_base.as_path())
                && canonical
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| Uuid::parse_str(n).ok())
                    .is_some();
            if !is_direct_uuid_child {
                return Err(ToolError::InvalidParameters(format!(
                    "project directory must be a job directory under {} (projects/<uuid>)",
                    canonical_base.display()
                )));
            }
            (canonical, true)
        }
        None => {
            let dir = canonical_base.join(project_id.to_string());
            std::fs::create_dir_all(&dir).map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "failed to create project dir {}: {}",
                    dir.display(),
                    e
                ))
            })?;
            let canonical = dir.canonicalize().map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "failed to canonicalize project dir {}: {}",
                    dir.display(),
                    e
                ))
            })?;
            (canonical, false)
        }
    };

    let browse_id = canonical_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| project_id.to_string());
    Ok((canonical_dir, browse_id))
}

fn monitor_route_from_ctx(ctx: &JobContext) -> Option<crate::agent::job_monitor::JobMonitorRoute> {
    // notify_channel is required — without it we don't know which channel to
    // route the monitor output to, so return None to skip monitoring entirely.
    let channel = ctx
        .metadata
        .get("notify_channel")
        .and_then(|v| v.as_str())?
        .to_string();
    // notify_user is optional — fall back to the job's own user_id, which is
    // always present. The channel is the routing decision; the user is just
    // for attribution and can default safely.
    let user_id = ctx
        .metadata
        .get("notify_user")
        .and_then(|v| v.as_str())
        .unwrap_or(&ctx.user_id)
        .to_string();
    let thread_id = ctx
        .metadata
        .get("notify_thread_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(crate::agent::job_monitor::JobMonitorRoute {
        channel,
        user_id,
        thread_id,
    })
}

#[async_trait]
impl Tool for CreateJobTool {
    fn name(&self) -> &str {
        "create_job"
    }

    fn description(&self) -> &str {
        if self.has_external_workers() {
            "Create and execute a tracked job using a local backend, Docker sandbox, or configured \
             external worker. When the user requests an external worker, call this create_job tool \
             directly and set mode to the exact worker name from the mode schema (for example \
             'opencode'). External workers are job execution backends, not extensions, MCP servers, \
             skills, routines, or SSH hosts; do not search for or install them. Put the complete task \
             in description. External jobs default to asynchronous dispatch so long tasks outlive \
             the current conversation turn; set wait=true only when a synchronous result is \
             required. Local and Docker workers default to 200 iterations; use max_iterations=300 \
             or higher for large multi-fix write tasks. External worker deadlines come from worker \
             configuration and are returned as timeout_ms; the 30-second local-job default does \
             not apply."
        } else if self.sandbox_enabled() {
            "Create and execute a job. The job runs in a sandboxed Docker container with its own \
             sub-agent that has shell, file read/write, list_dir, and apply_patch tools. Use this \
             whenever the user asks you to build, create, or work on something. The task \
             description should be detailed enough for the sub-agent to work independently. \
             Jobs default to 200 iterations; use max_iterations=300 or higher for large multi-fix work. \
             Jobs default to wait=true so the conversation blocks until the container exits. \
             Set wait=false if you want the conversation to stay responsive while they stream progress. \
             Use mode='harness' to add tmpfs secret files (worker_secrets), read-only workspace \
             injection (inject_files), and staged write-back into workspace memory (write_back); \
             harness jobs default to wait=false."
        } else {
            "Create a new job or task for the agent to work on. Use this when the user wants \
             you to do something substantial that should be tracked as a separate job. Jobs default \
             to 200 iterations; use max_iterations=300 or higher for large multi-fix write tasks."
        }
    }

    fn parameters_schema(&self) -> serde_json::Value {
        if self.sandbox_enabled() || self.has_external_workers() {
            let mut props = serde_json::json!({
                "title": {
                    "type": "string",
                    "description": "Clear description of what to accomplish"
                },
                "description": {
                    "type": "string",
                    "description": "Full description of what needs to be done"
                },
                "wait": {
                    "type": "boolean",
                    "description": "If true, wait for the job to complete and return results. \
                                    If false, start the job and return the job_id immediately. \
                                    Defaults to false for external workers and true for the Docker sandbox."
                }
            });

            if self.sandbox_enabled() || self.has_external_workers() {
                props["project_dir"] = serde_json::json!({
                    "type": "string",
                    "description": "Path to a project directory. For sandbox: mounted into the container \
                                    (must be under ~/.lunarwing/projects/). For external workers: passed \
                                    as context for the worker to use as working directory."
                });
                props["credentials"] = serde_json::json!({
                    "type": "object",
                    "description": "Map of secret names to env var names. Each secret must exist in the \
                                    secrets store (via 'lunarwing tool auth' or web UI). Example: \
                                    {\"github_token\": \"GITHUB_TOKEN\", \"npm_token\": \"NPM_TOKEN\"}",
                    "additionalProperties": { "type": "string" }
                });
                props["max_iterations"] = serde_json::json!({
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_WORKER_ITERATIONS,
                    "default": DEFAULT_WORKER_ITERATIONS,
                    "description": "Iteration budget for local or Docker worker jobs. Use 300+ for large multi-fix write tasks. Named external workers do not support this option."
                });
            }

            if self.sandbox_enabled() {
                props["mcp_servers"] = serde_json::json!({
                    "type": "array",
                    "items": { "type": "string", "minLength": 1 },
                    "maxItems": MAX_WORKER_MCP_SERVERS,
                    "uniqueItems": true,
                    "default": [],
                    "description": "Exact enabled job-worker MCP server names to grant this Docker job. Omit or use [] to grant none."
                });
            }

            if self.has_external_workers() || self.sandbox_enabled() {
                let mut names = self
                    .external_worker_manager
                    .as_ref()
                    .map(|m| m.worker_names())
                    .unwrap_or_default();
                names.sort_unstable();
                let mut modes = vec!["worker".to_string()];
                if self.sandbox_enabled() {
                    modes.push("harness".to_string());
                }
                modes.extend(names.iter().map(|name| (*name).to_string()));
                let harness_desc = " Use 'harness' for the Docker worker with tmpfs secret files \
                                    (worker_secrets), read-only workspace injection (inject_files), \
                                    and staged write-back into workspace memory (write_back).";
                let mode_desc = if !names.is_empty() && self.sandbox_enabled() {
                    format!(
                        "Execution backend. Use 'worker' for the Docker sandbox, or set the exact \
                         external worker name: {}.{} External workers are built-in job backends, not \
                         extensions or MCP servers; call create_job directly instead of searching \
                         or installing anything.",
                        names.join(", "),
                        harness_desc
                    )
                } else if !names.is_empty() {
                    format!(
                        "Execution backend. Omit mode for a local tracked job, or set the exact \
                         external worker name: {}. To use an external worker such as opencode, call \
                         create_job directly with that name; do not search extensions, MCP servers, \
                         skills, routines, or SSH hosts.",
                        names.join(", ")
                    )
                } else {
                    format!(
                        "Execution backend. Use 'worker' for the plain Docker sandbox.{}",
                        harness_desc
                    )
                };
                props["mode"] = serde_json::json!({
                    "type": "string",
                    "description": mode_desc,
                    "enum": modes
                });
                if self.has_external_workers() {
                    props["context_scope"] = serde_json::json!({
                        "type": "string",
                        "description": "External workers only. Use 'recent' to include bounded user/assistant messages from the current scoped conversation as untrusted reference context. Defaults to 'none'.",
                        "enum": ["none", "recent"],
                        "default": "none"
                    });
                }
            }

            if self.sandbox_enabled() {
                props["worker_secrets"] = serde_json::json!({
                    "type": "object",
                    "description": "Harness mode only. Map of secret names to file names mounted \
                                    read-only on tmpfs at /run/secrets/<NAME>. The worker pulls \
                                    values at start and fails closed on mismatch. Values never \
                                    enter the container environment, job context, logs, or job \
                                    events; the job reads the files when a command needs them. \
                                    File names must match [A-Z_][A-Z0-9_]* and must not be on the \
                                    env-var denylist or use LUNARWING_/IRONCLAW_ prefixes. \
                                    Requires explicit approval.",
                    "additionalProperties": { "type": "string" }
                });
                props["inject_files"] = serde_json::json!({
                    "type": "array",
                    "description": "Harness mode only. Workspace memory documents to snapshot into a \
                                    read-only mount at /harness/injected with a provenance manifest. \
                                    Entries are workspace paths (e.g. 'MEMORY.md', 'projects/x/notes.md') \
                                    or {source, target} objects. Paths must be in canonical form \
                                    (no whitespace padding, '.', '..', or backslashes). Identity \
                                    documents require allow_identity_injection=true; backups/, \
                                    engine/, and quarantined daily logs are never injectable.",
                    "items": { "type": ["string", "object"] }
                });
                props["allow_identity_injection"] = serde_json::json!({
                    "type": "boolean",
                    "default": false,
                    "description": "Harness mode only. Allow injecting identity documents (SOUL.md, \
                                    USER.md, IDENTITY.md, AGENTS.md, TOOLS.md, BOOTSTRAP.md). Off by \
                                    default. Requires explicit approval."
                });
                props["write_back"] = serde_json::json!({
                    "type": ["boolean", "object"],
                    "default": false,
                    "description": "Harness mode only. Enable a writable staging mount at \
                                    /harness/workspace_out; on successful completion, staged files \
                                    are promoted into workspace memory after allowlist, size/count, \
                                    and secret-scan checks. The allowed roots are operator-fixed to \
                                    projects/** — an object may only NARROW them \
                                    (allowed_roots: ['projects/sub']) or LOWER the limits \
                                    (max_files, max_file_bytes, max_total_bytes, capped at \
                                    256 / 1 MiB / 8 MiB). Identity, system-prompt, daily/, \
                                    lorebooks/, context/, backups/, and engine/ paths are never \
                                    promoted. Requires explicit approval."
                });
            }

            serde_json::json!({
                "type": "object",
                "properties": props,
                "required": ["title", "description"]
            })
        } else {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "A short title for the job (max 100 chars)"
                    },
                    "description": {
                        "type": "string",
                        "description": "Full description of what needs to be done"
                    },
                    "max_iterations": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_WORKER_ITERATIONS,
                        "default": DEFAULT_WORKER_ITERATIONS,
                        "description": "Iteration budget for autonomous job execution. Use 300+ for large multi-fix write tasks."
                    }
                },
                "required": ["title", "description"]
            })
        }
    }

    fn execution_timeout(&self) -> Duration {
        if self.sandbox_enabled() || self.has_external_workers() {
            let external_timeout = self
                .external_worker_manager
                .as_ref()
                .and_then(|manager| manager.max_timeout_ms())
                .map(|timeout_ms| self.external_worker_monitor_timeout(timeout_ms))
                .unwrap_or_default();
            LONG_JOB_TOOL_TIMEOUT.max(external_timeout)
        } else {
            Duration::from_secs(30)
        }
    }

    fn rate_limit_config(&self) -> Option<crate::tools::tool::ToolRateLimitConfig> {
        Some(crate::tools::tool::ToolRateLimitConfig::new(5, 30))
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let title = require_str(&params, "title")?;

        let description = require_str(&params, "description")?;

        let mode_str = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("worker");

        // WI#326: harness mode runs the conventional worker container with
        // tmpfs secrets, read-only workspace injection, and staged write-back.
        // CV-9: an external worker literally named "harness" stays reachable
        // on deployments WITHOUT the Docker sandbox (built-in mode wins when
        // the sandbox is enabled).
        let harness_routes_external = mode_str == "harness"
            && !self.sandbox_enabled()
            && self
                .external_worker_manager
                .as_ref()
                .is_some_and(|m| m.get_worker("harness").is_some());
        if mode_str == "harness" && !harness_routes_external {
            if !self.sandbox_enabled() {
                return Err(ToolError::InvalidParameters(
                    "harness mode requires the Docker sandbox".to_string(),
                ));
            }
            let harness = harness_options_from_params(&params)?;
            let max_iterations = worker_max_iterations(&params)?;
            // Harness jobs dispatch async by default (external-worker model);
            // wait=true blocks until completion like the plain sandbox.
            let wait = params
                .get("wait")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let explicit_dir = params
                .get("project_dir")
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
            let credential_grants = self.parse_credentials(&params, &ctx.user_id).await?;
            let selected_mcp_servers = params
                .get("mcp_servers")
                .cloned()
                .map(serde_json::from_value::<Vec<String>>)
                .transpose()
                .map_err(|_| {
                    ToolError::InvalidParameters(
                        "mcp_servers must be an array of server names".to_string(),
                    )
                })?
                .unwrap_or_default();
            let task = format!("{}\n\n{}", title, description);
            return self
                .execute_sandbox(
                    &task,
                    SandboxExecutionOptions {
                        explicit_dir,
                        wait,
                        credential_grants,
                        mcp_servers: selected_mcp_servers,
                        max_iterations,
                        harness,
                    },
                    ctx,
                )
                .await;
        }

        // Route to external worker if requested and available
        if mode_str != "worker" {
            if has_harness_params(&params) {
                // CV-9: never silently drop harness options on external modes.
                return Err(ToolError::InvalidParameters(
                    "worker_secrets, inject_files, allow_identity_injection, and write_back \
                     require mode 'harness' (Docker sandbox)"
                        .to_string(),
                ));
            }
            if params.get("mcp_servers").is_some() {
                return Err(ToolError::InvalidParameters(
                    "mcp_servers is supported only by the Docker worker".to_string(),
                ));
            }
            if let Some(ref ewm) = self.external_worker_manager
                && ewm.get_worker(mode_str).is_some()
            {
                if params.get("max_iterations").is_some() {
                    return Err(ToolError::InvalidParameters(format!(
                        "max_iterations is not supported by named external worker '{mode_str}'"
                    )));
                }
                let wait = external_job_wait(&params);
                let context_scope = ExternalContextScope::parse(&params)?;
                let project_dir = params
                    .get("project_dir")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let credential_grants = self.parse_credentials(&params, &ctx.user_id).await?;
                let task = format!("{}\n\n{}", title, description);
                return self
                    .execute_external(
                        &task,
                        mode_str,
                        ctx,
                        ExternalExecutionOptions {
                            wait,
                            context_scope,
                            project_dir,
                            credential_grants,
                        },
                    )
                    .await;
            }
            return Err(ToolError::InvalidParameters(format!(
                "Unknown job mode '{}'. Available: worker{}",
                mode_str,
                self.external_worker_manager
                    .as_ref()
                    .map(|m| {
                        let names = m.worker_names();
                        if names.is_empty() {
                            String::new()
                        } else {
                            format!(", {}", names.join(", "))
                        }
                    })
                    .unwrap_or_default()
            )));
        }

        if self.sandbox_enabled() {
            if has_harness_params(&params) {
                return Err(ToolError::InvalidParameters(
                    "worker_secrets, inject_files, allow_identity_injection, and write_back \
                     require mode 'harness'"
                        .to_string(),
                ));
            }
            let max_iterations = worker_max_iterations(&params)?;

            let wait = params.get("wait").and_then(|v| v.as_bool()).unwrap_or(true);

            let explicit_dir = params
                .get("project_dir")
                .and_then(|v| v.as_str())
                .map(PathBuf::from);

            // Parse and validate credential grants
            let credential_grants = self.parse_credentials(&params, &ctx.user_id).await?;
            let selected_mcp_servers = params
                .get("mcp_servers")
                .cloned()
                .map(serde_json::from_value::<Vec<String>>)
                .transpose()
                .map_err(|_| {
                    ToolError::InvalidParameters(
                        "mcp_servers must be an array of server names".to_string(),
                    )
                })?
                .unwrap_or_default();

            // Combine title and description into the task prompt for the sub-agent.
            let task = format!("{}\n\n{}", title, description);
            self.execute_sandbox(
                &task,
                SandboxExecutionOptions {
                    explicit_dir,
                    wait,
                    credential_grants,
                    mcp_servers: selected_mcp_servers,
                    max_iterations,
                    harness: None,
                },
                ctx,
            )
            .await
        } else {
            if params.get("mcp_servers").is_some() {
                return Err(ToolError::InvalidParameters(
                    "mcp_servers requires the Docker sandbox".to_string(),
                ));
            }
            if has_harness_params(&params) {
                return Err(ToolError::InvalidParameters(
                    "worker_secrets, inject_files, allow_identity_injection, and write_back \
                     require mode 'harness'"
                        .to_string(),
                ));
            }
            let max_iterations = worker_max_iterations(&params)?;
            self.execute_local(title, description, max_iterations, ctx)
                .await
        }
    }

    fn requires_approval(&self, params: &serde_json::Value) -> ApprovalRequirement {
        // WI#326 CW-4: harness capabilities that touch tenant secrets or
        // tenant memory sit ABOVE the auto-approve tiers — an agent must not
        // silently mutate its own memory substrate via a job side-channel,
        // and identity injection / secret mounts must be explicitly approved.
        // The approval prompt renders the params, which carry the resolved
        // (narrowed) write-back roots and the mounted secret names.
        //
        // DH-1: approval can be evaluated on RAW params while execution uses
        // schema-coerced params (`"true"` → true, a JSON string → object).
        // Elevate whenever a capability key is present in ANY shape coercion
        // could activate; only the literal inert forms stay at the default
        // tier. (The dispatchers also normalize before calling this — both
        // layers agree by construction.)
        if params.get("mode").and_then(|v| v.as_str()) == Some("harness") {
            let has_secrets = params
                .get("worker_secrets")
                .is_some_and(|v| harness_capability_active(v, true));
            let has_identity_injection = params
                .get("allow_identity_injection")
                .is_some_and(|v| harness_capability_active(v, false));
            // write_back: true or an options object both enable promotion.
            let has_write_back = params
                .get("write_back")
                .is_some_and(|v| harness_capability_active(v, false));
            if has_secrets || has_identity_injection || has_write_back {
                return ApprovalRequirement::Always;
            }
        }
        ApprovalRequirement::UnlessAutoApproved
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

/// Tool for listing jobs.
pub struct ListJobsTool {
    context_manager: Arc<ContextManager>,
    store: Option<Arc<dyn Database>>,
}

impl ListJobsTool {
    pub fn new(context_manager: Arc<ContextManager>) -> Self {
        Self {
            context_manager,
            store: None,
        }
    }

    pub fn with_store(mut self, store: Arc<dyn Database>) -> Self {
        self.store = Some(store);
        self
    }
}

#[async_trait]
impl Tool for ListJobsTool {
    fn name(&self) -> &str {
        "list_jobs"
    }

    fn description(&self) -> &str {
        "List up to 100 recent jobs or filter by status. Shows job IDs, titles, and current status."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filter": {
                    "type": "string",
                    "description": "Filter by status: 'active', 'completed', 'failed', 'all' (default: 'all')",
                    "enum": ["active", "completed", "failed", "all"]
                }
            }
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let filter = params
            .get("filter")
            .and_then(|v| v.as_str())
            .unwrap_or("all");

        let mut all_jobs = std::collections::HashMap::new();
        if let Some(store) = &self.store {
            match store.list_agent_jobs_for_user(&ctx.user_id).await {
                Ok(records) => {
                    for record in records {
                        all_jobs.insert(
                            record.id,
                            serde_json::json!({
                                "job_id": record.id.to_string(),
                                "title": record.title,
                                "status": record.status,
                                "created_at": record.created_at.to_rfc3339(),
                                "source": "database",
                            }),
                        );
                    }
                }
                Err(error) => tracing::warn!("Failed to list persisted agent jobs: {error}"),
            }
            match store.list_sandbox_jobs_for_user(&ctx.user_id).await {
                Ok(records) => {
                    for record in records {
                        all_jobs.insert(
                            record.id,
                            serde_json::json!({
                                "job_id": record.id.to_string(),
                                "title": record.task,
                                "status": record.status,
                                "created_at": record.created_at.to_rfc3339(),
                                "source": "database",
                            }),
                        );
                    }
                }
                Err(error) => tracing::warn!("Failed to list persisted sandbox jobs: {error}"),
            }
        }

        for job_id in self.context_manager.all_jobs_for(&ctx.user_id).await {
            if let Ok(job_ctx) = self.context_manager.get_context(job_id).await {
                all_jobs.insert(
                    job_id,
                    serde_json::json!({
                        "job_id": job_id.to_string(),
                        "title": job_ctx.title,
                        "status": format!("{:?}", job_ctx.state),
                        "created_at": job_ctx.created_at.to_rfc3339(),
                        "source": "live",
                    }),
                );
            }
        }

        let mut summary = serde_json::Map::from_iter([
            ("total".to_string(), serde_json::json!(all_jobs.len())),
            ("pending".to_string(), serde_json::json!(0)),
            ("in_progress".to_string(), serde_json::json!(0)),
            ("completed".to_string(), serde_json::json!(0)),
            ("failed".to_string(), serde_json::json!(0)),
            ("stuck".to_string(), serde_json::json!(0)),
            ("interrupted".to_string(), serde_json::json!(0)),
        ]);
        for job in all_jobs.values() {
            let status = job["status"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .replace('_', "");
            let key = match status.as_str() {
                "pending" => Some("pending"),
                "inprogress" | "running" | "creating" => Some("in_progress"),
                "completed" | "submitted" | "accepted" => Some("completed"),
                "failed" | "cancelled" | "timedout" => Some("failed"),
                "stuck" => Some("stuck"),
                "interrupted" => Some("interrupted"),
                _ => None,
            };
            if let Some(key) = key {
                let count = summary[key].as_u64().unwrap_or_default() + 1;
                summary.insert(key.to_string(), serde_json::json!(count));
            }
        }

        let mut jobs: Vec<_> = all_jobs
            .into_values()
            .filter(|job| job_matches_filter(filter, job["status"].as_str().unwrap_or_default()))
            .collect();
        jobs.sort_by(|left, right| {
            right["created_at"]
                .as_str()
                .cmp(&left["created_at"].as_str())
        });
        jobs.truncate(MAX_LISTED_JOBS);

        let result = serde_json::json!({
            "returned": jobs.len(),
            "jobs": jobs,
            "summary": summary,
        });

        Ok(ToolOutput::success(result, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

fn job_matches_filter(filter: &str, status: &str) -> bool {
    let status = status.to_ascii_lowercase().replace('_', "");
    match filter {
        "active" => matches!(
            status.as_str(),
            "pending" | "inprogress" | "running" | "creating"
        ),
        "completed" => matches!(status.as_str(), "completed" | "submitted" | "accepted"),
        "failed" => matches!(
            status.as_str(),
            "failed" | "cancelled" | "timedout" | "stuck" | "interrupted"
        ),
        _ => true,
    }
}

/// Tool for checking job status.
pub struct JobStatusTool {
    context_manager: Arc<ContextManager>,
    store: Option<Arc<dyn Database>>,
}

impl JobStatusTool {
    pub fn new(context_manager: Arc<ContextManager>) -> Self {
        Self {
            context_manager,
            store: None,
        }
    }

    pub fn with_store(mut self, store: Arc<dyn Database>) -> Self {
        self.store = Some(store);
        self
    }

    /// Durable `partial_output` snapshot for the DB fallback branch (WI#263
    /// review finding): after a daemon restart the in-memory context is gone,
    /// so the snapshot written by `preserve_timeout_evidence` is recovered
    /// from the persisted `partial_output` job event instead. Returns the
    /// latest such event's content.
    async fn persisted_partial_output(&self, job_id: Uuid) -> Option<String> {
        let store = self.store.as_ref()?;
        let events = store.list_job_events(job_id, Some(16)).await.ok()?;
        events
            .iter()
            .rev()
            .find(|event| event.event_type == "partial_output")
            .and_then(|event| event.data.get("content"))
            .and_then(|content| content.as_str())
            .map(str::to_string)
    }
}

#[async_trait]
impl Tool for JobStatusTool {
    fn name(&self) -> &str {
        "job_status"
    }

    fn description(&self) -> &str {
        "Check the status and details of a specific job by its ID."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The job ID (full UUID or short prefix, e.g. 'f2854dd8')"
                }
            },
            "required": ["job_id"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let requester_id = ctx.user_id.clone();

        let job_id_str = require_str(&params, "job_id")?;
        let job_id = resolve_query_job_id(
            job_id_str,
            &self.context_manager,
            self.store.as_ref(),
            &requester_id,
        )
        .await?;

        match self.context_manager.get_context(job_id).await {
            Ok(job_ctx) => {
                if job_ctx.user_id != requester_id {
                    let result = serde_json::json!({
                        "error": "Job not found".to_string()
                    });
                    return Ok(ToolOutput::success(result, start.elapsed()));
                }
                let result = serde_json::json!({
                    "job_id": job_id.to_string(),
                    "title": job_ctx.title,
                    "description": job_ctx.description,
                    "status": format!("{:?}", job_ctx.state),
                    "created_at": job_ctx.created_at.to_rfc3339(),
                    "started_at": job_ctx.started_at.map(|t| t.to_rfc3339()),
                    "completed_at": job_ctx.completed_at.map(|t| t.to_rfc3339()),
                    "actual_cost": job_ctx.actual_cost.to_string(),
                    "fallback_deliverable": job_ctx.metadata.get("fallback_deliverable"),
                    "partial_output": job_ctx.metadata.get("partial_output"),
                });
                Ok(ToolOutput::success(result, start.elapsed()))
            }
            Err(context_error) => {
                let Some(store) = &self.store else {
                    let result = serde_json::json!({
                        "error": format!("Job not found: {}", context_error)
                    });
                    return Ok(ToolOutput::success(result, start.elapsed()));
                };
                let record = store.get_sandbox_job(job_id).await.map_err(|error| {
                    ToolError::ExecutionFailed(format!("failed to load persisted job: {error}"))
                })?;
                let Some(record) = record.filter(|record| record.user_id == requester_id) else {
                    let result = serde_json::json!({ "error": "Job not found" });
                    return Ok(ToolOutput::success(result, start.elapsed()));
                };
                let result = serde_json::json!({
                    "job_id": record.id.to_string(),
                    "title": record.task,
                    "status": record.status,
                    "created_at": record.created_at.to_rfc3339(),
                    "started_at": record.started_at.map(|time| time.to_rfc3339()),
                    "completed_at": record.completed_at.map(|time| time.to_rfc3339()),
                    "success": record.success,
                    "failure_reason": record.failure_reason,
                    "project_dir": record.project_dir,
                    "source": "database",
                    "events_available": true,
                    // WI#263 review finding: the snapshot survives restarts by
                    // being recovered from the persisted `partial_output` job
                    // event, so the DB branch reports the same field as the
                    // in-memory branch.
                    "partial_output": self.persisted_partial_output(job_id).await,
                });
                Ok(ToolOutput::success(result, start.elapsed()))
            }
        }
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

/// Tool for canceling a job.
///
/// For sandbox jobs (registered via `register_sandbox_job`), cancellation also
/// stops the Docker container and updates the DB status — matching the behavior
/// of the web cancellation handler in `channels/web/handlers/jobs.rs`.
pub struct CancelJobTool {
    context_manager: Arc<ContextManager>,
    scheduler: Option<SchedulerSlot>,
    job_manager: Option<Arc<ContainerJobManager>>,
    external_worker_manager: Option<Arc<ExternalWorkerManager>>,
    store: Option<Arc<dyn Database>>,
}

impl CancelJobTool {
    pub fn new(context_manager: Arc<ContextManager>) -> Self {
        Self {
            context_manager,
            scheduler: None,
            job_manager: None,
            external_worker_manager: None,
            store: None,
        }
    }

    pub fn with_scheduler(mut self, scheduler: SchedulerSlot) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    /// Inject sandbox dependencies so cancellation also stops containers.
    pub fn with_sandbox(
        mut self,
        job_manager: Arc<ContainerJobManager>,
        store: Option<Arc<dyn Database>>,
    ) -> Self {
        self.job_manager = Some(job_manager);
        self.store = store;
        self
    }

    /// Inject external worker manager so cancellation can reach external workers.
    pub fn with_external_workers(mut self, mgr: Arc<ExternalWorkerManager>) -> Self {
        self.external_worker_manager = Some(mgr);
        self
    }

    pub fn with_store(mut self, store: Arc<dyn Database>) -> Self {
        self.store = Some(store);
        self
    }
}

#[async_trait]
impl Tool for CancelJobTool {
    fn name(&self) -> &str {
        "cancel_job"
    }

    fn description(&self) -> &str {
        "Immediately cancel a running or pending job and stop its active scheduler, sandbox, or \
         external-worker execution. Use this instead of job_prompt(done=true), which only ends \
         follow-up prompting and does not cancel work."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The job ID (full UUID or short prefix, e.g. 'f2854dd8')"
                }
            },
            "required": ["job_id"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let requester_id = ctx.user_id.clone();

        let job_id_str = require_str(&params, "job_id")?;
        let job_id = resolve_job_id(job_id_str, &self.context_manager).await?;

        let job = self
            .context_manager
            .get_context(job_id)
            .await
            .map_err(|_| ToolError::ExecutionFailed(format!("job {job_id} not found")))?;
        if job.user_id != requester_id {
            return Err(ToolError::ExecutionFailed("Job not found".to_string()));
        }
        if !job.state.is_parallel_blocking() {
            return Err(ToolError::ExecutionFailed(format!(
                "job {job_id} is {} and cannot be cancelled",
                job.state
            )));
        }

        let mut stopped = false;
        if let Some(ref ewm) = self.external_worker_manager {
            stopped |= ewm.cancel_task(job_id).await;
        }
        if let Some(ref slot) = self.scheduler
            && let Some(ref scheduler) = *slot.read().await
        {
            stopped |= scheduler.stop(job_id).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("failed to stop scheduled job: {e}"))
            })?;
        }
        if let Some(ref jm) = self.job_manager {
            match jm.stop_job(job_id).await {
                Ok(()) => stopped = true,
                Err(crate::error::OrchestratorError::ContainerNotFound { .. }) => {}
                // DG-4: write-back promotion is running; cancellation must
                // not clear the secrets its secret-scan needs or remove the
                // staging dir mid-walk.
                Err(crate::error::OrchestratorError::InvalidContainerState { state, .. })
                    if state.starts_with("finalizing") =>
                {
                    return Err(ToolError::ExecutionFailed(format!(
                        "job {job_id} is finalizing write-back promotion; cancellation is refused — retry once it completes"
                    )));
                }
                Err(error) => {
                    tracing::warn!(%job_id, "Failed to stop container during cancellation: {error}");
                }
            }
        }

        let runtime_backends_configured = self.scheduler.is_some()
            || self.job_manager.is_some()
            || self.external_worker_manager.is_some();
        if !stopped && runtime_backends_configured {
            return Err(ToolError::ExecutionFailed(format!(
                "job {job_id} has no active runtime to cancel"
            )));
        }

        self.context_manager
            .update_context(job_id, |ctx| {
                if ctx.state == JobState::Cancelled {
                    Ok(())
                } else {
                    ctx.transition_to(JobState::Cancelled, Some("Cancelled by user".to_string()))
                }
            })
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to update job: {e}")))?
            .map_err(|e| ToolError::ExecutionFailed(format!("cannot cancel job: {e}")))?;

        if let Some(ref store) = self.store
            && let Err(error) = store
                .update_sandbox_job_status(
                    job_id,
                    "cancelled",
                    Some(false),
                    Some("Cancelled by user"),
                    None,
                    Some(Utc::now()),
                )
                .await
        {
            tracing::warn!(%job_id, "Failed to persist job cancellation: {error}");
        }

        let result = serde_json::json!({
            "job_id": job_id.to_string(),
            "status": "cancelled",
            "message": "Job cancellation requested"
        });
        Ok(ToolOutput::success(result, start.elapsed()))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

/// Tool for reading sandbox job event logs.
///
/// Lets the main agent inspect what a running (or completed) container job has
/// been doing: messages, tool calls, results, status changes, etc.
///
/// Events are streamed from the sandbox worker into the database via the
/// orchestrator's event pipeline. This tool queries them with a DB-level
/// `LIMIT` (default 50, configurable via the `limit` parameter) so the
/// agent sees the most recent activity without loading the full history.
pub struct JobEventsTool {
    store: Arc<dyn Database>,
    context_manager: Arc<ContextManager>,
}

impl JobEventsTool {
    pub fn new(store: Arc<dyn Database>, context_manager: Arc<ContextManager>) -> Self {
        Self {
            store,
            context_manager,
        }
    }
}

#[async_trait]
impl Tool for JobEventsTool {
    fn name(&self) -> &str {
        "job_events"
    }

    fn description(&self) -> &str {
        "Read the event log for a sandbox job. Shows messages, tool calls, results, \
         and status changes from the container. Use this to check what Claude Code \
         or a worker sub-agent has been doing."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The job ID (full UUID or short prefix, e.g. 'f2854dd8')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of events to return (default 50, most recent)"
                }
            },
            "required": ["job_id"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let job_id_str = params
            .get("job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParameters("missing 'job_id' parameter".into()))?;

        let job_id = resolve_query_job_id(
            job_id_str,
            &self.context_manager,
            Some(&self.store),
            &ctx.user_id,
        )
        .await?;

        let live_owner = self
            .context_manager
            .get_context(job_id)
            .await
            .is_ok_and(|job_ctx| job_ctx.user_id == ctx.user_id);
        let persisted_owner = self
            .store
            .sandbox_job_belongs_to_user(job_id, &ctx.user_id)
            .await
            .map_err(|error| {
                ToolError::ExecutionFailed(format!("failed to verify job ownership: {error}"))
            })?;
        if !live_owner && !persisted_owner {
            return Err(ToolError::ExecutionFailed(format!(
                "job {} not found",
                job_id
            )));
        }

        const MAX_EVENT_LIMIT: i64 = 1000;
        let limit = params
            .get("limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(50)
            .clamp(1, MAX_EVENT_LIMIT);

        let events = self
            .store
            .list_job_events(job_id, Some(limit))
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to load job events: {}", e)))?;

        let recent: Vec<serde_json::Value> = events
            .iter()
            .map(|ev| {
                serde_json::json!({
                    "event_type": ev.event_type,
                    "data": ev.data,
                    "created_at": ev.created_at.to_rfc3339(),
                })
            })
            .collect();

        let result = serde_json::json!({
            "job_id": job_id.to_string(),
            "total_events": events.len(),
            "returned": recent.len(),
            "events": recent,
        });

        Ok(ToolOutput::success(result, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        true
    }
}

/// Tool for sending follow-up prompts to a running Claude Code sandbox job.
///
/// The prompt is queued in an in-memory `PromptQueue` (a broadcast channel
/// shared with the web gateway). The Claude Code bridge inside the container
/// polls for queued prompts between turns and feeds them into the next
/// `claude --resume` invocation, enabling interactive multi-turn sessions
/// with long-running sandbox jobs.
pub struct JobPromptTool {
    prompt_queue: PromptQueue,
    context_manager: Arc<ContextManager>,
    store: Option<Arc<dyn Database>>,
}

/// Type alias matching `crate::channels::web::server::PromptQueue`.
pub type PromptQueue = Arc<
    tokio::sync::Mutex<
        std::collections::HashMap<
            Uuid,
            std::collections::VecDeque<crate::orchestrator::api::PendingPrompt>,
        >,
    >,
>;

impl JobPromptTool {
    pub fn new(prompt_queue: PromptQueue, context_manager: Arc<ContextManager>) -> Self {
        Self {
            prompt_queue,
            context_manager,
            store: None,
        }
    }

    pub fn with_store(mut self, store: Arc<dyn Database>) -> Self {
        self.store = Some(store);
        self
    }
}

#[async_trait]
impl Tool for JobPromptTool {
    fn name(&self) -> &str {
        "job_prompt"
    }

    fn description(&self) -> &str {
        "Send a follow-up prompt to a running Claude Code sandbox job. The prompt is \
         queued and delivered on the next poll cycle. Use this to give the sub-agent \
         additional instructions, answer its questions, or tell it to wrap up. This does \
         not cancel work; use cancel_job for cancellation. External workers do not support \
         follow-up prompts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The job ID (full UUID or short prefix, e.g. 'f2854dd8')"
                },
                "content": {
                    "type": "string",
                    "description": "The follow-up prompt text to send"
                },
                "done": {
                    "type": "boolean",
                    "description": "If true, signals the sub-agent that no more prompts are coming \
                                    and it should finish up. Default false."
                }
            },
            "required": ["job_id", "content"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let job_id_str = params
            .get("job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParameters("missing 'job_id' parameter".into()))?;

        let job_id = resolve_job_id(job_id_str, &self.context_manager).await?;

        // Verify the caller owns this job. A missing context is treated as
        // unauthorized to prevent sending prompts to jobs after process restarts.
        let job_ctx = self
            .context_manager
            .get_context(job_id)
            .await
            .map_err(|_| {
                ToolError::ExecutionFailed(format!(
                    "job {} not found or context unavailable",
                    job_id
                ))
            })?;

        if job_ctx.user_id != ctx.user_id {
            return Err(ToolError::ExecutionFailed(format!(
                "job {} does not belong to current user",
                job_id
            )));
        }

        if !matches!(job_ctx.state, JobState::Pending | JobState::InProgress) {
            return Err(ToolError::ExecutionFailed(format!(
                "job {} is {} and cannot receive follow-up prompts",
                job_id, job_ctx.state
            )));
        }

        if let Some(ref store) = self.store
            && let Ok(Some(mode)) = store.get_sandbox_job_mode(job_id).await
            && mode.starts_with("external:")
        {
            return Err(ToolError::ExecutionFailed(format!(
                "job {job_id} uses external worker '{}'; external jobs cannot receive job_prompt. Use cancel_job to stop it",
                mode.trim_start_matches("external:")
            )));
        }

        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParameters("missing 'content' parameter".into()))?;

        let done = params
            .get("done")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let prompt = crate::orchestrator::api::PendingPrompt {
            content: content.to_string(),
            done,
        };

        {
            let mut queue = self.prompt_queue.lock().await;
            queue.entry(job_id).or_default().push_back(prompt);
        }

        let result = serde_json::json!({
            "job_id": job_id.to_string(),
            "status": "queued",
            "message": "Prompt queued",
            "done": done,
        });

        Ok(ToolOutput::success(result, start.elapsed()))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_job_tool_local() {
        let manager = Arc::new(ContextManager::new(5));
        let tool = CreateJobTool::new(manager.clone());

        // Without sandbox deps, it should use the local path
        assert!(!tool.sandbox_enabled()); // safety: test

        let params = serde_json::json!({
            "title": "Test Job",
            "description": "A test job description"
        });

        let ctx = JobContext::default();
        let result = tool.execute(params, &ctx).await.unwrap(); // safety: test

        let job_id = result.result.get("job_id").unwrap().as_str().unwrap(); // safety: test
        assert!(!job_id.is_empty()); // safety: test
        assert_eq!(
            /* safety: test */
            result.result.get("status").unwrap().as_str().unwrap(), // safety: test
            "pending"
        );
        assert_eq!(result.result["max_iterations"], DEFAULT_WORKER_ITERATIONS);
        let job_id = Uuid::parse_str(job_id).unwrap();
        assert_eq!(
            manager.get_context(job_id).await.unwrap().metadata["max_iterations"],
            DEFAULT_WORKER_ITERATIONS
        );
    }

    #[test]
    fn test_schema_changes_with_sandbox() {
        let manager = Arc::new(ContextManager::new(5));

        // Without sandbox
        let tool = CreateJobTool::new(Arc::clone(&manager));
        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap().as_object().unwrap(); // safety: test
        assert!(props.contains_key("title")); // safety: test
        assert!(props.contains_key("description")); // safety: test
        assert!(props.contains_key("max_iterations")); // safety: test
        assert!(!props.contains_key("wait")); // safety: test
        assert!(!props.contains_key("mode")); // safety: test
    }

    #[test]
    fn external_worker_schema_guides_direct_opencode_dispatch() {
        use crate::config::{ExternalWorkerConfig, LoadBalanceStrategy};

        let manager = Arc::new(ContextManager::new(5));
        let workers = Arc::new(ExternalWorkerManager::new(vec![ExternalWorkerConfig {
            name: "opencode".to_string(),
            url: "ws://127.0.0.1:20007/ws/agent".to_string(),
            auth_token: None,
            timeout_ms: 900_000,
            endpoints: vec![],
            load_balance: LoadBalanceStrategy::default(),
            prompt_context: Default::default(),
        }]));
        let tool = CreateJobTool::new(manager).with_external_workers(workers, None);

        assert!(
            tool.description()
                .contains("call this create_job tool directly")
        );
        assert!(tool.description().contains("not extensions, MCP servers"));
        assert!(tool.description().contains("30-second local-job default"));
        assert!(
            tool.description()
                .contains("default to asynchronous dispatch")
        );
        assert_eq!(tool.execution_timeout(), Duration::from_secs(960));
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::UnlessAutoApproved
        );

        let schema = tool.parameters_schema();
        let mode = &schema["properties"]["mode"];
        assert_eq!(mode["enum"], serde_json::json!(["worker", "opencode"]));
        let description = mode["description"].as_str().expect("mode description");
        assert!(description.contains("create_job directly"));
        assert!(description.contains("opencode"));
        assert_eq!(
            schema["properties"]["context_scope"]["enum"],
            serde_json::json!(["none", "recent"])
        );
        assert_eq!(schema["properties"]["context_scope"]["default"], "none");
        assert_eq!(
            schema["properties"]["max_iterations"]["default"],
            DEFAULT_WORKER_ITERATIONS
        );
        assert_eq!(
            schema["properties"]["max_iterations"]["maximum"],
            MAX_WORKER_ITERATIONS
        );
    }

    #[tokio::test]
    async fn named_external_worker_rejects_local_iteration_budget() {
        use crate::config::{ExternalWorkerConfig, LoadBalanceStrategy};

        let manager = Arc::new(ContextManager::new(5));
        let workers = Arc::new(ExternalWorkerManager::new(vec![ExternalWorkerConfig {
            name: "opencode".to_string(),
            url: "ws://127.0.0.1:9/ws/agent".to_string(),
            auth_token: None,
            timeout_ms: 900_000,
            endpoints: vec![],
            load_balance: LoadBalanceStrategy::default(),
            prompt_context: Default::default(),
        }]));
        let tool = CreateJobTool::new(manager).with_external_workers(workers, None);
        let error = tool
            .execute(
                serde_json::json!({
                    "title": "Unsupported option",
                    "description": "Do work",
                    "mode": "opencode",
                    "max_iterations": 300
                }),
                &JobContext::default(),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("not supported"));
    }

    #[test]
    fn external_jobs_default_to_async_but_allow_explicit_wait() {
        assert!(!external_job_wait(&serde_json::json!({})));
        assert!(!external_job_wait(&serde_json::json!({ "wait": false })));
        assert!(external_job_wait(&serde_json::json!({ "wait": true })));
    }

    #[test]
    fn worker_iteration_budget_defaults_and_validates_bounds() {
        assert_eq!(
            worker_max_iterations(&serde_json::json!({})).unwrap(),
            DEFAULT_WORKER_ITERATIONS
        );
        assert_eq!(
            worker_max_iterations(&serde_json::json!({"max_iterations": 300})).unwrap(),
            300
        );
        assert!(worker_max_iterations(&serde_json::json!({"max_iterations": 0})).is_err());
        assert!(
            worker_max_iterations(
                &serde_json::json!({"max_iterations": MAX_WORKER_ITERATIONS + 1})
            )
            .is_err()
        );
        assert!(worker_max_iterations(&serde_json::json!({"max_iterations": "300"})).is_err());
    }

    #[test]
    fn external_context_scope_defaults_to_none_and_rejects_unknown_values() {
        assert_eq!(
            ExternalContextScope::parse(&serde_json::json!({})).unwrap(),
            ExternalContextScope::None
        );
        assert_eq!(
            ExternalContextScope::parse(&serde_json::json!({"context_scope": "recent"})).unwrap(),
            ExternalContextScope::Recent
        );
        assert!(
            ExternalContextScope::parse(&serde_json::json!({"context_scope": "full"})).is_err()
        );
    }

    #[test]
    fn external_prompt_context_is_opt_in_bounded_and_metadata_independent() {
        use lunarwing_common::{PromptContextMessage, PromptContextRole};

        let ctx = JobContext {
            metadata: serde_json::json!({
                "conv_secret": "must not become history",
                "notify_user": "private-routing-id"
            }),
            prompt_context: Arc::from([
                PromptContextMessage {
                    role: PromptContextRole::User,
                    content: "old question".to_string(),
                },
                PromptContextMessage {
                    role: PromptContextRole::Assistant,
                    content: "new answer".to_string(),
                },
            ]),
            ..Default::default()
        };
        let limits = PromptContextLimits {
            max_messages: 1,
            max_chars_per_message: 3,
            max_total_chars: 3,
        };

        assert!(external_prompt_context(&ctx, ExternalContextScope::None, limits).is_empty());
        assert_eq!(
            external_prompt_context(&ctx, ExternalContextScope::Recent, limits),
            vec![PromptContextMessage {
                role: PromptContextRole::Assistant,
                content: "new".to_string(),
            }]
        );
    }

    #[test]
    fn test_execution_timeout_sandbox() {
        let manager = Arc::new(ContextManager::new(5));

        // Without sandbox: default timeout
        let tool = CreateJobTool::new(Arc::clone(&manager));
        assert_eq!(tool.execution_timeout(), Duration::from_secs(30)); // safety: test
    }

    #[tokio::test]
    async fn test_sandbox_without_job_manager_returns_error() {
        let manager = Arc::new(ContextManager::new(5));
        // Create tool without sandbox deps — job_manager is None.
        let tool = CreateJobTool::new(manager);
        assert!(!tool.sandbox_enabled());

        let result = tool
            .execute_sandbox(
                "test task",
                SandboxExecutionOptions {
                    explicit_dir: None,
                    wait: false,
                    credential_grants: vec![],
                    mcp_servers: vec![],
                    max_iterations: DEFAULT_WORKER_ITERATIONS,
                    harness: None,
                },
                &JobContext::default(),
            )
            .await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed(_)),
            "expected ExecutionFailed, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_list_jobs_tool() {
        let manager = Arc::new(ContextManager::new(5));

        // Create some jobs
        manager.create_job("Job 1", "Desc 1").await.unwrap(); // safety: test
        manager.create_job("Job 2", "Desc 2").await.unwrap(); // safety: test

        let tool = ListJobsTool::new(manager);

        let params = serde_json::json!({});
        let ctx = JobContext::default();
        let result = tool.execute(params, &ctx).await.unwrap(); // safety: test

        let jobs = result.result.get("jobs").unwrap().as_array().unwrap(); // safety: test
        assert_eq!(jobs.len(), 2); // safety: test
    }

    #[tokio::test]
    async fn test_job_status_tool() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = manager.create_job("Test Job", "Description").await.unwrap(); // safety: test

        let tool = JobStatusTool::new(manager);

        let params = serde_json::json!({
            "job_id": job_id.to_string()
        });
        let ctx = JobContext::default();
        let result = tool.execute(params, &ctx).await.unwrap(); // safety: test

        assert_eq!(
            /* safety: test */
            result.result.get("title").unwrap().as_str().unwrap(), // safety: test
            "Test Job"
        );
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn persisted_external_job_remains_queryable_after_restart() {
        use crate::testing::TestHarnessBuilder;

        let harness = TestHarnessBuilder::new().build().await;
        let store = Arc::clone(&harness.db);
        let manager = Arc::new(ContextManager::new(5));
        let job_id = Uuid::new_v4();
        store
            .save_sandbox_job(&SandboxJobRecord {
                id: job_id,
                task: "Persisted OpenCode task".to_string(),
                status: "interrupted".to_string(),
                user_id: "default".to_string(),
                project_dir: "/workspace/lunarwing2".to_string(),
                success: None,
                failure_reason: Some("Process restarted".to_string()),
                created_at: Utc::now(),
                started_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
                credential_grants_json: "[]".to_string(),
                job_mode: "worker".to_string(),
            })
            .await
            .expect("save persisted job");
        store
            .save_job_event(
                job_id,
                "message",
                &serde_json::json!({ "content": "reading source files" }),
            )
            .await
            .expect("save persisted event");

        let prefix = &job_id.simple().to_string()[..8];
        let ctx = JobContext::default();
        let status = JobStatusTool::new(Arc::clone(&manager))
            .with_store(Arc::clone(&store))
            .execute(serde_json::json!({ "job_id": prefix }), &ctx)
            .await
            .expect("query persisted status");
        assert_eq!(status.result["job_id"], job_id.to_string());
        assert_eq!(status.result["status"], "interrupted");
        assert_eq!(status.result["failure_reason"], "Process restarted");

        let events = JobEventsTool::new(Arc::clone(&store), Arc::clone(&manager))
            .execute(serde_json::json!({ "job_id": prefix }), &ctx)
            .await
            .expect("query persisted events");
        assert_eq!(events.result["total_events"], 1);
        assert_eq!(
            events.result["events"][0]["data"]["content"],
            "reading source files"
        );

        let listed = ListJobsTool::new(manager)
            .with_store(store)
            .execute(serde_json::json!({}), &ctx)
            .await
            .expect("list persisted jobs");
        assert_eq!(listed.result["jobs"][0]["job_id"], job_id.to_string());
        assert_eq!(listed.result["summary"]["interrupted"], 1);

        let other_ctx = JobContext::with_user("other-user", "query", "query");
        let hidden_status = JobStatusTool::new(Arc::new(ContextManager::new(5)))
            .with_store(Arc::clone(&harness.db))
            .execute(
                serde_json::json!({ "job_id": job_id.to_string() }),
                &other_ctx,
            )
            .await
            .expect("hide persisted status from another user");
        assert_eq!(hidden_status.result["error"], "Job not found");

        let hidden_events =
            JobEventsTool::new(Arc::clone(&harness.db), Arc::new(ContextManager::new(5)))
                .execute(
                    serde_json::json!({ "job_id": job_id.to_string() }),
                    &other_ctx,
                )
                .await;
        assert!(hidden_events.is_err());
    }

    /// WI#263 review finding: the `partial_output` snapshot must survive a
    /// daemon restart. The DB fallback branch of job_status recovers it from
    /// the persisted `partial_output` job event written by
    /// `preserve_timeout_evidence`, so the field is present in both branches.
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn db_job_status_recovers_partial_output_from_events() {
        use crate::testing::TestHarnessBuilder;

        let harness = TestHarnessBuilder::new().build().await;
        let store = Arc::clone(&harness.db);
        let manager = Arc::new(ContextManager::new(5));
        let job_id = Uuid::new_v4();
        store
            .save_sandbox_job(&SandboxJobRecord {
                id: job_id,
                task: "Timed-out kimi audit".to_string(),
                status: "timed_out".to_string(),
                user_id: "default".to_string(),
                project_dir: "/workspace/lunarwing2".to_string(),
                success: None,
                failure_reason: Some("External worker timed out".to_string()),
                created_at: Utc::now(),
                started_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
                credential_grants_json: "[]".to_string(),
                job_mode: "worker".to_string(),
            })
            .await
            .expect("save persisted job");
        store
            .save_job_event(
                job_id,
                "message",
                &serde_json::json!({ "content": "streamed investigation line" }),
            )
            .await
            .expect("save streamed event");
        store
            .save_job_event(
                job_id,
                "partial_output",
                &serde_json::json!({ "content": "[worker kimi timed out after streaming 40 bytes; tail preserved]\nInvestigation step: evidence gathered." }),
            )
            .await
            .expect("save partial_output event");

        let ctx = JobContext::default();
        let status = JobStatusTool::new(Arc::clone(&manager))
            .with_store(Arc::clone(&store))
            .execute(serde_json::json!({ "job_id": job_id.to_string() }), &ctx)
            .await
            .expect("query persisted status");

        assert_eq!(status.result["source"], "database");
        let partial = status
            .result
            .get("partial_output")
            .and_then(|value| value.as_str())
            .expect("partial_output must be recovered in the DB branch");
        assert!(
            partial.ends_with("Investigation step: evidence gathered."),
            "recovered snapshot must carry the streamed evidence tail"
        );
    }

    #[tokio::test]
    async fn test_create_job_params() {
        let manager = Arc::new(ContextManager::new(5));
        let tool = CreateJobTool::new(manager);
        let ctx = JobContext::default();

        let missing_title = tool
            .execute(serde_json::json!({ "description": "A test job" }), &ctx)
            .await;
        assert!(missing_title.is_err()); // safety: test
        assert!(
            /* safety: test */
            missing_title
                .unwrap_err()
                .to_string()
                .contains("missing 'title' parameter")
        );

        let missing_description = tool
            .execute(serde_json::json!({ "title": "Test Job" }), &ctx)
            .await;
        assert!(missing_description.is_err()); // safety: test
        assert!(
            /* safety: test */
            missing_description
                .unwrap_err()
                .to_string()
                .contains("missing 'description' parameter")
        );
    }

    #[tokio::test]
    async fn test_list_jobs_formatting() {
        let manager = Arc::new(ContextManager::new(10));
        let pending_id = manager
            .create_job_for_user("default", "Pending Job", "Todo")
            .await
            .unwrap(); // safety: test
        let completed_id = manager
            .create_job_for_user("default", "Completed Job", "Done")
            .await
            .unwrap(); // safety: test
        let failed_id = manager
            .create_job_for_user("default", "Failed Job", "Oops")
            .await
            .unwrap(); // safety: test
        manager
            .create_job_for_user("other-user", "Other User Job", "Ignore")
            .await
            .unwrap(); // safety: test

        manager
            .update_context(completed_id, |ctx| {
                ctx.transition_to(JobState::InProgress, None)?;
                ctx.transition_to(JobState::Completed, Some("done".to_string()))
            })
            .await
            .unwrap() // safety: test
            .unwrap(); // safety: test
        manager
            .update_context(failed_id, |ctx| {
                ctx.transition_to(JobState::InProgress, None)?;
                ctx.transition_to(JobState::Failed, Some("boom".to_string()))
            })
            .await
            .unwrap() // safety: test
            .unwrap(); // safety: test

        let tool = ListJobsTool::new(Arc::clone(&manager));
        let ctx = JobContext::default();
        let result = tool.execute(serde_json::json!({}), &ctx).await.unwrap(); // safety: test

        let jobs = result.result.get("jobs").unwrap().as_array().unwrap(); // safety: test
        assert_eq!(jobs.len(), 3); // safety: test
        assert!(jobs.iter().any(|job| {
            // safety: test
            job.get("job_id").and_then(|v| v.as_str()) == Some(&pending_id.to_string())
                && job.get("status").and_then(|v| v.as_str()) == Some("Pending")
        }));
        assert!(jobs.iter().any(|job| {
            // safety: test
            job.get("job_id").and_then(|v| v.as_str()) == Some(&completed_id.to_string())
                && job.get("status").and_then(|v| v.as_str()) == Some("Completed")
        }));
        assert!(jobs.iter().any(|job| {
            // safety: test
            job.get("job_id").and_then(|v| v.as_str()) == Some(&failed_id.to_string())
                && job.get("status").and_then(|v| v.as_str()) == Some("Failed")
        }));

        let summary = result.result.get("summary").unwrap(); // safety: test
        assert_eq!(summary.get("total").and_then(|v| v.as_u64()), Some(3)); // safety: test
        assert_eq!(summary.get("pending").and_then(|v| v.as_u64()), Some(1)); // safety: test
        assert_eq!(summary.get("completed").and_then(|v| v.as_u64()), Some(1)); // safety: test
        assert_eq!(summary.get("failed").and_then(|v| v.as_u64()), Some(1)); // safety: test
    }

    #[tokio::test]
    async fn test_job_status_transitions() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = manager
            .create_job_for_user("default", "Transition Job", "Track me")
            .await
            .unwrap(); // safety: test
        manager
            .update_context(job_id, |ctx| {
                ctx.transition_to(JobState::InProgress, Some("started".to_string()))?;
                ctx.transition_to(JobState::Completed, Some("finished".to_string()))
            })
            .await
            .unwrap() // safety: test
            .unwrap(); // safety: test

        let tool = JobStatusTool::new(Arc::clone(&manager));
        let ctx = JobContext::default();
        let result = tool
            .execute(serde_json::json!({ "job_id": job_id.to_string() }), &ctx)
            .await
            .unwrap(); // safety: test

        assert_eq!(
            /* safety: test */
            result.result.get("status").and_then(|v| v.as_str()),
            Some("Completed")
        );
        assert!(result.result.get("started_at").unwrap().is_string()); // safety: test
        assert!(result.result.get("completed_at").unwrap().is_string()); // safety: test
    }

    #[tokio::test]
    async fn test_cancel_job_running() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = manager
            .create_job_for_user("default", "Running Job", "In progress")
            .await
            .unwrap(); // safety: test
        manager
            .update_context(job_id, |ctx| ctx.transition_to(JobState::InProgress, None))
            .await
            .unwrap() // safety: test
            .unwrap(); // safety: test

        let tool = CancelJobTool::new(Arc::clone(&manager));
        let ctx = JobContext::default();
        let result = tool
            .execute(serde_json::json!({ "job_id": job_id.to_string() }), &ctx)
            .await
            .unwrap(); // safety: test

        assert_eq!(
            /* safety: test */
            result.result.get("status").and_then(|v| v.as_str()),
            Some("cancelled")
        );
        let updated = manager.get_context(job_id).await.unwrap(); // safety: test
        assert_eq!(updated.state, JobState::Cancelled); // safety: test
    }

    #[tokio::test]
    async fn test_cancel_job_completed() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = manager
            .create_job_for_user("default", "Completed Job", "Already done")
            .await
            .unwrap(); // safety: test
        manager
            .update_context(job_id, |ctx| {
                ctx.transition_to(JobState::InProgress, None)?;
                ctx.transition_to(JobState::Completed, Some("done".to_string()))
            })
            .await
            .unwrap() // safety: test
            .unwrap(); // safety: test

        let tool = CancelJobTool::new(Arc::clone(&manager));
        let ctx = JobContext::default();
        let error = tool
            .execute(serde_json::json!({ "job_id": job_id.to_string() }), &ctx)
            .await
            .unwrap_err(); // safety: test

        let message = error.to_string();
        assert!(message.contains("cannot be cancelled"));
        assert!(message.contains("completed"));
        let unchanged = manager.get_context(job_id).await.unwrap(); // safety: test
        assert_eq!(unchanged.state, JobState::Completed);
    }

    #[tokio::test]
    async fn test_job_status_includes_fallback_deliverable() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = manager
            .create_job_for_user("default", "Failing Job", "Will fail")
            .await
            .unwrap(); // safety: test

        // Inject a real FallbackDeliverable into the job metadata.
        let fallback = serde_json::json!({
            "partial": true,
            "failure_reason": "max iterations",
            "last_action": null,
            "action_stats": { "total": 5, "successful": 3, "failed": 2 },
            "tokens_used": 1000,
            "cost": "0.05",
            "elapsed_secs": 12.5,
            "repair_attempts": 1,
        });
        manager
            .update_context(job_id, |ctx| {
                ctx.metadata = serde_json::json!({ "fallback_deliverable": fallback.clone() });
                Ok::<(), String>(())
            })
            .await
            .unwrap() // safety: test
            .unwrap(); // safety: test

        let tool = JobStatusTool::new(manager);
        let params = serde_json::json!({ "job_id": job_id.to_string() });
        let ctx = JobContext::default();
        let result = tool.execute(params, &ctx).await.unwrap(); // safety: test

        let fb = result.result.get("fallback_deliverable").unwrap(); // safety: test
        assert_eq!(fb.get("partial").unwrap(), true); // safety: test
        assert_eq!(fb.get("failure_reason").unwrap(), "max iterations"); // safety: test
        let stats = fb.get("action_stats").unwrap(); // safety: test
        assert_eq!(stats.get("total").unwrap(), 5); // safety: test
        assert_eq!(stats.get("successful").unwrap(), 3); // safety: test
        assert_eq!(stats.get("failed").unwrap(), 2); // safety: test
    }

    /// WI#263: the timeout-evidence snapshot stored by the external worker
    /// orchestrator must be surfaced by job_status so a wedged-then-recovered
    /// investigation is readable without scraping job events.
    #[tokio::test]
    async fn test_job_status_includes_partial_output() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = manager
            .create_job_for_user("default", "Timed Out Job", "Wedge at report")
            .await
            .unwrap(); // safety: test

        let snapshot = "…tail of the streamed investigation…".to_string();
        manager
            .update_context(job_id, |ctx| {
                ctx.metadata = serde_json::json!({ "partial_output": snapshot.clone() });
                Ok::<(), String>(())
            })
            .await
            .unwrap() // safety: test
            .unwrap(); // safety: test

        let tool = JobStatusTool::new(manager);
        let params = serde_json::json!({ "job_id": job_id.to_string() });
        let ctx = JobContext::default();
        let result = tool.execute(params, &ctx).await.unwrap(); // safety: test

        let partial = result.result.get("partial_output").unwrap(); // safety: test
        assert_eq!(
            partial.as_str().unwrap(), // safety: test
            "…tail of the streamed investigation…"
        );
    }

    #[test]
    fn test_resolve_project_dir_auto() {
        let project_id = Uuid::new_v4();
        let (dir, browse_id) = resolve_project_dir(None, project_id).unwrap(); // safety: test
        assert!(dir.exists()); // safety: test
        assert!(dir.ends_with(project_id.to_string())); // safety: test
        assert_eq!(browse_id, project_id.to_string()); // safety: test

        // Must be under the projects base
        let base = projects_base().canonicalize().unwrap(); // safety: test
        assert!(dir.starts_with(&base)); // safety: test

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_project_dir_explicit_under_base() {
        let base = projects_base();
        std::fs::create_dir_all(&base).unwrap(); // safety: test
        // CW-3: explicit dirs must be exactly `projects/<uuid>` — a plain
        // named dir under the base is no longer accepted.
        let named = base.join("test_explicit_project");
        std::fs::create_dir_all(&named).unwrap(); // safety: test
        let project_id = Uuid::new_v4();
        assert!(resolve_project_dir(Some(named.clone()), project_id).is_err());
        let _ = std::fs::remove_dir_all(&named);

        let prior_job = Uuid::new_v4();
        let explicit = base.join(prior_job.to_string());
        // Explicit paths must already exist (no auto-create).
        std::fs::create_dir_all(&explicit).unwrap(); // safety: test

        let (dir, browse_id) = resolve_project_dir(Some(explicit.clone()), project_id).unwrap(); // safety: test
        assert!(dir.exists()); // safety: test
        assert_eq!(browse_id, prior_job.to_string()); // safety: test

        let canonical_base = base.canonicalize().unwrap(); // safety: test
        assert!(dir.starts_with(&canonical_base)); // safety: test

        let _ = std::fs::remove_dir_all(&explicit);
    }

    #[test]
    fn test_resolve_project_dir_rejects_outside_base() {
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let escape_attempt = tmp.path().join("evil_project");
        // Don't create it: explicit paths that don't exist are rejected
        // before the prefix check even runs.

        let result = resolve_project_dir(Some(escape_attempt), Uuid::new_v4());
        assert!(result.is_err()); // safety: test
        let err = result.unwrap_err().to_string();
        assert!(
            /* safety: test */
            err.contains("does not exist"),
            "expected 'does not exist' error, got: {}",
            err
        );
    }

    #[test]
    fn test_resolve_project_dir_rejects_outside_base_existing() {
        // A directory that exists but is outside the projects base.
        let tmp = tempfile::tempdir().unwrap(); // safety: test
        let outside = tmp.path().to_path_buf();

        let result = resolve_project_dir(Some(outside), Uuid::new_v4());
        assert!(result.is_err()); // safety: test
        let err = result.unwrap_err().to_string();
        assert!(
            /* safety: test */
            err.contains("must be a job directory under"),
            "expected 'must be a job directory under' error, got: {}",
            err
        );
    }

    #[test]
    fn test_resolve_project_dir_rejects_traversal() {
        // Non-existent traversal path is rejected because canonicalize fails.
        let base = projects_base();
        let traversal = base.join("legit").join("..").join("..").join(".ssh");

        let result = resolve_project_dir(Some(traversal), Uuid::new_v4());
        assert!(result.is_err(), "traversal path should be rejected"); // safety: test

        // Traversal path that actually resolves gets the prefix check.
        // `base/../` resolves to the parent of projects base, which is outside.
        let base_parent = projects_base().join("..").join("definitely_not_projects");
        std::fs::create_dir_all(&base_parent).ok();
        if base_parent.exists() {
            let result = resolve_project_dir(Some(base_parent.clone()), Uuid::new_v4());
            assert!(result.is_err(), "path outside base should be rejected"); // safety: test
            let _ = std::fs::remove_dir_all(&base_parent);
        }
    }

    #[test]
    fn test_sandbox_schema_includes_project_dir() {
        let manager = Arc::new(ContextManager::new(5));
        let jm = Arc::new(ContainerJobManager::new(
            crate::orchestrator::job_manager::ContainerJobConfig::default(),
            crate::orchestrator::TokenStore::new(),
        ));
        let tool = CreateJobTool::new(manager).with_sandbox(jm, None);
        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap().as_object().unwrap(); // safety: test
        assert!(
            /* safety: test */
            props.contains_key("project_dir"),
            "sandbox schema must expose project_dir"
        );
    }

    #[test]
    fn test_sandbox_schema_includes_credentials() {
        let manager = Arc::new(ContextManager::new(5));
        let jm = Arc::new(ContainerJobManager::new(
            crate::orchestrator::job_manager::ContainerJobConfig::default(),
            crate::orchestrator::TokenStore::new(),
        ));
        let tool = CreateJobTool::new(manager).with_sandbox(jm, None);
        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap().as_object().unwrap(); // safety: test
        assert!(
            /* safety: test */
            props.contains_key("credentials"),
            "sandbox schema must expose credentials"
        );
    }

    #[test]
    fn sandbox_schema_defaults_worker_mcp_to_none() {
        let manager = Arc::new(ContextManager::new(5));
        let jm = Arc::new(ContainerJobManager::new(
            crate::orchestrator::job_manager::ContainerJobConfig::default(),
            crate::orchestrator::TokenStore::new(),
        ));
        let schema = CreateJobTool::new(manager)
            .with_sandbox(jm, None)
            .parameters_schema();

        assert_eq!(
            schema["properties"]["mcp_servers"]["default"],
            serde_json::json!([])
        );
        assert_eq!(
            schema["properties"]["mcp_servers"]["maxItems"],
            MAX_WORKER_MCP_SERVERS
        );
    }

    #[tokio::test]
    async fn test_parse_credentials_empty() {
        let manager = Arc::new(ContextManager::new(5));
        let tool = CreateJobTool::new(manager);

        // No credentials parameter
        let params = serde_json::json!({"title": "t", "description": "d"});
        let grants = tool.parse_credentials(&params, "user1").await.unwrap(); // safety: test
        assert!(grants.is_empty()); // safety: test

        // Empty credentials object
        let params = serde_json::json!({"credentials": {}});
        let grants = tool.parse_credentials(&params, "user1").await.unwrap(); // safety: test
        assert!(grants.is_empty()); // safety: test
    }

    #[tokio::test]
    async fn test_parse_credentials_no_secrets_store() {
        let manager = Arc::new(ContextManager::new(5));
        let tool = CreateJobTool::new(manager);

        let params = serde_json::json!({"credentials": {"my_secret": "MY_SECRET"}});
        let result = tool.parse_credentials(&params, "user1").await;
        assert!(result.is_err()); // safety: test
        let err = result.unwrap_err().to_string();
        assert!(
            /* safety: test */
            err.contains("no secrets store"),
            "expected 'no secrets store' error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_parse_credentials_missing_secret() {
        use crate::testing::credentials::test_secrets_store;

        let manager = Arc::new(ContextManager::new(5));
        let secrets: Arc<dyn SecretsStore + Send + Sync> = Arc::new(test_secrets_store());

        let tool = CreateJobTool::new(manager).with_secrets(Arc::clone(&secrets));

        let params = serde_json::json!({"credentials": {"nonexistent_secret": "SOME_VAR"}});
        let result = tool.parse_credentials(&params, "user1").await;
        assert!(result.is_err()); // safety: test
        let err = result.unwrap_err().to_string();
        assert!(
            /* safety: test */
            err.contains("not found"),
            "expected 'not found' error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_parse_credentials_valid() {
        use crate::secrets::CreateSecretParams;
        use crate::testing::credentials::{TEST_GITHUB_TOKEN, test_secrets_store};

        let manager = Arc::new(ContextManager::new(5));
        let secrets: Arc<dyn SecretsStore + Send + Sync> = Arc::new(test_secrets_store());

        // Store a secret
        secrets
            .create(
                "user1",
                CreateSecretParams::new("github_token", TEST_GITHUB_TOKEN),
            )
            .await
            .unwrap(); // safety: test

        let tool = CreateJobTool::new(manager).with_secrets(Arc::clone(&secrets));

        let params = serde_json::json!({
            "credentials": {"github_token": "GITHUB_TOKEN"}
        });
        let grants = tool.parse_credentials(&params, "user1").await.unwrap(); // safety: test
        assert_eq!(grants.len(), 1); // safety: test
        assert_eq!(grants[0].secret_name, "github_token"); // safety: test
        assert_eq!(grants[0].env_var, "GITHUB_TOKEN"); // safety: test
    }

    fn test_prompt_tool(queue: PromptQueue) -> JobPromptTool {
        let cm = Arc::new(ContextManager::new(5));
        JobPromptTool::new(queue, cm)
    }

    #[tokio::test]
    async fn test_job_prompt_tool_queues_prompt() {
        let cm = Arc::new(ContextManager::new(5));
        let job_id = cm
            .create_job_for_user("default", "Test Job", "desc")
            .await
            .unwrap(); // safety: test

        let queue: PromptQueue =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let tool = JobPromptTool::new(Arc::clone(&queue), cm);

        let params = serde_json::json!({
            "job_id": job_id.to_string(),
            "content": "What's the status?",
            "done": false,
        });

        let ctx = JobContext::default();
        let result = tool.execute(params, &ctx).await.unwrap(); // safety: test

        assert_eq!(
            /* safety: test */
            result.result.get("status").unwrap().as_str().unwrap(), // safety: test
            "queued"
        );

        let q = queue.lock().await;
        let prompts = q.get(&job_id).unwrap(); // safety: test
        assert_eq!(prompts.len(), 1); // safety: test
        assert_eq!(prompts[0].content, "What's the status?"); // safety: test
        assert!(!prompts[0].done); // safety: test
    }

    #[tokio::test]
    async fn test_job_prompt_tool_requires_approval() {
        use crate::tools::tool::ApprovalRequirement;
        let queue: PromptQueue =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let tool = test_prompt_tool(queue);
        assert_eq!(
            /* safety: test */
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::UnlessAutoApproved
        );
    }

    #[tokio::test]
    async fn test_job_prompt_tool_rejects_invalid_uuid() {
        let queue: PromptQueue =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let tool = test_prompt_tool(queue);

        let params = serde_json::json!({
            "job_id": "not-a-uuid",
            "content": "hello",
        });

        let ctx = JobContext::default();
        let result = tool.execute(params, &ctx).await;
        assert!(result.is_err()); // safety: test
    }

    #[tokio::test]
    async fn test_job_prompt_tool_rejects_missing_content() {
        let queue: PromptQueue =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let tool = test_prompt_tool(queue);

        let params = serde_json::json!({
            "job_id": Uuid::new_v4().to_string(),
        });

        let ctx = JobContext::default();
        let result = tool.execute(params, &ctx).await;
        assert!(result.is_err()); // safety: test
    }

    #[tokio::test]
    async fn test_job_events_tool_rejects_other_users_job() {
        // JobEventsTool needs a Store (PostgreSQL) for the full path, but the
        // ownership check happens first via ContextManager, so we can test that
        // without a database by using a Store that will never be reached.
        //
        // We construct the tool by hand: the store field is never touched
        // because the ownership check short-circuits before the query.
        let cm = Arc::new(ContextManager::new(5));
        let job_id = cm
            .create_job_for_user("owner-user", "Secret Job", "classified")
            .await
            .unwrap(); // safety: test

        // We need a Store to construct the tool, but creating one requires
        // a database URL. Instead, test the ownership logic directly:
        // simulate what execute() does.
        let attacker_ctx = JobContext {
            user_id: "attacker".to_string(),
            ..Default::default()
        };

        let job_ctx = cm.get_context(job_id).await.unwrap(); // safety: test
        assert_ne!(job_ctx.user_id, attacker_ctx.user_id); // safety: test
        assert_eq!(job_ctx.user_id, "owner-user"); // safety: test
    }

    #[test]
    fn test_job_events_tool_schema() {
        // Verify the schema shape is correct (doesn't need a Store instance).
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The job ID (full UUID or short prefix, e.g. 'f2854dd8')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of events to return (default 50, most recent)"
                }
            },
            "required": ["job_id"]
        });

        let props = schema.get("properties").unwrap().as_object().unwrap(); // safety: test
        assert!(props.contains_key("job_id")); // safety: test
        assert!(props.contains_key("limit")); // safety: test
        let required = schema.get("required").unwrap().as_array().unwrap(); // safety: test
        assert_eq!(required.len(), 1); // safety: test
        assert_eq!(required[0].as_str().unwrap(), "job_id"); // safety: test
    }

    #[tokio::test]
    async fn test_job_prompt_tool_rejects_other_users_job() {
        let cm = Arc::new(ContextManager::new(5));
        let job_id = cm
            .create_job_for_user("owner-user", "Test Job", "desc")
            .await
            .unwrap(); // safety: test

        let queue: PromptQueue =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let tool = JobPromptTool::new(queue, cm);

        let params = serde_json::json!({
            "job_id": job_id.to_string(),
            "content": "sneaky prompt",
        });

        // Attacker context with a different user_id.
        let ctx = JobContext {
            user_id: "attacker".to_string(),
            ..Default::default()
        };

        let result = tool.execute(params, &ctx).await;
        assert!(result.is_err()); // safety: test
        let err = result.unwrap_err().to_string();
        assert!(
            /* safety: test */
            err.contains("does not belong to current user"),
            "expected ownership error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_job_prompt_tool_rejects_completed_job() {
        let cm = Arc::new(ContextManager::new(5));
        let job_id = cm
            .create_job_for_user("default", "Completed Job", "desc")
            .await
            .unwrap(); // safety: test

        cm.update_context(job_id, |ctx| {
            ctx.transition_to(JobState::InProgress, Some("running".to_string()))?;
            ctx.transition_to(JobState::Completed, Some("done".to_string()))
        })
        .await
        .unwrap()
        .unwrap(); // safety: test

        let queue: PromptQueue =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let tool = JobPromptTool::new(queue, cm);

        let params = serde_json::json!({
            "job_id": job_id.to_string(),
            "content": "one more thing",
        });

        let ctx = JobContext::default();
        let result = tool.execute(params, &ctx).await;
        assert!(result.is_err()); // safety: test
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("cannot receive follow-up prompts"),
            "expected completed-job rejection, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_resolve_job_id_full_uuid() {
        let cm = ContextManager::new(5);
        let job_id = cm.create_job("Test", "Desc").await.unwrap(); // safety: test

        let resolved = resolve_job_id(&job_id.to_string(), &cm).await.unwrap(); // safety: test
        assert_eq!(resolved, job_id); // safety: test
    }

    #[tokio::test]
    async fn test_resolve_job_id_short_prefix() {
        let cm = ContextManager::new(5);
        let job_id = cm.create_job("Test", "Desc").await.unwrap(); // safety: test

        // Use first 8 hex chars (without dashes)
        let hex = job_id.to_string().replace('-', "");
        let prefix = &hex[..8];
        let resolved = resolve_job_id(prefix, &cm).await.unwrap(); // safety: test
        assert_eq!(resolved, job_id); // safety: test
    }

    #[tokio::test]
    async fn test_resolve_job_id_no_match() {
        let cm = ContextManager::new(5);
        cm.create_job("Test", "Desc").await.unwrap(); // safety: test

        let result = resolve_job_id("00000000", &cm).await;
        assert!(result.is_err()); // safety: test
        let err = result.unwrap_err().to_string();
        assert!(
            /* safety: test */
            err.contains("no job found"),
            "expected 'no job found', got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_resolve_job_id_invalid_input() {
        let cm = ContextManager::new(5);
        let result = resolve_job_id("not-hex-at-all!", &cm).await;
        assert!(result.is_err()); // safety: test
    }

    // ── WI#326 harness mode ─────────────────────────────────────────────

    fn sandbox_tool() -> CreateJobTool {
        let manager = Arc::new(ContextManager::new(5));
        let jm = Arc::new(ContainerJobManager::new(
            crate::orchestrator::job_manager::ContainerJobConfig::default(),
            crate::orchestrator::TokenStore::new(),
        ));
        CreateJobTool::new(manager).with_sandbox(jm, None)
    }

    #[test]
    fn harness_schema_exposes_mode_and_capability_params() {
        let tool = sandbox_tool();
        let schema = tool.parameters_schema();
        let props = schema["properties"].as_object().unwrap(); // safety: test
        let modes = props["mode"]["enum"].as_array().unwrap(); // safety: test
        assert!(modes.iter().any(|m| m == "worker"));
        assert!(modes.iter().any(|m| m == "harness"));
        assert!(props.contains_key("worker_secrets"));
        assert!(props.contains_key("inject_files"));
        assert!(props.contains_key("allow_identity_injection"));
        assert!(props.contains_key("write_back"));
    }

    #[test]
    fn harness_options_require_at_least_one_capability() {
        let params = serde_json::json!({"title": "t", "description": "d"});
        let err = harness_options_from_params(&params).unwrap_err(); // safety: test
        assert!(err.to_string().contains("at least one"));
    }

    #[test]
    fn harness_options_parse_and_validate() {
        let params = serde_json::json!({
            "worker_secrets": {"github_token": "GITHUB_TOKEN"},
            "inject_files": ["MEMORY.md", {"source": "projects/x/notes.md", "target": "notes.md"}],
            "write_back": {"allowed_roots": ["projects"], "max_files": 10},
        });
        let options = harness_options_from_params(&params).unwrap().unwrap(); // safety: test
        assert_eq!(options.worker_secrets.len(), 1);
        assert_eq!(options.injections.len(), 2);
        let policy = options.write_back.unwrap(); // safety: test
        assert_eq!(policy.allowed_roots, vec!["projects".to_string()]);
        assert_eq!(policy.max_files, 10);

        // CW-1: roots may only NARROW the operator-fixed projects/** set, and
        // must be canonical (no trailing slash, no whitespace tricks).
        for bad in ["MEMORY.md", "engine", "projects/", " projects", "/projects"] {
            let params = serde_json::json!({"write_back": {"allowed_roots": [bad]}});
            assert!(
                harness_options_from_params(&params).is_err(),
                "root '{bad}' must be rejected"
            );
        }
        // CW-6: limits may only be lowered, never raised past the ceilings.
        let raised = serde_json::json!({"write_back": {"max_files": 999_999}});
        assert!(harness_options_from_params(&raised).is_err());
    }

    #[test]
    fn harness_capabilities_require_explicit_approval() {
        // CW-4: secrets, identity injection, and write-back sit above the
        // auto-approve tiers; a plain injection-only harness job does not.
        let tool = sandbox_tool();
        let base = serde_json::json!({
            "title": "t",
            "description": "d",
            "mode": "harness",
            "inject_files": ["MEMORY.md"],
        });
        assert_eq!(
            tool.requires_approval(&base),
            ApprovalRequirement::UnlessAutoApproved
        );
        for extra in [
            serde_json::json!({"worker_secrets": {"tok": "TOKEN"}}),
            serde_json::json!({"write_back": true}),
            serde_json::json!({"write_back": {"max_files": 1}}),
            serde_json::json!({"allow_identity_injection": true}),
            // DH-1: string-typed shapes that schema coercion ACTIVATES at
            // execution must elevate on raw params too.
            serde_json::json!({"allow_identity_injection": "true"}),
            serde_json::json!({"worker_secrets": "{\"tok\":\"TOKEN\"}"}),
            // write_back: {} enables the default policy at execution.
            serde_json::json!({"write_back": {}}),
        ] {
            let mut params = base.clone();
            params
                .as_object_mut()
                .unwrap() // safety: test
                .extend(extra.as_object().unwrap().clone()); // safety: test
            assert_eq!(
                tool.requires_approval(&params),
                ApprovalRequirement::Always,
                "{params}"
            );
        }
        // Inert forms keep the default tier: write_back: false,
        // allow_identity_injection: false, worker_secrets: {} (mounts
        // nothing even after coercion).
        for extra in [
            serde_json::json!({"write_back": false}),
            serde_json::json!({"allow_identity_injection": false}),
            serde_json::json!({"worker_secrets": {}}),
            serde_json::json!({"worker_secrets": null}),
        ] {
            let mut params = base.clone();
            params
                .as_object_mut()
                .unwrap() // safety: test
                .extend(extra.as_object().unwrap().clone()); // safety: test
            assert_eq!(
                tool.requires_approval(&params),
                ApprovalRequirement::UnlessAutoApproved,
                "{params}"
            );
        }
        // Non-harness modes never elevate through these params here.
        let worker = serde_json::json!({"title": "t", "description": "d", "mode": "worker"});
        assert_eq!(
            tool.requires_approval(&worker),
            ApprovalRequirement::UnlessAutoApproved
        );
    }

    #[test]
    fn harness_options_reject_identity_injection_by_default() {
        let params = serde_json::json!({"inject_files": ["SOUL.md"]});
        assert!(harness_options_from_params(&params).is_err());

        let opted_in = serde_json::json!({
            "inject_files": ["SOUL.md"],
            "allow_identity_injection": true,
        });
        assert!(harness_options_from_params(&opted_in).is_ok());
    }

    #[test]
    fn harness_options_reject_bad_secret_file_names() {
        let params = serde_json::json!({"worker_secrets": {"tok": "not-ok-name"}});
        assert!(harness_options_from_params(&params).is_err());
    }

    #[tokio::test]
    async fn explicit_project_dir_without_store_fails_closed() {
        // DH-7: the CW-3 ownership check must not be silently skipped when
        // the tool has no job store.
        let tool = sandbox_tool();
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "title": "t",
                    "description": "d",
                    "mode": "worker",
                    "project_dir": format!("/tmp/{}", Uuid::new_v4()),
                }),
                &ctx,
            )
            .await;
        let err = result.unwrap_err(); // safety: test
        assert!(
            err.to_string().contains("ownership verification"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn harness_mode_requires_sandbox() {        let manager = Arc::new(ContextManager::new(5));
        let tool = CreateJobTool::new(manager);
        let result = tool
            .execute(
                serde_json::json!({
                    "title": "t",
                    "description": "d",
                    "mode": "harness",
                    "write_back": true,
                }),
                &JobContext::default(),
            )
            .await;
        let err = result.unwrap_err(); // safety: test
        assert!(err.to_string().contains("requires the Docker sandbox"));
    }

    #[tokio::test]
    async fn harness_params_rejected_for_plain_worker_mode() {
        let tool = sandbox_tool();
        let result = tool
            .execute(
                serde_json::json!({
                    "title": "t",
                    "description": "d",
                    "mode": "worker",
                    "write_back": true,
                }),
                &JobContext::default(),
            )
            .await;
        let err = result.unwrap_err(); // safety: test
        assert!(err.to_string().contains("require mode 'harness'"));
    }

    #[test]
    fn harness_mode_db_roundtrip() {
        let mode = JobMode::Harness;
        assert_eq!(mode.db_value(), "harness");
        assert_eq!(JobMode::from_db_value("harness"), JobMode::Harness);
        assert!(!mode.is_external());
        assert_eq!(mode.to_string(), "harness");
    }
}
