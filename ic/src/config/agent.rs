use std::time::Duration;

use crate::config::helpers::{parse_bool_env, parse_option_env, parse_optional_env};
use crate::error::ConfigError;
use crate::settings::Settings;

/// Agent behavior configuration.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub max_parallel_jobs: usize,
    pub job_timeout: Duration,
    pub stuck_threshold: Duration,
    pub repair_check_interval: Duration,
    pub max_repair_attempts: u32,
    /// Whether to use planning before tool execution.
    pub use_planning: bool,
    /// Session idle timeout. Sessions inactive longer than this are pruned.
    pub session_idle_timeout: Duration,
    /// Allow chat to use filesystem/shell tools directly (bypass sandbox).
    pub allow_local_tools: bool,
    /// Maximum daily LLM spend in cents (e.g. 10000 = $100). None = unlimited.
    pub max_cost_per_day_cents: Option<u64>,
    /// Maximum LLM/tool actions per hour. None = unlimited.
    pub max_actions_per_hour: Option<u64>,
    /// Maximum tool-call iterations per agentic loop invocation. Default 50.
    pub max_tool_iterations: usize,
    /// When true, skip tool approval checks entirely. For benchmarks/CI.
    pub auto_approve_tools: bool,
    /// Default timezone for new sessions (IANA name, e.g. "America/New_York").
    pub default_timezone: String,
    /// Maximum tokens per job (0 = unlimited).
    pub max_tokens_per_job: u64,
    /// Max time per `handle_message()` before the agent loop skips it.
    pub handle_message_timeout: Duration,
    /// Max time per self-repair operation (detect/repair stuck jobs, broken tools).
    pub self_repair_op_timeout: Duration,
    /// Max time for session pruning.
    pub session_prune_timeout: Duration,
    /// Expiry window for pending execution gates (approval/auth). Gates older
    /// than this are expired by the background sweep and the paused turn is
    /// resumed with a wrap-up (WI#108). Default 1800s; values outside 30s
    /// through 30 days fall back to the default (the gate sweep ticks every 60s).
    /// Env: `AGENT_PENDING_GATE_EXPIRY_SECS`.
    pub pending_gate_expiry_secs: u64,
    /// SillyTavern-import-subset lorebook behavior.
    pub lorebook: LorebookConfig,
    /// Reflex compiler configuration.
    pub reflex: ReflexConfig,
}

/// Fallback for `pending_gate_expiry_secs` (WI#111).
pub(crate) const PENDING_GATE_EXPIRY_DEFAULT_SECS: u64 = 1800;
/// Floor for `pending_gate_expiry_secs`: below this a gate could expire
/// faster than the 60s background sweep can observe it.
pub(crate) const PENDING_GATE_EXPIRY_MIN_SECS: u64 = 30;
/// Ceiling for `pending_gate_expiry_secs`: longer-lived approval/auth gates
/// retain paused turns and risk overflowing downstream signed time arithmetic.
pub(crate) const PENDING_GATE_EXPIRY_MAX_SECS: u64 = 30 * 24 * 60 * 60;

/// Apply the bounded fallback rule for `pending_gate_expiry_secs`.
pub(crate) fn clamp_pending_gate_expiry_secs(secs: u64) -> u64 {
    if !(PENDING_GATE_EXPIRY_MIN_SECS..=PENDING_GATE_EXPIRY_MAX_SECS).contains(&secs) {
        PENDING_GATE_EXPIRY_DEFAULT_SECS
    } else {
        secs
    }
}

/// Lorebook activation configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LorebookConfig {
    /// Master feature switch. Disabled by default: activation scanning is
    /// computationally expensive and most deployments do not use lorebooks.
    /// Set `LOREBOOK_ENABLED=true` to opt in.
    pub enabled: bool,
    /// Permit primary-scope lorebooks in group chats. Disabled by default to
    /// avoid exposing private lore through participant-controlled triggers.
    pub allow_group_chats: bool,
    /// Newest visible user/assistant messages scanned for trigger keys.
    pub scan_depth: usize,
    /// Maximum activated entries injected into one prompt.
    pub max_entries: usize,
    /// Approximate token budget for the complete rendered lorebook block.
    pub token_budget: usize,
}

impl LorebookConfig {
    /// Hard cap for `LOREBOOK_SCAN_DEPTH`.
    pub const MAX_SCAN_DEPTH: usize = 100;
    /// Hard cap for `LOREBOOK_MAX_ENTRIES`.
    pub const MAX_ACTIVE_ENTRIES: usize = 64;
    /// Hard cap for `LOREBOOK_TOKEN_BUDGET`.
    pub const MAX_TOKEN_BUDGET: usize = 16_000;

    fn resolve() -> Result<Self, ConfigError> {
        let defaults = Self::default();
        Self {
            enabled: parse_bool_env("LOREBOOK_ENABLED", defaults.enabled)?,
            allow_group_chats: parse_bool_env(
                "LOREBOOK_ALLOW_GROUP_CHATS",
                defaults.allow_group_chats,
            )?,
            scan_depth: parse_optional_env("LOREBOOK_SCAN_DEPTH", defaults.scan_depth)?,
            max_entries: parse_optional_env("LOREBOOK_MAX_ENTRIES", defaults.max_entries)?,
            token_budget: parse_optional_env("LOREBOOK_TOKEN_BUDGET", defaults.token_budget)?,
        }
        .validated()
    }

    fn validated(self) -> Result<Self, ConfigError> {
        validate_limit("LOREBOOK_SCAN_DEPTH", self.scan_depth, Self::MAX_SCAN_DEPTH)?;
        validate_limit(
            "LOREBOOK_MAX_ENTRIES",
            self.max_entries,
            Self::MAX_ACTIVE_ENTRIES,
        )?;
        validate_limit(
            "LOREBOOK_TOKEN_BUDGET",
            self.token_budget,
            Self::MAX_TOKEN_BUDGET,
        )?;
        Ok(self)
    }
}

impl Default for LorebookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_group_chats: false,
            scan_depth: 10,
            max_entries: 20,
            token_budget: 1200,
        }
    }
}

fn validate_limit(key: &str, value: usize, maximum: usize) -> Result<(), ConfigError> {
    if value > maximum {
        return Err(ConfigError::InvalidValue {
            key: key.to_string(),
            message: format!("must be at most {maximum}, got {value}"),
        });
    }
    Ok(())
}

/// Configuration for the reflex compiler.
#[derive(Debug, Clone)]
pub struct ReflexConfig {
    /// Whether the reflex compiler is enabled.
    pub enabled: bool,
    /// How often to check for recurring patterns (default: 30 minutes).
    pub check_interval: Duration,
    /// Minimum number of times a pattern must appear before compilation (default: 3).
    pub min_match_count: u32,
    /// Maximum patterns to compile per check cycle (default: 5).
    pub max_patterns_per_run: usize,
    /// Fuzzy matching threshold (Jaro-Winkler similarity, default: 0.85).
    pub fuzzy_threshold: f64,
    /// Whether semantic matching may call the embedding provider (default: false).
    pub semantic_enabled: bool,
    /// Semantic matching threshold (cosine similarity, default: 0.75).
    pub semantic_threshold: f64,
    /// How often the evictor checks for stale patterns (default: 24 hours).
    pub eviction_check_interval: Duration,
    /// Days without a match before a pattern is considered stale (default: 30).
    pub eviction_stale_days: i32,
    /// Maximum number of active patterns per user (default: 50).
    pub max_patterns_per_user: usize,
}

impl Default for ReflexConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            check_interval: Duration::from_secs(1800),
            min_match_count: 3,
            max_patterns_per_run: 5,
            fuzzy_threshold: 0.85,
            semantic_enabled: false,
            semantic_threshold: 0.75,
            eviction_check_interval: Duration::from_secs(86400),
            eviction_stale_days: 30,
            max_patterns_per_user: 50,
        }
    }
}

impl AgentConfig {
    /// Create a test-friendly config without reading env vars.
    #[cfg(feature = "libsql")]
    pub fn for_testing() -> Self {
        Self {
            name: "test-rig".to_string(),
            max_parallel_jobs: 1,
            job_timeout: Duration::from_secs(30),
            stuck_threshold: Duration::from_secs(300),
            repair_check_interval: Duration::from_secs(3600),
            max_repair_attempts: 0,
            use_planning: false,
            session_idle_timeout: Duration::from_secs(3600),
            allow_local_tools: true,
            max_cost_per_day_cents: None,
            max_actions_per_hour: None,
            max_tool_iterations: 10,
            auto_approve_tools: true,
            default_timezone: "UTC".to_string(),
            max_tokens_per_job: 0,
            handle_message_timeout: Duration::from_secs(630),
            self_repair_op_timeout: Duration::from_secs(60),
            session_prune_timeout: Duration::from_secs(30),
            pending_gate_expiry_secs: 1800,
            lorebook: LorebookConfig {
                enabled: false,
                ..LorebookConfig::default()
            },
            reflex: ReflexConfig::default(),
        }
    }

    pub(crate) fn resolve(settings: &Settings) -> Result<Self, ConfigError> {
        warn_agent_env_keys();
        Ok(Self {
            name: parse_optional_env("AGENT_NAME", settings.agent.name.clone())?,
            max_parallel_jobs: parse_optional_env(
                "AGENT_MAX_PARALLEL_JOBS",
                settings.agent.max_parallel_jobs as usize,
            )?,
            job_timeout: Duration::from_secs(parse_optional_env(
                "AGENT_JOB_TIMEOUT_SECS",
                settings.agent.job_timeout_secs,
            )?),
            stuck_threshold: Duration::from_secs(parse_optional_env(
                "AGENT_STUCK_THRESHOLD_SECS",
                settings.agent.stuck_threshold_secs,
            )?),
            repair_check_interval: Duration::from_secs(parse_optional_env(
                "SELF_REPAIR_CHECK_INTERVAL_SECS",
                settings.agent.repair_check_interval_secs,
            )?),
            max_repair_attempts: parse_optional_env(
                "SELF_REPAIR_MAX_ATTEMPTS",
                settings.agent.max_repair_attempts,
            )?,
            use_planning: parse_bool_env("AGENT_USE_PLANNING", settings.agent.use_planning)?,
            session_idle_timeout: Duration::from_secs(parse_optional_env(
                "SESSION_IDLE_TIMEOUT_SECS",
                settings.agent.session_idle_timeout_secs,
            )?),
            allow_local_tools: parse_bool_env("ALLOW_LOCAL_TOOLS", false)?,
            max_cost_per_day_cents: parse_option_env("MAX_COST_PER_DAY_CENTS")?,
            max_actions_per_hour: parse_option_env("MAX_ACTIONS_PER_HOUR")?,
            max_tool_iterations: parse_optional_env(
                "AGENT_MAX_TOOL_ITERATIONS",
                settings.agent.max_tool_iterations,
            )?,
            auto_approve_tools: parse_bool_env(
                "AGENT_AUTO_APPROVE_TOOLS",
                settings.agent.auto_approve_tools,
            )?,
            default_timezone: {
                let tz: String = parse_optional_env(
                    "DEFAULT_TIMEZONE",
                    settings.agent.default_timezone.clone(),
                )?;
                if crate::timezone::parse_timezone(&tz).is_none() {
                    return Err(ConfigError::InvalidValue {
                        key: "DEFAULT_TIMEZONE".into(),
                        message: format!("invalid IANA timezone: '{tz}'"),
                    });
                }
                tz
            },
            max_tokens_per_job: parse_optional_env(
                "AGENT_MAX_TOKENS_PER_JOB",
                settings.agent.max_tokens_per_job,
            )?,
            handle_message_timeout: Duration::from_secs(parse_optional_env(
                "AGENT_HANDLE_MESSAGE_TIMEOUT_SECS",
                settings.agent.handle_message_timeout_secs,
            )?),
            self_repair_op_timeout: Duration::from_secs(parse_optional_env(
                "AGENT_SELF_REPAIR_OP_TIMEOUT_SECS",
                settings.agent.self_repair_op_timeout_secs,
            )?),
            session_prune_timeout: Duration::from_secs(parse_optional_env(
                "AGENT_SESSION_PRUNE_TIMEOUT_SECS",
                settings.agent.session_prune_timeout_secs,
            )?),
            pending_gate_expiry_secs: parse_optional_env(
                "AGENT_PENDING_GATE_EXPIRY_SECS",
                settings.agent.pending_gate_expiry_secs,
            )?,
            lorebook: LorebookConfig::resolve()?,
            reflex: ReflexConfig {
                enabled: parse_bool_env("REFLEX_COMPILER_ENABLED", false)?,
                check_interval: Duration::from_secs(parse_optional_env(
                    "REFLEX_COMPILER_INTERVAL_SECS",
                    1800u64,
                )?),
                min_match_count: parse_optional_env("REFLEX_MIN_MATCH_COUNT", 3u32)?,
                max_patterns_per_run: parse_optional_env("REFLEX_MAX_PATTERNS_PER_RUN", 5usize)?,
                fuzzy_threshold: parse_optional_env("REFLEX_FUZZY_THRESHOLD", 0.85f64)?,
                semantic_enabled: parse_bool_env("REFLEX_SEMANTIC_ENABLED", false)?,
                semantic_threshold: parse_optional_env("REFLEX_SEMANTIC_THRESHOLD", 0.75f64)?,
                eviction_check_interval: Duration::from_secs(parse_optional_env(
                    "REFLEX_EVICTION_INTERVAL_SECS",
                    86400u64,
                )?),
                eviction_stale_days: parse_optional_env("REFLEX_EVICTION_STALE_DAYS", 30i32)?,
                max_patterns_per_user: parse_optional_env("REFLEX_MAX_PATTERNS_PER_USER", 50usize)?,
            },
        })
    }
}

/// Diagnose misspelled agent settings without treating unrelated service or
/// extension variables as daemon config. Log names only, never env values.
fn warn_agent_env_keys() {
    for (key, _) in std::env::vars_os() {
        let Some(key) = key.to_str() else { continue };
        if let Some(message) = agent_env_key_warning(key) {
            tracing::warn!(env_key = key, "{message}");
        }
    }
}

fn agent_env_key_warning(key: &str) -> Option<&'static str> {
    // Keep in sync with AgentConfig::resolve and the engine supervision flag.
    const KNOWN: &[&str] = &[
        "AGENT_NAME",
        "AGENT_MAX_PARALLEL_JOBS",
        "AGENT_JOB_TIMEOUT_SECS",
        "AGENT_STUCK_THRESHOLD_SECS",
        "AGENT_USE_PLANNING",
        "AGENT_MAX_TOOL_ITERATIONS",
        "AGENT_AUTO_APPROVE_TOOLS",
        "AGENT_MAX_TOKENS_PER_JOB",
        "AGENT_HANDLE_MESSAGE_TIMEOUT_SECS",
        "AGENT_SELF_REPAIR_OP_TIMEOUT_SECS",
        "AGENT_SESSION_PRUNE_TIMEOUT_SECS",
        "AGENT_PENDING_GATE_EXPIRY_SECS",
        "AGENT_SUPERVISED_MODE",
    ];
    if key == "AUTO_APPROVE_TOOLS" {
        Some("Legacy approval setting; use AGENT_AUTO_APPROVE_TOOLS (preferred when both are set)")
    } else if (key.starts_with("AGENT_") || key.starts_with("AUTO_APPROVE_"))
        && !KNOWN.contains(&key)
    {
        Some("Unrecognized agent environment key; this setting is ignored")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_env_diagnostics_distinguish_aliases_typos_and_other_services() {
        assert!(agent_env_key_warning("AGENT_AUTO_APPROVE_TOOLS").is_none());
        assert!(agent_env_key_warning("AGENT_SUPERVISED_MODE").is_none());
        assert!(
            agent_env_key_warning("AUTO_APPROVE_TOOLS")
                .unwrap()
                .contains("Legacy")
        );
        assert!(
            agent_env_key_warning("AGENT_AUTO_APROVE_TOOLS")
                .unwrap()
                .contains("ignored")
        );
        assert!(
            agent_env_key_warning("AUTO_APPROVE_TOOL")
                .unwrap()
                .contains("ignored")
        );
        assert!(agent_env_key_warning("PATH").is_none());
        assert!(agent_env_key_warning("XMPP_BRIDGE_TOKEN").is_none());
    }

    #[test]
    fn test_default_timezone_rejects_invalid() {
        let mut settings = Settings::default();
        settings.agent.default_timezone = "Fake/Zone".to_string();

        let result = AgentConfig::resolve(&settings);
        assert!(result.is_err(), "invalid IANA timezone should be rejected");
    }

    #[test]
    fn test_default_timezone_accepts_valid() {
        let settings = Settings::default(); // default is "UTC"
        let config = AgentConfig::resolve(&settings).expect("resolve");
        assert_eq!(config.default_timezone, "UTC");
    }

    #[test]
    fn reflex_semantic_matching_defaults_off() {
        assert!(!ReflexConfig::default().semantic_enabled);
    }

    #[test]
    fn pending_gate_expiry_defaults_to_1800() {
        let settings = Settings::default();
        let config = AgentConfig::resolve(&settings).expect("resolve");
        assert_eq!(config.pending_gate_expiry_secs, 1800);
        assert_eq!(
            clamp_pending_gate_expiry_secs(config.pending_gate_expiry_secs),
            1800
        );
    }

    #[test]
    fn pending_gate_expiry_settings_value_flows_through_when_in_bounds() {
        let mut settings = Settings::default();
        settings.agent.pending_gate_expiry_secs = 900;
        let config = AgentConfig::resolve(&settings).expect("resolve");
        assert_eq!(config.pending_gate_expiry_secs, 900);
        assert_eq!(
            clamp_pending_gate_expiry_secs(config.pending_gate_expiry_secs),
            900
        );
    }

    #[test]
    fn pending_gate_expiry_below_floor_falls_back_to_default() {
        // Under 30s a gate could expire faster than the 60s sweep observes it.
        for secs in [0u64, 1, 29] {
            assert_eq!(
                clamp_pending_gate_expiry_secs(secs),
                PENDING_GATE_EXPIRY_DEFAULT_SECS,
                "{secs}s must fall back to the default"
            );
        }
        assert_eq!(
            clamp_pending_gate_expiry_secs(PENDING_GATE_EXPIRY_MIN_SECS),
            PENDING_GATE_EXPIRY_MIN_SECS
        );
    }

    #[test]
    fn pending_gate_expiry_above_ceiling_falls_back_to_default() {
        assert_eq!(
            clamp_pending_gate_expiry_secs(PENDING_GATE_EXPIRY_MAX_SECS),
            PENDING_GATE_EXPIRY_MAX_SECS
        );

        for secs in [
            PENDING_GATE_EXPIRY_MAX_SECS + 1,
            i64::MAX as u64 / 1000 + 1,
            i64::MAX as u64 + 1,
            u64::MAX,
        ] {
            assert_eq!(
                clamp_pending_gate_expiry_secs(secs),
                PENDING_GATE_EXPIRY_DEFAULT_SECS,
                "{secs}s must fall back to the default"
            );
        }
    }

    #[test]
    fn lorebook_defaults_are_disabled_bounded_and_group_private() {
        let config = LorebookConfig::default()
            .validated()
            .expect("valid defaults");
        assert!(!config.enabled);
        assert!(!config.allow_group_chats);
        assert_eq!(config.scan_depth, 10);
        assert_eq!(config.max_entries, 20);
        assert_eq!(config.token_budget, 1200);
    }

    #[test]
    fn lorebook_rejects_values_above_hard_limits() {
        let result = LorebookConfig {
            scan_depth: LorebookConfig::MAX_SCAN_DEPTH + 1,
            ..LorebookConfig::default()
        }
        .validated();
        assert!(matches!(result, Err(ConfigError::InvalidValue { .. })));
    }
}
