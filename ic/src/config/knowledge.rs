use std::path::PathBuf;

use crate::bootstrap::lunarwing_base_dir;
use crate::config::helpers::{parse_bool_env, parse_optional_env};
use crate::error::ConfigError;

/// Study-notes stale-sweep configuration.
///
/// Controls the background lifecycle sweep that marks aging study notes
/// `stale`. Maps to `crate::workspace::knowledge_sweep::KnowledgeSweepConfig`.
#[derive(Debug, Clone)]
pub struct KnowledgeSweepConfig {
    /// Whether the sweep is enabled. Env: `KNOWLEDGE_SWEEP_ENABLED` (default: true).
    pub enabled: bool,
    /// Notes studied (or last updated) more than this many days ago are marked
    /// stale. Env: `KNOWLEDGE_STALE_AFTER_DAYS` (default: 30).
    pub stale_after_days: u32,
    /// Minimum hours between sweeps. Env: `KNOWLEDGE_SWEEP_CADENCE_HOURS` (default: 24).
    pub cadence_hours: u32,
    /// Whether the shared herd scope is swept in addition to the private
    /// scope. Env: `KNOWLEDGE_SWEEP_INCLUDE_SHARED` (default: true).
    pub include_shared: bool,
}

impl Default for KnowledgeSweepConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stale_after_days: 30,
            cadence_hours: 24,
            include_shared: true,
        }
    }
}

impl KnowledgeSweepConfig {
    pub(crate) fn resolve() -> Result<Self, ConfigError> {
        Ok(Self {
            enabled: parse_bool_env("KNOWLEDGE_SWEEP_ENABLED", true)?,
            stale_after_days: parse_optional_env("KNOWLEDGE_STALE_AFTER_DAYS", 30)?,
            cadence_hours: parse_optional_env("KNOWLEDGE_SWEEP_CADENCE_HOURS", 24)?,
            include_shared: parse_bool_env("KNOWLEDGE_SWEEP_INCLUDE_SHARED", true)?,
        })
    }

    /// Convert to the workspace sweep config, resolving the state directory
    /// to the standard `~/.lunarwing` location.
    pub fn to_workspace_config(&self) -> crate::workspace::knowledge_sweep::KnowledgeSweepConfig {
        crate::workspace::knowledge_sweep::KnowledgeSweepConfig {
            enabled: self.enabled,
            stale_after_days: self.stale_after_days,
            cadence_hours: self.cadence_hours,
            include_shared: self.include_shared,
            state_dir: lunarwing_base_dir(),
        }
    }
}

/// One-shot study-notes knowledge seed configuration (new tenants only).
///
/// Maps to `crate::workspace::knowledge_seed::KnowledgeSeedConfig`. The seed
/// imports the synced lw2studymcp checkout into the tenant's private
/// knowledge store on first startup. mt-admin writes
/// `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` exclusively on the add-tenant path, so
/// existing tenants upgraded to a seed-capable binary are never populated.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeSeedConfig {
    /// Whether the one-shot seed is armed. Env:
    /// `KNOWLEDGE_SEED_FROM_STUDY_MCP` (default: false).
    pub enabled: bool,
    /// lw2studymcp checkout to seed from. Env: `LW2STUDYMCP_HOME`
    /// (`None` disables the seed even when armed).
    pub source_home: Option<PathBuf>,
}

impl KnowledgeSeedConfig {
    pub(crate) fn resolve() -> Result<Self, ConfigError> {
        Ok(Self {
            enabled: parse_bool_env("KNOWLEDGE_SEED_FROM_STUDY_MCP", false)?,
            source_home: std::env::var_os(crate::tools::mcp::config::STUDY_NOTES_HOME_ENV)
                .map(PathBuf::from),
        })
    }

    /// Convert to the workspace seed config, resolving the state directory
    /// to the standard `~/.lunarwing` location. Returns `None` unless the
    /// seed is armed and a source checkout is configured.
    pub fn to_workspace_config(
        &self,
    ) -> Option<crate::workspace::knowledge_seed::KnowledgeSeedConfig> {
        if !self.enabled {
            return None;
        }
        let source_home = self.source_home.clone()?;
        Some(crate::workspace::knowledge_seed::KnowledgeSeedConfig {
            enabled: true,
            source_home,
            state_dir: lunarwing_base_dir(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let config = KnowledgeSweepConfig::default();
        assert!(config.enabled);
        assert_eq!(config.stale_after_days, 30);
        assert_eq!(config.cadence_hours, 24);
        assert!(config.include_shared);
    }

    #[test]
    fn seed_defaults_disabled() {
        let config = KnowledgeSeedConfig::default();
        assert!(!config.enabled);
        assert!(config.source_home.is_none());
        assert!(config.to_workspace_config().is_none());
    }

    #[test]
    fn seed_workspace_config_requires_checkout() {
        let mut config = KnowledgeSeedConfig {
            enabled: true,
            source_home: None,
        };
        assert!(config.to_workspace_config().is_none());
        config.source_home = Some(PathBuf::from("/var/cache/lunarwing/lw2studymcp"));
        let workspace = config.to_workspace_config().expect("seed config");
        assert!(workspace.enabled);
        assert_eq!(
            workspace.source_home,
            PathBuf::from("/var/cache/lunarwing/lw2studymcp")
        );
    }

    #[test]
    fn workspace_config_mapping() {
        let config = KnowledgeSweepConfig {
            enabled: false,
            stale_after_days: 90,
            cadence_hours: 12,
            include_shared: false,
        };
        let ws = config.to_workspace_config();
        assert!(!ws.enabled);
        assert_eq!(ws.stale_after_days, 90);
        assert_eq!(ws.cadence_hours, 12);
        assert!(!ws.include_shared);
    }
}
