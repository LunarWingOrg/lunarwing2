use crate::config::helpers::{optional_env, parse_bool_env, parse_optional_env, parse_string_env};
use crate::error::ConfigError;
pub use lunarwing_common::PromptContextLimits;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tracing;

/// Sandbox configuration (generic podman-first sandbox).
#[derive(Debug, Clone)]
pub struct SandboxModeConfig {
    /// Whether the sandbox is enabled.
    pub enabled: bool,
    /// Sandbox policy: "readonly", "workspace_write", or "full_access".
    pub policy: String,
    /// Container runtime CLI: "auto" (podman preferred), "podman", or "docker".
    pub runtime: String,
    /// Network egress mode for sandboxed policies: "none" (default),
    /// "proxy-enforced" (isolated allowlist web access), or legacy
    /// advisory "proxy".
    pub network: String,
    /// Force sandboxed shell execution even in dev mode (ALLOW_LOCAL_TOOLS).
    /// Default false: dev boxes keep direct host execution.
    pub enforce: bool,
    /// Explicit opt-in for `FullAccess` policy.
    ///
    /// When `policy` is `full_access` but this is `false`, the policy is
    /// downgraded to `workspace_write` with a loud error log. This prevents
    /// accidental host-level command execution from a single misconfigured
    /// env var.
    pub allow_full_access: bool,
    /// Command timeout in seconds.
    pub timeout_secs: u64,
    /// Memory limit in megabytes.
    pub memory_limit_mb: u64,
    /// CPU shares (relative weight).
    pub cpu_shares: u32,
    /// Container image for the sandbox.
    pub image: String,
    /// Whether to auto-pull the image if not found.
    pub auto_pull_image: bool,
    /// Additional domains to allow through the network proxy.
    pub extra_allowed_domains: Vec<String>,
    /// How often the reaper scans for orphaned containers (seconds). Default: 300 (5 min).
    pub reaper_interval_secs: u64,
    /// Containers older than this with no active job are reaped (seconds). Default: 600 (10 min).
    pub orphan_threshold_secs: u64,
    /// The sandbox-visible daemon log copy and its `/tenant-logs` mount
    /// (WI#287): `SANDBOX_TENANT_LOGS=off|info|debug`, default `info`. Env
    /// only (read before the database is up, by tracing init), strictly
    /// parsed.
    pub tenant_logs: crate::sandbox::TenantLogsMode,
}

impl Default for SandboxModeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            policy: "readonly".to_string(),
            runtime: "auto".to_string(),
            network: "none".to_string(),
            enforce: false,
            allow_full_access: false,
            timeout_secs: 120,
            memory_limit_mb: 2048,
            cpu_shares: 1024,
            image: "lunarwing-worker:latest".to_string(),
            auto_pull_image: true,
            extra_allowed_domains: Vec::new(),
            reaper_interval_secs: 300,
            orphan_threshold_secs: 600,
            tenant_logs: crate::sandbox::TenantLogsMode::Info,
        }
    }
}

impl SandboxModeConfig {
    pub(crate) fn resolve(settings: &crate::settings::Settings) -> Result<Self, ConfigError> {
        let ss = &settings.sandbox;

        let extra_domains = optional_env("SANDBOX_EXTRA_DOMAINS")?
            .map(|s| s.split(',').map(|d| d.trim().to_string()).collect())
            .unwrap_or_else(|| {
                if ss.extra_allowed_domains.is_empty() {
                    Vec::new()
                } else {
                    ss.extra_allowed_domains.clone()
                }
            });

        // reaper/orphan fields have no Settings counterpart — env > default only.
        let reaper_interval_secs: u64 = parse_optional_env("SANDBOX_REAPER_INTERVAL_SECS", 300)?;
        let orphan_threshold_secs: u64 = parse_optional_env("SANDBOX_ORPHAN_THRESHOLD_SECS", 600)?;

        // Validate that reaper timings are non-zero to prevent tokio::time::interval panics
        if reaper_interval_secs == 0 {
            return Err(ConfigError::InvalidValue {
                key: "SANDBOX_REAPER_INTERVAL_SECS".to_string(),
                message: "must be greater than 0".to_string(),
            });
        }

        if orphan_threshold_secs == 0 {
            return Err(ConfigError::InvalidValue {
                key: "SANDBOX_ORPHAN_THRESHOLD_SECS".to_string(),
                message: "must be greater than 0".to_string(),
            });
        }

        Ok(Self {
            enabled: parse_bool_env("SANDBOX_ENABLED", ss.enabled)?,
            policy: parse_string_env("SANDBOX_POLICY", ss.policy.clone())?,
            runtime: parse_string_env("SANDBOX_RUNTIME", ss.runtime.clone())?,
            network: parse_string_env("SANDBOX_NETWORK", ss.network.clone())?,
            // enforce: env > settings, default false.
            enforce: parse_bool_env("SANDBOX_ENFORCE", ss.enforce)?,
            // allow_full_access has no Settings counterpart — env > default only.
            allow_full_access: parse_bool_env("SANDBOX_ALLOW_FULL_ACCESS", false)?,
            timeout_secs: parse_optional_env("SANDBOX_TIMEOUT_SECS", ss.timeout_secs)?,
            memory_limit_mb: parse_optional_env("SANDBOX_MEMORY_LIMIT_MB", ss.memory_limit_mb)?,
            cpu_shares: parse_optional_env("SANDBOX_CPU_SHARES", ss.cpu_shares)?,
            image: parse_string_env("SANDBOX_IMAGE", ss.image.clone())?,
            auto_pull_image: parse_bool_env("SANDBOX_AUTO_PULL", ss.auto_pull_image)?,
            extra_allowed_domains: extra_domains,
            reaper_interval_secs,
            orphan_threshold_secs,
            // No Settings counterpart: env > default only.
            tenant_logs: parse_optional_env(
                "SANDBOX_TENANT_LOGS",
                crate::sandbox::TenantLogsMode::Info,
            )?,
        })
    }

    /// Convert to SandboxConfig for the sandbox module.
    ///
    /// If `policy` is `FullAccess` but `allow_full_access` is `false`,
    /// the policy is downgraded to `WorkspaceWrite` and an error is logged.
    pub fn to_sandbox_config(&self) -> crate::sandbox::SandboxConfig {
        use crate::sandbox::SandboxPolicy;
        use std::time::Duration;

        let mut policy = self.policy.parse().unwrap_or(SandboxPolicy::ReadOnly);

        // Double opt-in guard: FullAccess requires SANDBOX_ALLOW_FULL_ACCESS=true
        if policy == SandboxPolicy::FullAccess && !self.allow_full_access {
            tracing::error!(
                "SANDBOX_POLICY=full_access is set but SANDBOX_ALLOW_FULL_ACCESS is not \
                 set to 'true'. FullAccess bypasses the container and runs commands \
                 directly on the host. Downgrading to WorkspaceWrite for safety. Set \
                 SANDBOX_ALLOW_FULL_ACCESS=true to explicitly enable FullAccess."
            );
            policy = SandboxPolicy::WorkspaceWrite;
        }

        let runtime = self.runtime.parse().unwrap_or_else(|e| {
            tracing::error!(
                value = %self.runtime,
                error = %e,
                "Invalid SANDBOX_RUNTIME; falling back to 'auto' (podman preferred)"
            );
            crate::sandbox::SandboxRuntime::Auto
        });

        let network = self.network.parse().unwrap_or_else(|e| {
            tracing::error!(
                value = %self.network,
                error = %e,
                "Invalid SANDBOX_NETWORK; falling back to 'none'"
            );
            crate::sandbox::SandboxNetworkMode::None
        });

        let mut allowlist = crate::sandbox::default_allowlist();
        allowlist.extend(self.extra_allowed_domains.clone());

        crate::sandbox::SandboxConfig {
            enabled: self.enabled,
            policy,
            allow_full_access: self.allow_full_access,
            timeout: Duration::from_secs(self.timeout_secs),
            memory_limit_mb: self.memory_limit_mb,
            cpu_shares: self.cpu_shares,
            network_allowlist: allowlist,
            image: self.image.clone(),
            auto_pull_image: self.auto_pull_image,
            proxy_port: 0, // Auto-assign
            runtime,
            network,
            protected_paths: crate::sandbox::mount_guard::resolve_protected_paths(),
            tenant_logs: self.tenant_logs,
        }
    }
}

/// A single endpoint for a named external worker (URL + optional auth).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerEndpoint {
    pub url: String,
    // Secret-bearing: skipped by serde so JSON/API transports cannot expose it;
    // `Debug` auto-redacts via secrecy's `[REDACTED]`. `Settings::save_toml`
    // explicitly re-injects it into the on-disk config, while `merge_from`
    // preserves it across its JSON merge.
    #[serde(skip_serializing, default)]
    pub auth_token: Option<SecretString>,
    pub weight: Option<u32>,
}

/// Load balancing strategy for multi-instance workers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LoadBalanceStrategy {
    #[default]
    RoundRobin,
    LeastConnections,
}

/// Operating mode for an external worker (WI#325).
///
/// - `Task` (default): bounded, on-demand dispatches via `create_job`.
/// - `Cron`: mission-like, long-running supervised operation. A host-side
///   supervisor dispatches a structured `cron.tick` job on a cron schedule,
///   watches heartbeats, and re-dispatches a fresh bounded job when a tick
///   job dies or wedges. The mission's lifetime is the chain plus the
///   worker's durable `/workspace` state, never any one process; the
///   in-process mission lifecycle machine (WI#311/#317) never sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExternalWorkerMode {
    #[default]
    Task,
    Cron,
}

/// Cron-mission configuration for an `ExternalWorkerMode::Cron` worker.
///
/// All durations are optional; defaults are chosen so a plain
/// `mode = "cron"` + `cron.schedule` + `cron.goal` block is enough.
/// Unknown keys are rejected (`deny_unknown_fields`) so a misspelled setting
/// cannot silently fall back to defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CronWorkerConfig {
    /// Cron expression (5, 6, or 7 fields; normalized like routines).
    pub schedule: String,
    /// Mission goal — the durable instruction every tick job works toward.
    pub goal: String,
    /// Stable mission identifier surfaced in every `cron.tick` event and in
    /// job metadata. Defaults to `cron-worker-<name>` when absent so the
    /// herd sees the same mission across daemon restarts.
    #[serde(default)]
    pub mission_id: Option<String>,
    /// Heartbeat watchdog for a tick job (seconds). If a tick job shows no
    /// genuine progress for this long, the supervisor cancels it and
    /// re-dispatches a fresh bounded job. Default: 900 (15 min).
    #[serde(default)]
    pub heartbeat_timeout_secs: Option<u64>,
    /// Per-tick job deadline in milliseconds. Replaces the standard bounded
    /// `timeout_ms` for this mode — the mission itself is unbounded because
    /// the supervisor always holds the lease and chains fresh jobs.
    /// Default: [`Self::MAX_TICK_TIMEOUT_MS`] (the host cap, 4h), so
    /// a tick link genuinely outlives the worker's ordinary bounded deadline;
    /// the heartbeat watchdog bounds *silence*, not work.
    #[serde(default)]
    pub tick_timeout_ms: Option<u64>,
    /// Maximum re-dispatches of a single tick after wedge/failure before the
    /// tick is abandoned (the mission continues at the next scheduled fire).
    /// Default: 3. Capped at [`Self::MAX_REENTRIES_LIMIT`].
    #[serde(default)]
    pub max_reentries: Option<u32>,
    /// Explicit escape hatch for schedules with a fire gap below
    /// [`Self::MIN_SCHEDULE_INTERVAL_SECS`] (e.g. a per-minute smoke
    /// mission). Sub-minute schedules multiply LLM spend and `job_events`
    /// growth, so the floor is enforced unless this is set.
    #[serde(default)]
    pub allow_subminute_schedule: Option<bool>,
    /// Per-mission workspace directory sent as the tick job's
    /// `context.project_dir` (WI#325). Relative paths resolve under the
    /// worker's `WORKSPACE_ROOT`; updated bridges create missing cron-only
    /// directories without following symlinks. Older worker images require
    /// the directory to be pre-created before enabling the schedule. Default:
    /// `missions/<mission_id>`, so a mission never shares the worker root
    /// with ordinary dispatches unless the operator opts into that.
    /// Host-side validated at load: must be a relative path with no `..`
    /// segments. Note the kimi/pebble workspace lock treats nested paths as
    /// overlapping, so ordinary dispatches that use the worker root still
    /// contend with this mission — give ordinary tasks their own
    /// non-overlapping `project_dir` on a cron-mode worker.
    #[serde(default)]
    pub project_dir: Option<String>,
    /// IANA timezone for the schedule (e.g. "America/New_York"), resolved
    /// with the same normalization the routine engine applies
    /// (`agent::routine::next_cron_fire`). Default: UTC. An unparseable zone
    /// rejects the worker at load.
    #[serde(default)]
    pub timezone: Option<String>,
    /// Tool-proxy authority for cron **tick** jobs (WI#324, MR !1506 CQ-10).
    /// Deliberately separate from the worker-level `tool_allowlist`: a cron
    /// mission fires unattended on a schedule, so tick jobs never inherit the
    /// interactive list — grant the schedule its own (usually narrower) set
    /// here. Default: empty — every `tool_call` from a tick job is denied.
    /// Sanitized with the same rules as the worker-level list at manager load
    /// (phase-1 positive set, denylist drops, cleartext strip).
    /// MR !1549 EF-1: a cron worker's worker-level list is stripped at
    /// manager load (one container = one authority domain), so this is the
    /// ONLY proxy authority a cron worker container holds — ordinary
    /// dispatches to it get no proxied tools either.
    /// WI#396: when two cron entries share one endpoint listener with
    /// differing lists, the manager strips every list on that listener at
    /// load (fail closed); identical lists stay with a `warn!`.
    /// Serialized only when non-empty (MR !1549 EF-6): `CronWorkerConfig` is
    /// `deny_unknown_fields`, so an older binary rejects any config carrying
    /// this field at all — write-backs must not introduce
    /// `tool_allowlist = []` or a rollback could no longer boot the config.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_allowlist: Vec<String>,
    /// Per-mission retention window for persisted tick rows (WI#370): after
    /// each tick the supervisor deletes this mission's tick rows (and their
    /// `job_events`) beyond the newest `retain_last_ticks`, always keeping
    /// the persisted last-job anchor. The window counts JOB ROWS, not
    /// distinct ticks — a re-dispatched link carries the same prompt prefix,
    /// so one tick with reentries spends several rows of the window. Bounds
    /// the unbounded `agent_jobs`/`job_events` growth a schedule produces
    /// (up to 1,440 rows/day at the 60s floor before reentries).
    /// Default: 50; floor: 1 (a window of 0 would keep nothing but the
    /// anchor — use the anchor's own lifecycle if no tick history is
    /// wanted); cap: 10,000. Serialized only when set
    /// (`CronWorkerConfig` is `deny_unknown_fields`, so an older binary
    /// rejects any config carrying this key — write-backs must not
    /// introduce it, matching the `tool_allowlist` rollback rule).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_last_ticks: Option<u32>,
}

impl CronWorkerConfig {
    pub const DEFAULT_HEARTBEAT_TIMEOUT_SECS: u64 = 900;
    pub const DEFAULT_MAX_REENTRIES: u32 = 3;
    /// Minimum heartbeat watchdog window (seconds). Below this the watcher
    /// polls so fast that a slow-but-alive job looks wedged.
    pub const MIN_HEARTBEAT_TIMEOUT_SECS: u64 = 5;
    /// Hard cap on per-tick re-dispatches; bounds LLM spend from a
    /// crash-looping worker.
    pub const MAX_REENTRIES_LIMIT: u32 = 16;
    /// Minimum per-tick job deadline (ms).
    pub const MIN_TICK_TIMEOUT_MS: u64 = 1_000;
    /// Maximum per-tick job deadline (ms). Matches the default
    /// `KIMI_MAX_TIMEOUT_MS` cap of the kimi worker bridge (4h), applied by
    /// the host to all workers. Operators lowering Kimi's configurable cap
    /// must also lower `tick_timeout_ms`; the host cannot discover that cap.
    pub const MAX_TICK_TIMEOUT_MS: u64 = 14_400_000;
    /// Minimum gap between scheduled fires (seconds) unless
    /// `allow_subminute_schedule` is set.
    pub const MIN_SCHEDULE_INTERVAL_SECS: u64 = 60;
    /// Default per-mission retention of persisted tick rows (WI#370).
    pub const DEFAULT_RETAIN_LAST_TICKS: u32 = 50;
    /// Floor on `retain_last_ticks` (LW-9): a window of 0 would keep nothing
    /// but the anchor — the anchor is always kept anyway, so 0 is almost
    /// certainly a misconfiguration and is refused.
    pub const MIN_RETAIN_LAST_TICKS: u32 = 1;
    /// Cap on `retain_last_ticks`: retention exists to bound growth, and a
    /// window beyond this is almost certainly a typo.
    pub const MAX_RETAIN_LAST_TICKS_LIMIT: u32 = 10_000;

    pub fn heartbeat_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.heartbeat_timeout_secs
                .unwrap_or(Self::DEFAULT_HEARTBEAT_TIMEOUT_SECS),
        )
    }

    pub fn max_reentries(&self) -> u32 {
        self.max_reentries.unwrap_or(Self::DEFAULT_MAX_REENTRIES)
    }

    /// Effective per-mission tick-row retention window (WI#370).
    pub fn retain_last_ticks(&self) -> u32 {
        self.retain_last_ticks
            .unwrap_or(Self::DEFAULT_RETAIN_LAST_TICKS)
    }

    /// Per-link tick deadline. Defaults to the host's 4h cap so a tick
    /// genuinely outlives the worker's ordinary bounded `timeout_ms`
    /// (WI#325 "infinite deadline participation": the lease is the chain,
    /// and each link is bounded by the host and any lower worker cap).
    pub fn tick_timeout_ms(&self) -> u64 {
        self.tick_timeout_ms.unwrap_or(Self::MAX_TICK_TIMEOUT_MS)
    }

    pub fn resolved_mission_id(&self, worker_name: &str) -> String {
        self.mission_id
            .clone()
            .unwrap_or_else(|| format!("cron-worker-{worker_name}"))
    }

    /// The mission's workspace directory: the explicit `project_dir`, or
    /// `missions/<mission_id>` so a default mission never shares the worker
    /// root with ordinary dispatches.
    pub fn resolved_project_dir(&self, worker_name: &str) -> String {
        self.project_dir
            .clone()
            .unwrap_or_else(|| format!("missions/{}", self.resolved_mission_id(worker_name)))
    }

    /// Validate the cron block against runaway-guard limits (WI#325).
    ///
    /// Fails loudly at config resolution: an invalid block means the worker
    /// is rejected from the resolved config entirely, so a typo can never
    /// silently leave the worker with no mission.
    pub fn validate(&self, worker_name: &str) -> Result<(), String> {
        if self.goal.trim().is_empty() {
            return Err("cron.goal must not be empty".to_string());
        }
        self.validate_schedule()?;
        self.validate_heartbeat()?;
        self.validate_reentries()?;
        self.validate_tick_timeout()?;
        self.validate_timezone()?;
        self.validate_retention()?;
        self.validate_project_dir(worker_name)
    }

    fn validate_retention(&self) -> Result<(), String> {
        if self.retain_last_ticks() < Self::MIN_RETAIN_LAST_TICKS {
            return Err(format!(
                "cron.retain_last_ticks {} is below the floor of {} (the anchor is always kept; \
                 the window must keep at least one tick row)",
                self.retain_last_ticks(),
                Self::MIN_RETAIN_LAST_TICKS
            ));
        }
        if self.retain_last_ticks() > Self::MAX_RETAIN_LAST_TICKS_LIMIT {
            return Err(format!(
                "cron.retain_last_ticks {} exceeds the {} cap",
                self.retain_last_ticks(),
                Self::MAX_RETAIN_LAST_TICKS_LIMIT
            ));
        }
        Ok(())
    }

    fn validate_timezone(&self) -> Result<(), String> {
        if let Some(tz) = &self.timezone
            && crate::timezone::parse_timezone(tz).is_none()
        {
            return Err(format!(
                "cron.timezone {tz:?} is not a parseable IANA timezone"
            ));
        }
        Ok(())
    }

    fn validate_schedule(&self) -> Result<(), String> {
        let timezone = self.timezone.as_deref();
        crate::agent::routine::next_cron_fire(&self.schedule, timezone)
            .map_err(|e| format!("cron.schedule does not parse: {e}"))?
            .ok_or_else(|| "cron.schedule yields no future fire times".to_string())?;
        if self.allow_subminute_schedule == Some(true) {
            return Ok(());
        }
        match crate::agent::routine::cron_min_fire_gap_secs(&self.schedule, timezone) {
            Ok(Some(gap)) if gap < Self::MIN_SCHEDULE_INTERVAL_SECS => Err(format!(
                "cron.schedule fires every {gap}s, below the {}s floor; set \
                 allow_subminute_schedule = true to override",
                Self::MIN_SCHEDULE_INTERVAL_SECS
            )),
            Ok(_) => Ok(()),
            Err(e) => Err(format!("cron.schedule does not parse: {e}")),
        }
    }

    fn validate_heartbeat(&self) -> Result<(), String> {
        if self.heartbeat_timeout()
            < std::time::Duration::from_secs(Self::MIN_HEARTBEAT_TIMEOUT_SECS)
        {
            return Err(format!(
                "cron.heartbeat_timeout_secs below the {}s floor",
                Self::MIN_HEARTBEAT_TIMEOUT_SECS
            ));
        }
        Ok(())
    }

    fn validate_reentries(&self) -> Result<(), String> {
        if self.max_reentries() > Self::MAX_REENTRIES_LIMIT {
            return Err(format!(
                "cron.max_reentries {} exceeds the {} cap",
                self.max_reentries(),
                Self::MAX_REENTRIES_LIMIT
            ));
        }
        Ok(())
    }

    fn validate_tick_timeout(&self) -> Result<(), String> {
        let tick_timeout_ms = self.tick_timeout_ms();
        if !(Self::MIN_TICK_TIMEOUT_MS..=Self::MAX_TICK_TIMEOUT_MS).contains(&tick_timeout_ms) {
            return Err(format!(
                "cron.tick_timeout_ms {tick_timeout_ms} outside {}..={} (ms); the upper bound \
                 matches the kimi worker bridge's default cap",
                Self::MIN_TICK_TIMEOUT_MS,
                Self::MAX_TICK_TIMEOUT_MS
            ));
        }
        Ok(())
    }

    /// Host-side `project_dir` validation (MR !1522 DE-5): the worker bridges
    /// reject bad paths with *wording that differs per bridge*, so the host
    /// refuses obviously-invalid values at load and the supervisor only has
    /// to classify filesystem/preflight failures at dispatch time.
    /// Validates the *effective* directory (explicit or `missions/<id>`
    /// default), which also confines a hostile `mission_id`.
    fn validate_project_dir(&self, worker_name: &str) -> Result<(), String> {
        let dir = self.resolved_project_dir(worker_name);
        let path = std::path::Path::new(&dir);
        let bad = |why: &str| {
            format!(
                "cron.project_dir {dir:?} is {why}; use a relative path under the worker's WORKSPACE_ROOT"
            )
        };
        if dir.trim().is_empty() {
            return Err(bad("empty"));
        }
        if dir.contains('\0') {
            return Err(bad("NUL-containing"));
        }
        if dir.starts_with('~') {
            return Err(bad("home-relative (`~`)"));
        }
        for component in path.components() {
            match component {
                std::path::Component::Normal(part) => {
                    // MR !1531 DS-5: the TS bridges' containment predicate
                    // rejects any residual starting with `..` (creating the
                    // directory first and then failing the tick), while pebble
                    // accepts it. Refuse `..x` names at load so every bridge
                    // agrees and nothing is created.
                    if part.as_encoded_bytes().starts_with(b"..") {
                        return Err(bad("a component starting with `..`"));
                    }
                    // MR !1531 DQ-3: components longer than NAME_MAX pass this
                    // validation but fail on the worker (a long
                    // `mission_id`-derived default can hit it); reject at load.
                    if part.len() > 255 {
                        return Err(bad("a component over 255 bytes"));
                    }
                }
                std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                    return Err(bad("absolute"));
                }
                std::path::Component::ParentDir => {
                    return Err(bad("a `..` traversal"));
                }
                std::path::Component::CurDir => {
                    return Err(bad("a `.` segment"));
                }
            }
        }
        // Path::components normalizes interior `.` and empty components;
        // the bridge's descriptor-relative creation rejects them literally.
        if dir.split('/').any(|part| part.is_empty() || part == ".") {
            return Err(bad("not a sequence of nonempty normal components"));
        }
        // MR !1531 DQ-3: `project_dir` components must be plain directories —
        // the bridges create them descriptor-relative with O_NOFOLLOW and
        // refuse any pre-existing symlink component (ENOTDIR), even when it
        // points inside the root.
        Ok(())
    }
}

/// Configuration for a named external worker endpoint.
#[derive(Debug, Clone)]
pub struct ExternalWorkerConfig {
    pub name: String,
    pub url: String,
    pub auth_token: Option<SecretString>,
    pub timeout_ms: u64,
    /// Multiple endpoints for load-balanced workers.
    /// When non-empty, `url`/`auth_token` are treated as fallback only.
    pub endpoints: Vec<WorkerEndpoint>,
    /// Load balancing strategy (defaults to RoundRobin).
    pub load_balance: LoadBalanceStrategy,
    /// Bounds applied when recent conversation context is requested.
    pub prompt_context: PromptContextLimits,
    /// Per-worker tool capability allowlist for the proxied tool path
    /// (WI#324). Deny-by-default: empty means every `tool_call` frame from
    /// this worker is rejected. Exact tool names only — there is no
    /// wildcard; `ExternalWorkerManager` drops `"*"`, autonomous-denylisted,
    /// proxy-denylisted, non-identifier, and non-phase-1 entries at load
    /// (the proxy surface is the positive WI#324 phase-1 capability set,
    /// MR !1533 DP-2), and strips the whole list when the worker is reached
    /// over cleartext `ws://` on a non-loopback host.
    ///
    /// This list governs **ordinary (task-mode) dispatches only**. Cron tick
    /// jobs on a `mode = "cron"` worker use `cron.tool_allowlist` instead
    /// (MR !1506 CQ-10): a schedule is standing unattended authority, so the
    /// mission's grant is an explicit separate decision. On a cron-mode worker
    /// this worker-level list is stripped at manager load (MR !1549 EF-1): one
    /// worker container is one authority domain shared by every concurrent and
    /// successive job in it, so a tick could otherwise borrow the interactive
    /// grant through another task's relay socket.
    pub tool_allowlist: Vec<String>,
    /// Operating mode (default task). `Cron` arms the host-side mission
    /// supervisor. The worker still accepts ordinary bounded dispatches, but
    /// the kimi/pebble workspace lock treats nested paths as overlapping, so
    /// an ordinary dispatch that shares the worker root contends with a
    /// running tick (whose `project_dir` defaults to `missions/<mission_id>`)
    /// — give ordinary tasks on a cron-mode worker their own non-overlapping
    /// `project_dir`, or expect busy rejections while a tick runs.
    pub mode: ExternalWorkerMode,
    /// Cron-mission settings; required when `mode` is `Cron`. A missing or
    /// invalid block rejects the worker at config resolution (logged at
    /// `error!`), so a bad cron config never boots silently mission-less.
    pub cron: Option<CronWorkerConfig>,
}

/// Sensible defaults for tests and partial construction (MR !1522 DE-4):
/// every literal should use `..Default::default()` so a sibling MR adding a
/// field does not break compilation outside Git's conflict markers.
impl Default for ExternalWorkerConfig {
    /// Sensible standalone defaults (used by tests and by struct-update
    /// literals so added fields do not break every construction site).
    /// `timeout_ms` matches the settings-layer external-worker default.
    fn default() -> Self {
        Self {
            name: String::new(),
            url: String::new(),
            auth_token: None,
            // Mirrors `default_external_worker_timeout` in settings.rs.
            timeout_ms: 3_600_000,
            endpoints: Vec::new(),
            load_balance: LoadBalanceStrategy::default(),
            prompt_context: PromptContextLimits::default(),
            tool_allowlist: Vec::new(),
            mode: ExternalWorkerMode::default(),
            cron: None,
        }
    }
}

impl ExternalWorkerConfig {
    pub fn resolve_from_settings(settings: &crate::settings::Settings) -> Vec<Self> {
        // WI#325 (MR !1522 DE-7): two cron workers sharing a mission id would
        // share the persisted `cron-mission:<id>:*` settings keys and corrupt
        // each other's durable state, so the duplicate is rejected loudly.
        let mut seen_mission_ids = std::collections::HashSet::new();
        settings
            .sandbox
            .external_workers
            .iter()
            .filter_map(|ew| {
                let prompt_context = ew.prompt_context.bounded();
                if prompt_context != ew.prompt_context {
                    tracing::warn!(
                        worker = %ew.name,
                        requested_max_messages = ew.prompt_context.max_messages,
                        requested_max_chars_per_message = ew.prompt_context.max_chars_per_message,
                        requested_max_total_chars = ew.prompt_context.max_total_chars,
                        "External worker prompt-context limits exceeded hard ceilings; clamped"
                    );
                }
                let config = Self {
                    name: ew.name.clone(),
                    url: ew.url.clone(),
                    auth_token: ew.auth_token.clone(),
                    timeout_ms: ew.timeout_ms,
                    endpoints: ew.endpoints.clone(),
                    load_balance: ew.load_balance.clone(),
                    prompt_context,
                    tool_allowlist: ew.tool_allowlist.clone(),
                    mode: ew.mode,
                    cron: ew.cron.clone(),
                };
                // WI#325: reject invalid cron configs loudly at load instead
                // of booting a worker whose mission supervisor can never run.
                if config.is_cron() && !config.cron_block_valid(&mut seen_mission_ids) {
                    return None;
                }
                // MR !1506 CQ-10 / MR !1549 EF-1: tick jobs use
                // `cron.tool_allowlist`, never the worker-level list, and the
                // manager strips a cron worker's worker-level list at load
                // (one container = one authority domain). Say so when a
                // configured list would otherwise silently vanish.
                if config.is_cron() && !config.tool_allowlist.is_empty() {
                    tracing::warn!(
                        worker = %config.name,
                        "External worker is mode=cron with a worker-level tool_allowlist; \
                         the manager strips it at load — grant tick authority via \
                         cron.tool_allowlist (ordinary dispatches get no proxied tools)"
                    );
                }
                // A [cron] block on a task-mode worker is ignored — say so
                // instead of letting the operator believe a mission is armed.
                if !config.is_cron() && config.cron.is_some() {
                    tracing::warn!(
                        worker = %config.name,
                        "External worker has a [cron] block but mode is not \"cron\"; \
                         the cron block is ignored (set mode = \"cron\" to arm the mission)"
                    );
                }
                Some(config)
            })
            .collect()
    }

    /// Whether the cron block of a cron-mode worker passes validation and its
    /// mission id is unique. Logs the precise reason at `error!` when not.
    fn cron_block_valid(&self, seen_mission_ids: &mut std::collections::HashSet<String>) -> bool {
        let Some(cron) = &self.cron else {
            tracing::error!(
                worker = %self.name,
                "External worker is mode=cron but has no [cron] block; worker rejected"
            );
            return false;
        };
        if let Err(reason) = cron.validate(&self.name) {
            tracing::error!(
                worker = %self.name,
                "Invalid cron config for cron-mode worker; worker rejected: {reason}"
            );
            return false;
        }
        let mission_id = cron.resolved_mission_id(&self.name);
        if !seen_mission_ids.insert(mission_id.clone()) {
            tracing::error!(
                worker = %self.name,
                %mission_id,
                "Duplicate cron mission_id (persisted mission state is keyed by it); \
                 worker rejected — set a unique cron.mission_id"
            );
            return false;
        }
        true
    }

    /// Returns the canonical endpoint list for this worker.
    ///
    /// When `endpoints` is non-empty those are used directly; otherwise a
    /// single `WorkerEndpoint` is synthesized from the legacy `url` and
    /// `auth_token` fields.
    pub fn endpoints(&self) -> Vec<WorkerEndpoint> {
        if !self.endpoints.is_empty() {
            self.endpoints.clone()
        } else {
            vec![WorkerEndpoint {
                url: self.url.clone(),
                auth_token: self.auth_token.clone(),
                weight: None,
            }]
        }
    }

    /// Whether this worker runs mission-style under the cron supervisor (WI#325).
    pub fn is_cron(&self) -> bool {
        self.mode == ExternalWorkerMode::Cron
    }
}

#[cfg(test)]
impl ExternalWorkerConfig {
    /// Shared constructor for test fixtures. Tests that need a config should
    /// start from `ExternalWorkerConfig::test_config(name, url)` and override
    /// fields with struct-update syntax.
    ///
    /// Built by round-tripping a minimal `[[sandbox.external_workers]]` TOML
    /// block through the real settings resolver rather than a struct literal
    /// (MR !1525 DI-2: a full literal textually auto-merges against sibling
    /// branches that add `ExternalWorkerConfig` fields and then fails E0063
    /// in test lanes only). New fields with the project-mandated
    /// `#[serde(default)]` on the Settings side flow through with no edit
    /// here; a missing serde default fails loudly in this one constructor
    /// instead of breaking every test lane's compile.
    ///
    /// This only covers field ADDITIONS. Sibling branches that change
    /// signatures (`reserve_task`) or handle fields (`ExternalJobHandle`)
    /// still require whoever lands second to fix the call sites.
    pub fn test_config(name: &str, url: &str) -> Self {
        // `{name:?}`/`{url:?}` render valid TOML basic strings for the plain
        // identifier/URL values tests pass here.
        let toml_src = format!(
            "[[sandbox.external_workers]]\nname = {name:?}\nurl = {url:?}\ntimeout_ms = 300000\n"
        );
        let settings: crate::settings::Settings =
            toml::from_str(&toml_src).expect("test worker config must parse"); // safety: test
        Self::resolve_from_settings(&settings)
            .into_iter()
            .next()
            .expect("exactly one test worker") // safety: test
    }
}

/// ACP (Agent Client Protocol) mode configuration.
#[derive(Debug, Clone)]
pub struct AcpModeConfig {
    pub enabled: bool,
    pub memory_limit_mb: u64,
    pub timeout_secs: u64,
}

impl Default for AcpModeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            memory_limit_mb: 4096,
            timeout_secs: 1800,
        }
    }
}

impl AcpModeConfig {
    pub fn from_env() -> Self {
        match Self::resolve_env_only() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to resolve AcpModeConfig: {e}, using defaults");
                Self::default()
            }
        }
    }

    pub(crate) fn resolve(settings: &crate::settings::Settings) -> Result<Self, ConfigError> {
        let defaults = Self::default();
        Ok(Self {
            enabled: parse_bool_env("ACP_ENABLED", settings.sandbox.acp_enabled)?,
            memory_limit_mb: parse_optional_env("ACP_MEMORY_LIMIT_MB", defaults.memory_limit_mb)?,
            timeout_secs: parse_optional_env("ACP_TIMEOUT_SECS", defaults.timeout_secs)?,
        })
    }

    fn resolve_env_only() -> Result<Self, ConfigError> {
        let defaults = Self::default();
        Ok(Self {
            enabled: parse_bool_env("ACP_ENABLED", defaults.enabled)?,
            memory_limit_mb: parse_optional_env("ACP_MEMORY_LIMIT_MB", defaults.memory_limit_mb)?,
            timeout_secs: parse_optional_env("ACP_TIMEOUT_SECS", defaults.timeout_secs)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::config::sandbox::*;
    use secrecy::ExposeSecret;

    // ── SandboxModeConfig defaults ──────────────────────────────────

    #[test]
    fn sandbox_mode_config_default_values() {
        let cfg = SandboxModeConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.policy, "readonly");
        assert_eq!(cfg.timeout_secs, 120);
        assert_eq!(cfg.memory_limit_mb, 2048);
        assert_eq!(cfg.cpu_shares, 1024);
        assert_eq!(cfg.image, "lunarwing-worker:latest");
        assert!(cfg.auto_pull_image);
        assert!(cfg.extra_allowed_domains.is_empty());
    }

    #[test]
    fn sandbox_mode_config_custom_values() {
        let cfg = SandboxModeConfig {
            enabled: false,
            policy: "full_access".to_string(),
            runtime: "podman".to_string(),
            network: "none".to_string(),
            enforce: true,
            timeout_secs: 600,
            memory_limit_mb: 4096,
            cpu_shares: 512,
            image: "custom-worker:v2".to_string(),
            auto_pull_image: false,
            extra_allowed_domains: vec!["example.com".to_string()],
            reaper_interval_secs: 300,
            orphan_threshold_secs: 600,
            allow_full_access: false,
            tenant_logs: crate::sandbox::TenantLogsMode::Info,
        };
        assert!(!cfg.enabled);
        assert_eq!(cfg.policy, "full_access");
        assert_eq!(cfg.runtime, "podman");
        assert_eq!(cfg.network, "none");
        assert!(cfg.enforce);
        assert_eq!(cfg.timeout_secs, 600);
        assert_eq!(cfg.memory_limit_mb, 4096);
        assert_eq!(cfg.cpu_shares, 512);
        assert_eq!(cfg.image, "custom-worker:v2");
        assert!(!cfg.auto_pull_image);
        assert_eq!(cfg.extra_allowed_domains, vec!["example.com"]);
    }

    #[test]
    fn sandbox_mode_to_sandbox_config_propagates_fields() {
        let mode = SandboxModeConfig {
            enabled: true,
            policy: "workspace_write".to_string(),
            runtime: "docker".to_string(),
            network: "none".to_string(),
            timeout_secs: 300,
            memory_limit_mb: 1024,
            cpu_shares: 2048,
            image: "test:latest".to_string(),
            auto_pull_image: false,
            extra_allowed_domains: vec!["custom.example.com".to_string()],
            reaper_interval_secs: 300,
            orphan_threshold_secs: 600,
            allow_full_access: false,
            enforce: false,
            tenant_logs: crate::sandbox::TenantLogsMode::Off,
        };
        let sc = mode.to_sandbox_config();
        assert!(sc.enabled);
        assert_eq!(sc.policy, crate::sandbox::SandboxPolicy::WorkspaceWrite);
        assert_eq!(sc.timeout, std::time::Duration::from_secs(300));
        assert_eq!(sc.memory_limit_mb, 1024);
        assert_eq!(sc.cpu_shares, 2048);
        assert_eq!(sc.image, "test:latest");
        assert!(!sc.auto_pull_image);
        assert_eq!(sc.runtime, crate::sandbox::SandboxRuntime::Docker);
        assert_eq!(sc.network, crate::sandbox::SandboxNetworkMode::None);
        assert_eq!(sc.tenant_logs, crate::sandbox::TenantLogsMode::Off);
        // extra domain should be in the allowlist
        assert!(
            sc.network_allowlist
                .contains(&"custom.example.com".to_string()),
            "expected custom domain in allowlist"
        );
    }

    #[test]
    fn sandbox_mode_invalid_runtime_network_fall_back() {
        let mode = SandboxModeConfig {
            runtime: "containerd".to_string(),
            network: "wide-open".to_string(),
            ..SandboxModeConfig::default()
        };
        let sc = mode.to_sandbox_config();
        assert_eq!(sc.runtime, crate::sandbox::SandboxRuntime::Auto);
        assert_eq!(sc.network, crate::sandbox::SandboxNetworkMode::None);
    }

    #[test]
    fn sandbox_network_default_and_enforced_mode_survive_resolution() {
        let default_cfg = SandboxModeConfig::default().to_sandbox_config();
        assert_eq!(
            default_cfg.network,
            crate::sandbox::SandboxNetworkMode::None
        );

        let enforced = SandboxModeConfig {
            network: "proxy-enforced".to_string(),
            ..SandboxModeConfig::default()
        };
        assert_eq!(
            enforced.to_sandbox_config().network,
            crate::sandbox::SandboxNetworkMode::ProxyEnforced
        );

        let legacy = SandboxModeConfig {
            network: "proxy".to_string(),
            ..SandboxModeConfig::default()
        };
        assert_eq!(
            legacy.to_sandbox_config().network,
            crate::sandbox::SandboxNetworkMode::Proxy
        );
    }

    #[test]
    fn sandbox_runtime_network_env_overrides_settings() {
        let _guard = crate::config::helpers::lock_env();
        let mut settings = crate::settings::Settings::default();
        settings.sandbox.runtime = "docker".to_string();
        settings.sandbox.network = "none".to_string();
        settings.sandbox.enforce = true;

        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe { std::env::set_var("SANDBOX_RUNTIME", "podman") };
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve");
        unsafe { std::env::remove_var("SANDBOX_RUNTIME") };

        assert_eq!(cfg.runtime, "podman");
        // No env override for network: settings win.
        assert_eq!(cfg.network, "none");
        assert!(cfg.enforce);
    }

    /// WI#287 review S7: SANDBOX_TENANT_LOGS selects the mode (default info)
    /// and a typo is a config error rather than a silent fallback.
    #[test]
    fn sandbox_tenant_logs_env_selects_mode_and_rejects_typos() {
        use crate::sandbox::TenantLogsMode;

        let _guard = crate::config::helpers::lock_env();
        let settings = crate::settings::Settings::default();

        // SAFETY: Under ENV_MUTEX (restored on drop), no concurrent env access.
        unsafe { std::env::remove_var("SANDBOX_TENANT_LOGS") };
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve default");
        assert_eq!(cfg.tenant_logs, TenantLogsMode::Info);

        unsafe { std::env::set_var("SANDBOX_TENANT_LOGS", "off") };
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve off");
        assert_eq!(cfg.tenant_logs, TenantLogsMode::Off);
        assert_eq!(cfg.to_sandbox_config().tenant_logs, TenantLogsMode::Off);

        unsafe { std::env::set_var("SANDBOX_TENANT_LOGS", "debug") };
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve debug");
        assert_eq!(cfg.tenant_logs, TenantLogsMode::Debug);

        unsafe { std::env::set_var("SANDBOX_TENANT_LOGS", "offf") };
        match SandboxModeConfig::resolve(&settings) {
            Err(ConfigError::InvalidValue { key, .. }) => assert_eq!(key, "SANDBOX_TENANT_LOGS"),
            other => panic!("expected InvalidValue, got {other:?}"),
        }
        unsafe { std::env::remove_var("SANDBOX_TENANT_LOGS") };
    }

    #[test]
    fn sandbox_enforce_env_overrides_default() {
        let _guard = crate::config::helpers::lock_env();
        let settings = crate::settings::Settings::default();
        assert!(!settings.sandbox.enforce);

        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe { std::env::set_var("SANDBOX_ENFORCE", "true") };
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve");
        unsafe { std::env::remove_var("SANDBOX_ENFORCE") };

        assert!(cfg.enforce);
    }

    #[test]
    fn sandbox_mode_to_sandbox_config_invalid_policy_falls_back_to_readonly() {
        let mode = SandboxModeConfig {
            policy: "garbage_value".to_string(),
            ..SandboxModeConfig::default()
        };
        let sc = mode.to_sandbox_config();
        assert_eq!(sc.policy, crate::sandbox::SandboxPolicy::ReadOnly);
    }

    #[test]
    fn sandbox_mode_to_sandbox_config_includes_default_allowlist() {
        let mode = SandboxModeConfig::default();
        let sc = mode.to_sandbox_config();
        // The default allowlist from sandbox module should be non-empty
        assert!(
            !sc.network_allowlist.is_empty(),
            "default allowlist should not be empty"
        );
    }

    #[test]
    fn test_full_access_downgraded_without_allow() {
        let config = SandboxModeConfig {
            policy: "full_access".to_string(),
            allow_full_access: false,
            ..Default::default()
        };
        let sandbox = config.to_sandbox_config();
        // Should have been downgraded to WorkspaceWrite
        assert_eq!(
            sandbox.policy,
            crate::sandbox::SandboxPolicy::WorkspaceWrite
        );
        assert!(!sandbox.allow_full_access);
    }

    #[test]
    fn test_full_access_allowed_with_explicit_opt_in() {
        let config = SandboxModeConfig {
            policy: "full_access".to_string(),
            allow_full_access: true,
            ..Default::default()
        };
        let sandbox = config.to_sandbox_config();
        assert_eq!(sandbox.policy, crate::sandbox::SandboxPolicy::FullAccess);
        assert!(sandbox.allow_full_access);
    }

    #[test]
    fn test_non_full_access_policy_unaffected() {
        let config = SandboxModeConfig {
            policy: "workspace_write".to_string(),
            allow_full_access: false,
            ..Default::default()
        };
        let sandbox = config.to_sandbox_config();
        assert_eq!(
            sandbox.policy,
            crate::sandbox::SandboxPolicy::WorkspaceWrite
        );
    }

    // ── Settings fallback tests ──────────────────────────────────────

    #[test]
    fn sandbox_resolve_falls_back_to_settings() {
        let _guard = crate::config::helpers::lock_env();
        let mut settings = crate::settings::Settings::default();
        settings.sandbox.cpu_shares = 99;
        settings.sandbox.auto_pull_image = false;
        settings.sandbox.enabled = false;

        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve");
        assert!(!cfg.enabled);
        assert_eq!(cfg.cpu_shares, 99);
        assert!(!cfg.auto_pull_image);
    }

    #[test]
    fn sandbox_env_overrides_settings() {
        let _guard = crate::config::helpers::lock_env();
        let mut settings = crate::settings::Settings::default();
        settings.sandbox.timeout_secs = 999;

        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe { std::env::set_var("SANDBOX_TIMEOUT_SECS", "5") };
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve");
        unsafe { std::env::remove_var("SANDBOX_TIMEOUT_SECS") };

        assert_eq!(cfg.timeout_secs, 5);
    }

    #[test]
    fn test_readonly_policy_unaffected() {
        let config = SandboxModeConfig {
            policy: "readonly".to_string(),
            allow_full_access: false,
            ..Default::default()
        };
        let sandbox = config.to_sandbox_config();
        assert_eq!(sandbox.policy, crate::sandbox::SandboxPolicy::ReadOnly);
    }

    // ── External worker config (mt-admin config.toml contract) ───────────

    /// The exact `config.toml` block `lunarwing-mt-admin.sh` writes for a
    /// tenant's nanocode worker must deserialize into an `ExternalWorkerConfig`
    /// the daemon can route to. Guards the script↔daemon contract: if a field
    /// name (`external_workers`, `auth_token`, `timeout_ms`) or the nesting
    /// (`[[sandbox.external_workers]]`) drifts, the worker would silently fail
    /// to load and `create_job(mode: "nanocode")` would have nothing to route
    /// to — the exact bug in docs/bugs/MISSING-CONFIG-FOR-NANOCODE.md.
    #[test]
    fn external_worker_config_matches_mt_admin_output() {
        // Mirrors ensure_external_worker_config() — including the minimal
        // fresh-file form add-tenant writes (only the external-workers block;
        // every other Settings field falls back to its default).
        let generated = r#"
# LunarWing tenant configuration (auto-generated by lunarwing-mt-admin.sh).

# External worker: nanocode — create_job(mode: "nanocode")
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:10007/ws/agent"
auth_token = "deadbeefcafe1234"
timeout_ms = 3600000
"#;

        let settings: crate::settings::Settings =
            toml::from_str(generated).expect("mt-admin config.toml must parse");

        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        assert_eq!(workers.len(), 1, "exactly one external worker expected");
        assert_eq!(workers[0].name, "nanocode");
        assert_eq!(workers[0].url, "ws://127.0.0.1:10007/ws/agent");
        assert_eq!(
            workers[0].auth_token.as_ref().map(|s| s.expose_secret()),
            Some("deadbeefcafe1234")
        );
        assert_eq!(workers[0].timeout_ms, 3_600_000);
        assert_eq!(workers[0].prompt_context, PromptContextLimits::default());
    }

    #[test]
    fn external_worker_prompt_context_limits_are_clamped() {
        let generated = r#"
[[sandbox.external_workers]]
name = "opencode"
url = "ws://127.0.0.1:10007/ws/agent"

[sandbox.external_workers.prompt_context]
max_messages = 500
max_chars_per_message = 100000
max_total_chars = 500000
"#;
        let settings: crate::settings::Settings = toml::from_str(generated).expect("config parses");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);

        assert_eq!(
            workers[0].prompt_context,
            lunarwing_common::HARD_PROMPT_CONTEXT_LIMITS
        );
    }

    /// Multiple `[[sandbox.external_workers]]` blocks in one file (e.g. nanocode
    /// plus pebble) must all resolve. Mirrors a tenant config that has had a
    /// second worker appended, guarding the array-of-tables append path the
    /// script relies on.
    #[test]
    fn external_worker_config_supports_multiple_workers() {
        let generated = r#"
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:10007/ws/agent"
auth_token = "tok-nano"
timeout_ms = 3600000

[[sandbox.external_workers]]
name = "pebble"
url = "ws://127.0.0.1:10008/ws/agent"
auth_token = "tok-pebble"
timeout_ms = 3600000
"#;

        let settings: crate::settings::Settings =
            toml::from_str(generated).expect("multi-worker config.toml must parse");

        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        let names: Vec<&str> = workers.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(workers.len(), 2);
        assert!(names.contains(&"nanocode"));
        assert!(names.contains(&"pebble"));
    }

    #[test]
    fn external_worker_config_multi_endpoint() {
        let toml_str = r#"
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:10007/ws/agent"
auth_token = "tok-legacy"
timeout_ms = 3600000

[[sandbox.external_workers.endpoints]]
url = "ws://10.0.0.1:9090/ws/agent"
auth_token = "tok-1"

[[sandbox.external_workers.endpoints]]
url = "ws://10.0.0.2:9090/ws/agent"
auth_token = "tok-2"
"#;

        let settings: crate::settings::Settings =
            toml::from_str(toml_str).expect("multi-endpoint config must parse");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].endpoints.len(), 2);
        assert_eq!(workers[0].endpoints[0].url, "ws://10.0.0.1:9090/ws/agent");
        assert_eq!(workers[0].endpoints[1].url, "ws://10.0.0.2:9090/ws/agent");
        assert!(matches!(
            workers[0].load_balance,
            LoadBalanceStrategy::RoundRobin
        ));
    }

    #[test]
    fn external_worker_config_legacy_fallback() {
        let toml_str = r#"
[[sandbox.external_workers]]
name = "pebble"
url = "ws://127.0.0.1:8443/ws/agent"
auth_token = "tok-legacy"
timeout_ms = 3600000
"#;

        let settings: crate::settings::Settings =
            toml::from_str(toml_str).expect("legacy config must parse");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        assert_eq!(workers.len(), 1);
        assert!(workers[0].endpoints.is_empty());

        let endpoints = workers[0].endpoints();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].url, "ws://127.0.0.1:8443/ws/agent");
        assert_eq!(
            endpoints[0].auth_token.as_ref().map(|s| s.expose_secret()),
            Some("tok-legacy")
        );
    }

    // ── WI#325 cron worker mode ──────────────────────────────────────

    #[test]
    fn external_worker_mode_defaults_to_task() {
        let toml_str = r#"
[[sandbox.external_workers]]
name = "pebble"
url = "ws://127.0.0.1:8443/ws/agent"
"#;
        let settings: crate::settings::Settings =
            toml::from_str(toml_str).expect("config must parse");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        assert_eq!(workers[0].mode, ExternalWorkerMode::Task);
        assert!(!workers[0].is_cron());
        assert!(workers[0].cron.is_none());
    }

    #[test]
    fn external_worker_cron_mode_resolves() {
        let toml_str = r#"
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:10007/ws/agent"
timeout_ms = 3600000
mode = "cron"

[sandbox.external_workers.cron]
schedule = "0 15 10 * * *"
goal = "Produce the daily PR digest"
mission_id = "pr-digest"
heartbeat_timeout_secs = 300
tick_timeout_ms = 1800000
max_reentries = 5
"#;
        let settings: crate::settings::Settings =
            toml::from_str(toml_str).expect("cron config must parse");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        assert_eq!(workers.len(), 1);
        let worker = &workers[0];
        assert_eq!(worker.mode, ExternalWorkerMode::Cron);
        assert!(worker.is_cron());

        let cron = worker.cron.as_ref().expect("cron config present");
        assert_eq!(cron.schedule, "0 15 10 * * *");
        assert_eq!(cron.goal, "Produce the daily PR digest");
        assert_eq!(cron.resolved_mission_id("kimi"), "pr-digest");
        assert_eq!(
            cron.heartbeat_timeout(),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(cron.tick_timeout_ms(), 1_800_000);
        assert_eq!(cron.max_reentries(), 5);
    }

    #[test]
    fn external_worker_cron_defaults_are_applied() {
        let toml_str = r#"
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:10007/ws/agent"
timeout_ms = 3600000
mode = "cron"

[sandbox.external_workers.cron]
schedule = "*/30 * * * *"
goal = "Watch the herd"
"#;
        let settings: crate::settings::Settings =
            toml::from_str(toml_str).expect("cron config must parse");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        let cron = workers[0].cron.as_ref().expect("cron config present");

        assert_eq!(cron.resolved_mission_id("kimi"), "cron-worker-kimi");
        assert_eq!(
            cron.heartbeat_timeout(),
            std::time::Duration::from_secs(CronWorkerConfig::DEFAULT_HEARTBEAT_TIMEOUT_SECS)
        );
        // Default per-link deadline is the host's 4h tick cap, not the
        // worker's ordinary timeout: a tick link must outlive the standard
        // bounded deadline by default (WI#325, MR !1522 CU-3).
        assert_eq!(
            cron.tick_timeout_ms(),
            CronWorkerConfig::MAX_TICK_TIMEOUT_MS
        );
        assert_eq!(
            cron.max_reentries(),
            CronWorkerConfig::DEFAULT_MAX_REENTRIES
        );
    }

    // ── WI#325 runaway guards: invalid cron configs reject the worker ──

    fn cron_worker_toml(cron_block: &str) -> String {
        format!(
            r#"
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:10007/ws/agent"
timeout_ms = 3600000
mode = "cron"

[sandbox.external_workers.cron]
{cron_block}
"#
        )
    }

    fn resolve_cron_worker(cron_block: &str) -> Option<ExternalWorkerConfig> {
        let settings: crate::settings::Settings =
            toml::from_str(&cron_worker_toml(cron_block)).expect("config must parse");
        ExternalWorkerConfig::resolve_from_settings(&settings)
            .into_iter()
            .next()
    }

    #[test]
    fn cron_mode_without_cron_block_is_rejected() {
        let settings: crate::settings::Settings = toml::from_str(
            r#"
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:10007/ws/agent"
mode = "cron"
"#,
        )
        .expect("config must parse");
        assert!(ExternalWorkerConfig::resolve_from_settings(&settings).is_empty());
    }

    #[test]
    fn invalid_schedule_is_rejected() {
        assert!(resolve_cron_worker("schedule = \"not a schedule\"\ngoal = \"g\"").is_none());
    }

    #[test]
    fn empty_goal_is_rejected() {
        assert!(resolve_cron_worker("schedule = \"0 15 10 * * *\"\ngoal = \"  \"").is_none());
    }

    #[test]
    fn subminute_schedule_is_rejected_without_override() {
        assert!(
            resolve_cron_worker("schedule = \"* * * * * *\"\ngoal = \"g\"").is_none(),
            "per-second schedule must not pass the floor"
        );
        assert!(
            resolve_cron_worker("schedule = \"*/30 * * * * *\"\ngoal = \"g\"").is_none(),
            "30-second schedule must not pass the floor"
        );
        // The floor is 60s, so a per-minute schedule is allowed.
        assert!(resolve_cron_worker("schedule = \"* * * * *\"\ngoal = \"g\"").is_some());
    }

    #[test]
    fn subminute_schedule_passes_with_explicit_override() {
        // MR !1522 DE-6: must use a genuinely sub-60s schedule — a per-minute
        // expression passes the floor without the override and cannot regress.
        let worker = resolve_cron_worker(
            "schedule = \"* * * * * *\"\ngoal = \"g\"\nallow_subminute_schedule = true",
        )
        .expect("override must allow the per-second schedule");
        assert!(worker.is_cron());
    }

    #[test]
    fn heartbeat_below_floor_is_rejected() {
        assert!(
            resolve_cron_worker(
                "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nheartbeat_timeout_secs = 0"
            )
            .is_none()
        );
        assert!(
            resolve_cron_worker(
                "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nheartbeat_timeout_secs = 4"
            )
            .is_none()
        );
    }

    #[test]
    fn max_reentries_above_cap_is_rejected() {
        assert!(
            resolve_cron_worker(
                "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nmax_reentries = 4294967295"
            )
            .is_none()
        );
    }

    #[test]
    fn tick_timeout_outside_bounds_is_rejected() {
        // Zero is rejected by the worker bridge (timeout_ms must be >= 1).
        assert!(
            resolve_cron_worker("schedule = \"0 15 10 * * *\"\ngoal = \"g\"\ntick_timeout_ms = 0")
                .is_none()
        );
        // Above the host's 4h tick cap: rejected at load (the host enforces
        // the same bound per link; only Kimi's bridge separately caps
        // timeout_ms, configurable via KIMI_MAX_TIMEOUT_MS).
        assert!(
            resolve_cron_worker(
                "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\ntick_timeout_ms = 604800000"
            )
            .is_none()
        );
        // The cap itself is accepted.
        assert!(
            resolve_cron_worker(
                "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\ntick_timeout_ms = 14400000"
            )
            .is_some()
        );
        // The worker's ordinary timeout no longer feeds the tick deadline:
        // the default tick deadline is the host cap regardless.
        let settings: crate::settings::Settings = toml::from_str(
            r#"
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:10007/ws/agent"
timeout_ms = 86400000
mode = "cron"

[sandbox.external_workers.cron]
schedule = "0 15 10 * * *"
goal = "g"
"#,
        )
        .expect("config must parse");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        let cron = workers[0].cron.as_ref().expect("cron block");
        assert_eq!(
            cron.tick_timeout_ms(),
            CronWorkerConfig::MAX_TICK_TIMEOUT_MS
        );
    }

    #[test]
    fn mission_project_dir_resolves() {
        let worker = resolve_cron_worker(
            "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nproject_dir = \"missions/pr-digest\"",
        )
        .expect("valid cron config with project_dir");
        let cron = worker.cron.as_ref().expect("cron block");
        assert_eq!(cron.project_dir.as_deref(), Some("missions/pr-digest"));
    }

    /// MR !1522 DE-2: the mission workspace defaults to `missions/<id>` so a
    /// default mission never shares the worker root with ordinary dispatches.
    #[test]
    fn mission_project_dir_defaults_to_missions_subdir() {
        let worker = resolve_cron_worker("schedule = \"0 15 10 * * *\"\ngoal = \"g\"")
            .expect("valid cron config");
        let cron = worker.cron.as_ref().expect("cron block");
        assert_eq!(cron.project_dir, None);
        assert_eq!(
            cron.resolved_project_dir("kimi"),
            "missions/cron-worker-kimi"
        );
    }

    /// MR !1522 DE-5: obviously-invalid project dirs are refused at load on
    /// the host instead of being classified from per-bridge error wording.
    #[test]
    fn mission_project_dir_rejects_absolute_and_traversal() {
        for dir in [
            "/etc",
            "../escape",
            "missions/../../escape",
            "~/rooted",
            "./dot",
            "missions/./digest",
            "missions//digest",
            "missions/digest/",
            "white space/..",
        ] {
            assert!(
                resolve_cron_worker(&format!(
                    "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nproject_dir = \"{dir}\""
                ))
                .is_none(),
                "project_dir {dir:?} must be rejected at load"
            );
        }
        // A hostile mission_id cannot smuggle a traversal into the default dir.
        assert!(
            resolve_cron_worker(
                "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nmission_id = \"../x\""
            )
            .is_none()
        );
        // MR !1531 DS-5: `..`-prefixed normal components are refused at load
        // so all bridges agree (previously the TS bridges created the
        // directory and then rejected it as an escape; pebble accepted it).
        for dir in ["..missions/pr-digest", "missions/..digest"] {
            assert!(
                resolve_cron_worker(&format!(
                    "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nproject_dir = \"{dir}\""
                ))
                .is_none(),
                "project_dir {dir:?} must be rejected at load"
            );
        }
        // MR !1531 DQ-3: a component over NAME_MAX (255 bytes) passes
        // validation but fails on the worker; refuse it at load. Exactly 255
        // bytes stays valid.
        let over = "m".repeat(256);
        assert!(
            resolve_cron_worker(&format!(
                "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nproject_dir = \"missions/{over}\""
            ))
            .is_none(),
            "project_dir with a >255-byte component must be rejected at load"
        );
        let at_max = "m".repeat(255);
        assert!(
            resolve_cron_worker(&format!(
                "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nproject_dir = \"missions/{at_max}\""
            ))
            .is_some(),
            "a 255-byte component must remain valid"
        );
    }

    /// MR !1522 DE-7: two cron workers sharing a mission id would share the
    /// persisted `cron-mission:<id>:*` settings keys; the duplicate loses.
    #[test]
    fn duplicate_mission_id_is_rejected() {
        let settings: crate::settings::Settings = toml::from_str(
            r#"
[[sandbox.external_workers]]
name = "kimi-a"
url = "ws://127.0.0.1:10007/ws/agent"
mode = "cron"
[sandbox.external_workers.cron]
schedule = "0 15 10 * * *"
goal = "g"
mission_id = "shared"

[[sandbox.external_workers]]
name = "kimi-b"
url = "ws://127.0.0.1:10008/ws/agent"
mode = "cron"
[sandbox.external_workers.cron]
schedule = "0 16 10 * * *"
goal = "g"
mission_id = "shared"
"#,
        )
        .expect("config must parse");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].name, "kimi-a");
    }

    /// MR !1522 DE-8: schedules honor a configured IANA timezone, resolved
    /// with the same normalization as routines; a bad zone rejects at load.
    #[test]
    fn cron_timezone_is_validated() {
        assert!(
            resolve_cron_worker(
                "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\ntimezone = \"America/New_York\""
            )
            .is_some()
        );
        assert!(
            resolve_cron_worker(
                "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\ntimezone = \"Not/AZone\""
            )
            .is_none()
        );
    }

    /// MR !1522 DE-10: unknown keys in the cron block are a load error, not a
    /// silent fallback to defaults.
    #[test]
    fn cron_block_rejects_unknown_keys() {
        let settings: Result<crate::settings::Settings, _> = toml::from_str(
            r#"
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:10007/ws/agent"
mode = "cron"

[sandbox.external_workers.cron]
schedule = "0 15 10 * * *"
goal = "g"
hearbeat_timeout_secs = 300
"#,
        );
        assert!(
            settings.is_err(),
            "a misspelled cron key must fail TOML deserialization"
        );
    }

    /// WI#370 LW-9: the retention window is load-validated on both ends —
    /// 0 would keep nothing but the anchor (the anchor is always kept
    /// anyway), and beyond the cap a typo would silently re-enable unbounded
    /// growth. Both reject the worker at load.
    #[test]
    fn cron_retention_window_is_floored_and_capped() {
        assert!(
            resolve_cron_worker("schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nretain_last_ticks = 0")
                .is_none(),
            "retain_last_ticks = 0 must reject the worker at load"
        );
        assert!(resolve_cron_worker(
            "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nretain_last_ticks = 1"
        )
        .is_some());
        assert!(resolve_cron_worker(
            "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nretain_last_ticks = 50"
        )
        .is_some());
        assert!(resolve_cron_worker(&format!(
            "schedule = \"0 15 10 * * *\"\ngoal = \"g\"\nretain_last_ticks = {}",
            CronWorkerConfig::MAX_RETAIN_LAST_TICKS_LIMIT + 1
        ))
        .is_none());
    }
}
