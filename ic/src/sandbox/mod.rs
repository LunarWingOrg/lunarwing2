//! Generic execution sandbox for secure command execution (podman-first).
//!
//! This module provides a complete sandboxing solution for running untrusted commands:
//! - **Container isolation**: Commands run in ephemeral containers driven by the
//!   `podman` (preferred) or `docker` CLI — no daemon socket required, mirroring
//!   the rootless pattern the mt-admin external-worker tooling uses
//! - **Network proxy**: `none` (default) disables egress; `proxy-enforced`
//!   retains `--network none` and exposes only a Unix relay to the validating
//!   proxy. Legacy `proxy` remains advisory (env-var based).
//! - **Credential injection**: Secrets are injected by the proxy, never exposed in containers
//! - **Resource limits**: Memory, CPU, and timeout enforcement
//!
//! # BOUNDARY: generic sandbox vs. orchestrator worker containers
//!
//! There are TWO container subsystems in this codebase. Do not mix them:
//!
//! - **This module** (`crate::sandbox`): the *generic sandbox*. It runs one
//!   short-lived hardened container per command via the podman/docker CLI
//!   ([`cli_runner::CliRunner`]) and is consumed by the shell tool's
//!   fail-closed execution path. Containers are labeled
//!   [`cli_runner::SANDBOX_CONTAINER_LABEL`] (`lunarwing.sandbox`).
//! - **The orchestrator** (`crate::orchestrator::job_manager::ContainerJobManager`):
//!   the *live worker path*. It creates long-lived per-job worker containers
//!   over the Docker-compatible API via bollard (`connect_docker`), with job
//!   auth tokens, the orchestrator HTTP API, and the `lunarwing.job_id` label
//!   namespace.
//!
//! Never wire `SandboxManager`/`CliRunner` into the orchestrator, and never
//! route shell execution through `ContainerJobManager`. The ONLY shared
//! component is the reaper (`crate::orchestrator::SandboxReaper`), which
//! scans both label namespaces separately.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                           Sandbox System                                     │
//! │                                                                              │
//! │  ┌─────────────────────────────────────────────────────────────────────┐    │
//! │  │                        SandboxManager                                │    │
//! │  │                                                                      │    │
//! │  │  • Coordinates container execution (CLI runner)                      │    │
//! │  │  • Manages proxy lifecycle (proxy network mode only)                 │    │
//! │  │  • Enforces resource limits                                          │    │
//! │  └─────────────────────────────────────────────────────────────────────┘    │
//! │           │                              │                                   │
//! │           ▼                              ▼                                   │
//! │  ┌──────────────────┐          ┌───────────────────┐                        │
//! │  │   CLI Runner     │          │   Network Proxy   │                        │
//! │  │  (podman/docker) │          │                   │                        │
//! │  │                  │          │  • Allowlist      │                        │
//! │  │  • podman run    │◀────────▶│  • Credentials    │                        │
//! │  │  • kill/rm       │          │  • Logging        │                        │
//! │  └──────────────────┘          └───────────────────┘                        │
//! │           │                              │                                   │
//! │           ▼                              ▼                                   │
//! │  ┌──────────────────┐          ┌───────────────────┐                        │
//! │  │   Containers     │          │     Internet      │                        │
//! │  │  (--rm, labeled) │          │   (allowed hosts) │                        │
//! └─────────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Sandbox Policies
//!
//! | Policy | Filesystem | Network | Use Case |
//! |--------|------------|---------|----------|
//! | `ReadOnly` | Read workspace | Proxy allowlist or none | Explore code, fetch docs |
//! | `WorkspaceWrite` | Read/write workspace | Proxy allowlist or none | Build software, run tests |
//! | `FullAccess` | Full host | Full | Direct execution (no sandbox) |
//!
//! # Example
//!
//! ```rust,no_run
//! use lunarwing::sandbox::{SandboxManager, SandboxManagerBuilder, SandboxPolicy};
//! use std::collections::HashMap;
//! use std::path::Path;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let manager = SandboxManagerBuilder::new()
//!     .enabled(true)
//!     .policy(SandboxPolicy::WorkspaceWrite)
//!     .build();
//!
//! manager.initialize().await?;
//!
//! let result = manager.execute(
//!     "cargo build --release",
//!     Path::new("/workspace/my-project"),
//!     HashMap::new(),
//! ).await?;
//!
//! println!("Exit code: {}", result.exit_code);
//! println!("Output: {}", result.output);
//!
//! manager.shutdown().await;
//! # Ok(())
//! # }
//! ```
//!
//! # Security Properties
//!
//! - **No credentials in containers**: Environment variables with secrets never enter containers
//! - **Network isolation**: Proxy mode routes traffic through the proxy (validated
//!   domains only — advisory, env-var based); `none` mode removes the network entirely
//! - **Non-root execution**: keep-id or the workspace owner's numeric UID/GID
//! - **Read-only root**: Container filesystem is read-only (except workspace mount)
//! - **Capability dropping**: All Linux capabilities dropped, only essential ones added back
//! - **Auto-cleanup**: `--rm`, cancellation-safe supervisor, independent CLI reaper
//! - **Timeout enforcement**: Commands are killed after the timeout

pub mod cli_runner;
pub mod config;
pub mod container;
pub mod detect;
pub mod error;
pub mod manager;
pub mod proxy;

pub use cli_runner::{
    CliRunner, ContainerRuntime, KeepIdMode, SANDBOX_CONTAINER_LABEL, inflight_sandbox_containers,
};
pub use config::{
    ResourceLimits, SandboxConfig, SandboxNetworkMode, SandboxPolicy, SandboxRuntime,
};
pub use container::{ContainerOutput, ContainerRunner, connect_docker};
pub use detect::{
    DockerDetection, DockerStatus, Platform, check_docker, check_podman, podman_install_hint,
};
pub use error::{Result, SandboxError};
pub use manager::{ExecOutput, SandboxManager, SandboxManagerBuilder};
pub use proxy::{
    CredentialResolver, DefaultPolicyDecider, DomainAllowlist, EnvCredentialResolver, HttpProxy,
    NetworkDecision, NetworkPolicyDecider, NetworkProxyBuilder, NetworkRequest,
};

/// Default allowlist getter (re-export for convenience).
pub fn default_allowlist() -> Vec<String> {
    config::default_allowlist()
}

/// Default credential mappings getter (re-export for convenience).
pub fn default_credential_mappings() -> Vec<crate::secrets::CredentialMapping> {
    config::default_credential_mappings()
}
