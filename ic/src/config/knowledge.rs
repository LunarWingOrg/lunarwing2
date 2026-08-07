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
