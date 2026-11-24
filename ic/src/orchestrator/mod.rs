//! Orchestrator for managing sandboxed worker containers.
//!
//! # BOUNDARY (do not cross)
//!
//! This module is the **live worker path**: per-job worker containers created
//! over the Docker-compatible API via bollard (`job_manager.rs`, containers
//! labeled `lunarwing.job_id`). It is NOT the generic sandbox — that lives in
//! `crate::sandbox` (podman/docker CLI runner, `lunarwing.sandbox` labels,
//! consumed by the shell tool). Never wire `SandboxManager`/`CliRunner` into
//! the orchestrator, and never route shell execution through
//! `ContainerJobManager`. The ONLY shared component is the reaper
//! (`reaper.rs`), which scans both label namespaces separately.
//!
//! The orchestrator runs in the main agent process and provides:
//! - An internal HTTP API for worker communication (LLM proxy, status, secrets)
//! - Per-job bearer token authentication
//! - Container lifecycle management (create, monitor, stop)
//!
//! ```text
//! ┌───────────────────────────────────────────────┐
//! │              Orchestrator                       │
//! │                                                 │
//! │  Internal API (default :50051, configurable)    │
//! │    POST /worker/{id}/llm/complete               │
//! │    POST /worker/{id}/llm/complete_with_tools    │
//! │    GET  /worker/{id}/job                        │
//! │    GET  /worker/{id}/credentials                │
//! │    GET  /worker/{id}/mcp/credentials            │
//! │    POST /worker/{id}/status                     │
//! │    POST /worker/{id}/complete                   │
//! │                                                 │
//! │  ContainerJobManager                            │
//! │    create_job() -> container + token             │
//! │    stop_job()                                    │
//! │    list_jobs()                                   │
//! │                                                 │
//! │  TokenStore                                     │
//! │    per-job bearer tokens (in-memory only)       │
//! │    per-job credential grants (in-memory only)   │
//! └───────────────────────────────────────────────┘
//! ```

pub mod api;
pub mod auth;
pub mod cron_worker;
pub mod external_worker;
pub mod harness;
pub mod job_cancel;
pub mod job_manager;
pub mod reaper;

pub use api::OrchestratorApi;
pub use auth::{CredentialGrant, McpCredentialGrant, TokenStore};
pub use external_worker::{
    ExternalTaskResult, ExternalWorkerManager, WorkerProbeState, probe_configured_workers,
};
pub use harness::{
    HarnessError, HarnessJobOptions, InjectedFileProvenance, InjectionManifest, InjectionSpec,
    PromotionRecord, PromotionRejection, PromotionSummary, ResolvedSecretFile, SecretFileMount,
    SecretValueRedactor, WriteBackPolicy,
};
pub use job_manager::{
    CompletionResult, ContainerHandle, ContainerJobConfig, ContainerJobManager,
    ContainerJobRequest, HarnessJobRequest, HarnessJobState, JobMode, snapshot_worker_mcp,
};
pub use reaper::{ReaperConfig, SandboxReaper, sweep_cli_sandboxes_once};

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

use crate::channels::web::types::SseEvent;
use crate::db::Database;
use crate::llm::LlmProvider;
use crate::secrets::SecretsStore;

/// Resolve the orchestrator port from the `ORCHESTRATOR_PORT` environment
/// variable, falling back to 50051.
fn resolve_orchestrator_port() -> u16 {
    std::env::var("ORCHESTRATOR_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50051)
}

/// Result of orchestrator setup, containing all handles needed by the agent.
pub struct OrchestratorSetup {
    pub container_job_manager: Option<Arc<ContainerJobManager>>,
    pub external_worker_manager: Option<Arc<ExternalWorkerManager>>,
    pub job_event_tx: Option<broadcast::Sender<(Uuid, String, SseEvent)>>,
    pub prompt_queue: Arc<Mutex<HashMap<Uuid, VecDeque<api::PendingPrompt>>>>,
    pub docker_status: crate::sandbox::DockerStatus,
    /// Runtime CLI selected for the generic sandbox (WI#871). `None` when the
    /// sandbox is disabled or no runtime in the probe order is usable.
    pub sandbox_runtime: Option<crate::sandbox::ContainerRuntime>,
    /// User-visible warning when the sandbox runtime is usable but full-job
    /// (worker) containers cannot start (WI#876). `None` when both are fine,
    /// when the sandbox is disabled, or when no runtime is usable at all.
    pub full_job_warning: Option<String>,
}

/// One startup diagnostic for the sandbox runtime (WI#871).
///
/// The generic sandbox drives the podman or docker CLI directly, so a missing
/// Docker daemon is not "sandbox disabled". Report the selected runtime by
/// name, and say `sandbox disabled` only when every probed runtime failed.
fn log_sandbox_runtime_diagnostic(
    selection: &crate::sandbox::SandboxRuntimeDetection,
    docker_status: crate::sandbox::DockerStatus,
) {
    match selection.runtime {
        Some(runtime) => {
            let job_containers = if docker_status.is_ok() {
                "job containers: available"
            } else {
                "job containers: unavailable (no Docker-compatible API)"
            };
            tracing::info!(
                runtime = runtime.binary(),
                "{}; {}",
                selection.summary(),
                job_containers
            );
        }
        None => tracing::warn!("{}", selection.summary()),
    }
}

/// Dependencies for the WI#324 proxied tool path: when present, external
/// workers may invoke their allowlisted in-process tools over the task
/// WebSocket control channel. Hooks are required so proxied calls pass the
/// same `BeforeToolCall` operator policy as every other autonomous path
/// (MR !1506 review CQ-1).
pub struct ToolProxyDeps {
    pub tools: Arc<crate::tools::ToolRegistry>,
    pub safety: Arc<crate::safety::SafetyLayer>,
    pub hooks: Arc<crate::hooks::HookRegistry>,
}

/// Detect Docker availability, create the container job manager, and start
/// the orchestrator internal API in the background.
pub async fn setup_orchestrator(
    config: &crate::config::Config,
    llm: &Arc<dyn LlmProvider>,
    db: Option<&Arc<dyn Database>>,
    secrets_store: Option<&Arc<dyn SecretsStore + Send + Sync>>,
    context_manager: Option<Arc<crate::context::ContextManager>>,
    tool_proxy_deps: Option<ToolProxyDeps>,
    workspace_resolver: Option<Arc<dyn crate::tools::builtin::memory::WorkspaceResolver>>,
) -> OrchestratorSetup {
    let prompt_queue = Arc::new(Mutex::new(
        HashMap::<Uuid, VecDeque<api::PendingPrompt>>::new(),
    ));

    let (docker_status, sandbox_runtime, full_job_warning) = if config.sandbox.enabled {
        let docker = crate::sandbox::check_docker().await;
        // WI#871: select the runtime the generic sandbox will actually drive
        // (podman-first) and name it, instead of reporting Docker's absence as
        // a disabled sandbox on a healthy podman-only host.
        let selection =
            crate::sandbox::check_sandbox_runtime(config.sandbox.resolved_runtime()).await;
        log_sandbox_runtime_diagnostic(&selection, docker.status);
        // WI#876: a working sandbox on a host with no Docker-compatible API
        // still cannot start full-job (worker) containers. That used to be an
        // info log only, so surface it to the boot screen and the channel.
        let full_job_warning = selection.full_job_warning(docker.status);
        if let Some(warning) = &full_job_warning {
            tracing::warn!("{warning}");
        }
        (docker.status, selection.runtime, full_job_warning)
    } else {
        (crate::sandbox::DockerStatus::Disabled, None, None)
    };

    // Create broadcast channel for job events (shared by container and external workers).
    let has_external_workers = !config.external_workers.is_empty();
    let needs_event_tx = (config.sandbox.enabled && docker_status.is_ok()) || has_external_workers;
    let job_event_tx: Option<broadcast::Sender<(Uuid, String, SseEvent)>> = if needs_event_tx {
        let (tx, _) = broadcast::channel(256);
        Some(tx)
    } else {
        None
    };

    let container_job_manager = if config.sandbox.enabled && docker_status.is_ok() {
        let token_store = TokenStore::new();
        let orchestrator_port = resolve_orchestrator_port();
        let job_config = ContainerJobConfig {
            image: config.sandbox.image.clone(),
            memory_limit_mb: config.sandbox.memory_limit_mb,
            cpu_shares: config.sandbox.cpu_shares,
            orchestrator_port,
        };
        let mut jm_inner = ContainerJobManager::new(job_config, token_store.clone());
        if let (Some(etx), Some(cm)) = (&job_event_tx, &context_manager) {
            jm_inner = jm_inner.with_completion_deps(etx.clone(), Arc::clone(cm));
        }
        // WI#326: harness write-back promotion needs the tenant workspace;
        // the audit event persists to the sandbox job event log when a DB
        // is available.
        if let Some(resolver) = workspace_resolver {
            jm_inner = jm_inner.with_write_back_deps(resolver, db.cloned());
        }
        let jm = Arc::new(jm_inner);

        // WI#326 CW-8: harness dirs hold injection snapshots (memory and
        // opted-in identity documents) and staged writes. Sweep any left
        // behind by a crash/restart; the sweep only removes dirs with no
        // live labeled container and a grace period, so in-flight dispatch
        // is never disturbed.
        {
            let jm = Arc::clone(&jm);
            tokio::spawn(async move {
                jm.sweep_orphaned_harness_dirs().await;
            });
        }

        let orchestrator_state = api::OrchestratorState {
            llm: Arc::clone(llm),
            job_manager: Arc::clone(&jm),
            token_store,
            job_event_tx: job_event_tx.clone(),
            prompt_queue: Arc::clone(&prompt_queue),
            store: db.cloned(),
            secrets_store: secrets_store.cloned(),
            user_id: "default".to_string(),
            job_owner_cache: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
            context_manager: context_manager.clone(),
        };

        tokio::spawn(async move {
            if let Err(e) = OrchestratorApi::start(orchestrator_state, orchestrator_port).await {
                tracing::error!("Orchestrator API failed: {}", e);
            }
        });

        Some(jm)
    } else {
        None
    };

    // External worker manager (independent of Docker)
    let external_worker_manager = if has_external_workers {
        let mut mgr = ExternalWorkerManager::new(config.external_workers.clone());
        if let (Some(etx), Some(cm)) = (&job_event_tx, &context_manager) {
            mgr = mgr.with_event_deps(etx.clone(), Arc::clone(cm));
        }
        if let Some(store) = db {
            mgr = mgr.with_store(Arc::clone(store));
        }
        if let Some(deps) = tool_proxy_deps {
            mgr = mgr.with_tool_proxy(external_worker::ToolProxy::new(
                deps.tools,
                deps.safety,
                deps.hooks,
            ));
        }
        Some(Arc::new(mgr))
    } else {
        None
    };

    OrchestratorSetup {
        container_job_manager,
        external_worker_manager,
        job_event_tx,
        prompt_queue,
        docker_status,
        sandbox_runtime,
        full_job_warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::helpers::lock_env;

    #[test]
    fn resolve_orchestrator_port_from_env() {
        let _guard = lock_env();

        // Safety: env-var mutation requires unsafe in edition 2024;
        // lock_env() serializes concurrent access from other test threads.

        // Absent env var → default 50051
        unsafe { std::env::remove_var("ORCHESTRATOR_PORT") };
        assert_eq!(resolve_orchestrator_port(), 50051);

        // Valid custom port
        unsafe { std::env::set_var("ORCHESTRATOR_PORT", "50052") };
        assert_eq!(resolve_orchestrator_port(), 50052);

        // Non-numeric value → fallback to default
        unsafe { std::env::set_var("ORCHESTRATOR_PORT", "not_a_port") };
        assert_eq!(resolve_orchestrator_port(), 50051);

        // Out of u16 range → fallback to default
        unsafe { std::env::set_var("ORCHESTRATOR_PORT", "99999") };
        assert_eq!(resolve_orchestrator_port(), 50051);

        // Cleanup
        unsafe { std::env::remove_var("ORCHESTRATOR_PORT") };
    }
}
