//! Configuration management CLI commands.
//!
//! Commands for viewing and modifying settings.
//! Settings are stored in the database (env > DB > default).
//!
//! Every command reads and writes the settings scope the daemon reads: the
//! owner id resolved from the layered environment (`LUNARWING_OWNER_ID`),
//! falling back to `default` only when nothing is configured. The resolved
//! scope is named in the command output so an operator can see which row is
//! in force instead of assuming the write reached the daemon.

use std::sync::Arc;

use clap::Subcommand;

use crate::db::SettingsStore;
use crate::settings::Settings;

#[derive(Subcommand, Debug, Clone)]
pub enum ConfigCommand {
    /// Generate a default config.toml file
    Init {
        /// Output path (default: ~/.lunarwing/config.toml)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,

        /// Overwrite existing file
        #[arg(long)]
        force: bool,
    },

    /// List all settings and their current values
    List {
        /// Show only settings matching this prefix (e.g., "agent", "heartbeat")
        #[arg(short, long)]
        filter: Option<String>,
    },

    /// Get a specific setting value
    Get {
        /// Setting path (e.g., "agent.max_parallel_jobs")
        path: String,
    },

    /// Set a setting value
    Set {
        /// Setting path (e.g., "agent.max_parallel_jobs")
        path: String,

        /// Value to set
        value: String,
    },

    /// Reset a setting to its default value
    Reset {
        /// Setting path (e.g., "agent.max_parallel_jobs")
        path: String,
    },

    /// Show the settings storage info
    Path,
}

/// Run a config command.
///
/// Connects to the database to read/write settings. Falls back to disk
/// if the database is not available.
pub async fn run_config_command(cmd: ConfigCommand) -> anyhow::Result<()> {
    let storage = resolve_storage().await;
    let store = storage.store();
    let scope = storage.owner_id.as_str();

    match cmd {
        ConfigCommand::Init { output, force } => init_toml(store, scope, output, force).await,
        ConfigCommand::List { filter } => list_settings(store, scope, filter).await,
        ConfigCommand::Get { path } => get_setting(store, scope, &path).await,
        ConfigCommand::Set { path, value } => set_setting(store, scope, &path, &value).await,
        ConfigCommand::Reset { path } => reset_setting(store, scope, &path).await,
        ConfigCommand::Path => show_path(store.is_some(), scope),
    }
}

/// Settings scope the daemon uses when neither env nor settings name an owner.
const DEFAULT_OWNER_ID: &str = "default";

/// Settings storage context for the config commands: the DB handle (when
/// reachable) plus the scope the daemon reads and writes.
struct ConfigCliStorage {
    db: Option<Arc<dyn crate::db::Database>>,
    owner_id: String,
}

impl ConfigCliStorage {
    /// Borrow the DB as a settings store; `None` means disk fallback.
    fn store(&self) -> Option<&(dyn SettingsStore + Sync)> {
        self.db
            .as_deref()
            .map(|db| db as &(dyn SettingsStore + Sync))
    }
}

/// Resolve the DB handle and the settings scope for the config commands.
///
/// `Config::from_env()` runs the same owner-id ladder as daemon startup
/// (env > TOML > `default`), and it loads the bootstrap env file that
/// mt-admin writes `LUNARWING_OWNER_ID` into, so on a tenant the resolved
/// owner is the tenant scope the daemon reads.
async fn resolve_storage() -> ConfigCliStorage {
    let config = match crate::config::Config::from_env().await {
        Ok(config) => config,
        Err(e) => {
            eprintln!(
                "Warning: Could not load configuration ({}), using disk fallback",
                e
            );
            return ConfigCliStorage {
                db: None,
                owner_id: DEFAULT_OWNER_ID.to_string(),
            };
        }
    };

    let db = match crate::db::connect_from_config(&config.database).await {
        Ok(db) => Some(db),
        Err(e) => {
            eprintln!(
                "Warning: Could not connect to database ({}), using disk fallback",
                e
            );
            None
        }
    };

    ConfigCliStorage {
        db,
        owner_id: config.owner_id,
    }
}

/// Load settings: DB (owner scope) if available, else defaults.
async fn load_settings(store: Option<&(dyn SettingsStore + Sync)>, owner_id: &str) -> Settings {
    if let Some(store) = store {
        match store.get_all_settings(owner_id).await {
            Ok(map) if !map.is_empty() => return Settings::from_db_map(&map),
            _ => {}
        }
    }
    Settings::default()
}

/// List all settings.
async fn list_settings(
    store: Option<&(dyn SettingsStore + Sync)>,
    owner_id: &str,
    filter: Option<String>,
) -> anyhow::Result<()> {
    let settings = load_settings(store, owner_id).await;
    let all = settings.list();

    let max_key_len = all.iter().map(|(k, _)| k.len()).max().unwrap_or(0);

    let source = if store.is_some() { "database" } else { "disk" };
    println!("Settings (scope: {}, source: {}):", owner_id, source);
    println!();

    for (key, value) in all {
        if let Some(ref f) = filter
            && !key.starts_with(f)
        {
            continue;
        }

        let display_value = if value.len() > 60 {
            format!("{}...", &value[..57])
        } else {
            value
        };

        println!("  {:width$}  {}", key, display_value, width = max_key_len);
    }

    Ok(())
}

/// Get a specific setting.
async fn get_setting(
    store: Option<&(dyn SettingsStore + Sync)>,
    owner_id: &str,
    path: &str,
) -> anyhow::Result<()> {
    let settings = load_settings(store, owner_id).await;

    match settings.get(path) {
        Some(value) => {
            println!("{}", value);
            println!("Scope: {}", owner_id);
            Ok(())
        }
        None => {
            anyhow::bail!("Setting not found in scope '{}': {}", owner_id, path);
        }
    }
}

/// Set a setting value.
async fn set_setting(
    store: Option<&(dyn SettingsStore + Sync)>,
    owner_id: &str,
    path: &str,
    value: &str,
) -> anyhow::Result<()> {
    let mut settings = load_settings(store, owner_id).await;

    settings
        .set(path, value)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let store = store.ok_or_else(|| {
        anyhow::anyhow!("Database connection required to save settings. Check DATABASE_URL.")
    })?;
    let json_value = match serde_json::from_str::<serde_json::Value>(value) {
        Ok(v) => v,
        Err(_) => serde_json::Value::String(value.to_string()),
    };
    store
        .set_setting(owner_id, path, &json_value)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to save to database: {}", e))?;

    println!("Set {} = {} (scope: {})", path, value, owner_id);
    Ok(())
}

/// Reset a setting to default.
async fn reset_setting(
    store: Option<&(dyn SettingsStore + Sync)>,
    owner_id: &str,
    path: &str,
) -> anyhow::Result<()> {
    let default = Settings::default();
    let default_value = default
        .get(path)
        .ok_or_else(|| anyhow::anyhow!("Unknown setting: {}", path))?;

    let store = store.ok_or_else(|| {
        anyhow::anyhow!("Database connection required to reset settings. Check DATABASE_URL.")
    })?;
    store
        .delete_setting(owner_id, path)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to delete setting from database: {}", e))?;

    println!(
        "Reset {} to default in scope '{}': {}",
        path, owner_id, default_value
    );
    Ok(())
}

/// Generate a default TOML config file.
async fn init_toml(
    store: Option<&(dyn SettingsStore + Sync)>,
    owner_id: &str,
    output: Option<std::path::PathBuf>,
    force: bool,
) -> anyhow::Result<()> {
    let path = output.unwrap_or_else(Settings::default_toml_path);

    if path.exists() && !force {
        anyhow::bail!(
            "Config file already exists: {}\nUse --force to overwrite.",
            path.display()
        );
    }

    // Start from current settings (DB or defaults) so the generated file
    // reflects the user's existing configuration.
    let settings = load_settings(store, owner_id).await;

    settings
        .save_toml(&path)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    println!(
        "Config file written to {} (settings scope: {})",
        path.display(),
        owner_id
    );
    println!();
    println!("Edit the file to customize settings.");
    println!("Priority: env var > config.toml > database > defaults");
    Ok(())
}

/// Show the settings storage info.
fn show_path(has_db: bool, owner_id: &str) -> anyhow::Result<()> {
    if has_db {
        println!(
            "Settings stored in: database (settings table, scope: {})",
            owner_id
        );
    } else {
        println!(
            "Settings stored in: PostgreSQL (not connected, using defaults, scope: {})",
            owner_id
        );
    }
    println!(
        "Env config:         {}",
        crate::bootstrap::lunarwing_env_path().display()
    );

    let toml_path = Settings::default_toml_path();
    let toml_status = if toml_path.exists() {
        "found"
    } else {
        "not found (run `lunarwing config init` to create)"
    };
    println!(
        "TOML config:        {} ({})",
        toml_path.display(),
        toml_status
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use std::collections::HashMap;

    use async_trait::async_trait;
    use tokio::sync::RwLock;

    use crate::error::DatabaseError;
    use crate::history::SettingRow;

    /// Minimal in-memory `SettingsStore` keyed by `(user_id, key)`.
    #[derive(Default)]
    struct TestSettingsStore {
        values: RwLock<HashMap<String, HashMap<String, serde_json::Value>>>,
    }

    #[async_trait]
    impl SettingsStore for TestSettingsStore {
        async fn get_setting(
            &self,
            user_id: &str,
            key: &str,
        ) -> Result<Option<serde_json::Value>, DatabaseError> {
            Ok(self
                .values
                .read()
                .await
                .get(user_id)
                .and_then(|row| row.get(key).cloned()))
        }

        async fn get_setting_full(
            &self,
            _user_id: &str,
            _key: &str,
        ) -> Result<Option<SettingRow>, DatabaseError> {
            Ok(None)
        }

        async fn set_setting(
            &self,
            user_id: &str,
            key: &str,
            value: &serde_json::Value,
        ) -> Result<(), DatabaseError> {
            self.values
                .write()
                .await
                .entry(user_id.to_string())
                .or_default()
                .insert(key.to_string(), value.clone());
            Ok(())
        }

        async fn compare_and_set_setting(
            &self,
            user_id: &str,
            key: &str,
            expected: Option<&serde_json::Value>,
            value: &serde_json::Value,
        ) -> Result<bool, DatabaseError> {
            let mut values = self.values.write().await;
            let current = values.get(user_id).and_then(|row| row.get(key));
            if current != expected {
                return Ok(false);
            }
            values
                .entry(user_id.to_string())
                .or_default()
                .insert(key.to_string(), value.clone());
            Ok(true)
        }

        async fn delete_setting(&self, user_id: &str, key: &str) -> Result<bool, DatabaseError> {
            Ok(self
                .values
                .write()
                .await
                .get_mut(user_id)
                .and_then(|row| row.remove(key))
                .is_some())
        }

        async fn list_settings(&self, _user_id: &str) -> Result<Vec<SettingRow>, DatabaseError> {
            Ok(vec![])
        }

        async fn get_all_settings(
            &self,
            user_id: &str,
        ) -> Result<HashMap<String, serde_json::Value>, DatabaseError> {
            Ok(self
                .values
                .read()
                .await
                .get(user_id)
                .cloned()
                .unwrap_or_default())
        }

        async fn set_all_settings(
            &self,
            user_id: &str,
            settings: &HashMap<String, serde_json::Value>,
        ) -> Result<(), DatabaseError> {
            self.values
                .write()
                .await
                .insert(user_id.to_string(), settings.clone());
            Ok(())
        }

        async fn has_settings(&self, user_id: &str) -> Result<bool, DatabaseError> {
            Ok(self
                .values
                .read()
                .await
                .get(user_id)
                .is_some_and(|row| !row.is_empty()))
        }
    }

    #[test]
    fn test_list_settings() {
        // Just verify it doesn't panic
        let settings = Settings::default();
        let list = settings.list();
        assert!(!list.is_empty());
    }

    #[test]
    fn test_get_set_reset() {
        let _dir = tempdir().unwrap();

        let mut settings = Settings::default();

        // Set a value
        settings.set("agent.name", "testbot").unwrap();
        assert_eq!(settings.agent.name, "testbot");

        // Reset to default
        settings.reset("agent.name").unwrap();
        assert_eq!(settings.agent.name, "lunarwing");
    }

    /// WI#716: on a tenant the CLI must write the scope the daemon reads
    /// (`LUNARWING_OWNER_ID` -> owner id), not the legacy `default` scope.
    #[tokio::test]
    async fn set_writes_the_owner_scope_the_daemon_reads() {
        let store = TestSettingsStore::default();
        let store_ref = &store as &(dyn SettingsStore + Sync);
        let owner_id = "netbirddnstester";

        set_setting(Some(store_ref), owner_id, "agent.name", "tenantbot")
            .await
            .unwrap();

        // The daemon loads its settings with the same resolved owner id.
        let daemon_view = store.get_all_settings(owner_id).await.unwrap();
        assert_eq!(
            daemon_view.get("agent.name"),
            Some(&serde_json::json!("tenantbot"))
        );

        // The legacy `default` scope must stay untouched.
        assert!(store.get_all_settings("default").await.unwrap().is_empty());

        // `get` reads back from the same scope it wrote to.
        let read_back = load_settings(Some(store_ref), owner_id).await;
        assert_eq!(read_back.get("agent.name").as_deref(), Some("tenantbot"));
        let default_scope = load_settings(Some(store_ref), DEFAULT_OWNER_ID).await;
        assert_eq!(
            default_scope.get("agent.name").as_deref(),
            Some("lunarwing")
        );
    }

    /// Without an owner id the commands keep targeting the `default` scope.
    #[tokio::test]
    async fn set_without_owner_keeps_the_default_scope() {
        let store = TestSettingsStore::default();
        let store_ref = &store as &(dyn SettingsStore + Sync);

        set_setting(Some(store_ref), DEFAULT_OWNER_ID, "agent.name", "single")
            .await
            .unwrap();

        let rows = store.get_all_settings(DEFAULT_OWNER_ID).await.unwrap();
        assert_eq!(rows.get("agent.name"), Some(&serde_json::json!("single")));
    }

    /// `reset` deletes from the owner scope, so a value set through the
    /// gateway (caller's scope) does not survive a CLI reset.
    #[tokio::test]
    async fn reset_deletes_from_the_owner_scope() {
        let store = TestSettingsStore::default();
        let store_ref = &store as &(dyn SettingsStore + Sync);
        let owner_id = "tenant-a";

        store
            .set_setting(owner_id, "agent.name", &serde_json::json!("tenantbot"))
            .await
            .unwrap();

        reset_setting(Some(store_ref), owner_id, "agent.name")
            .await
            .unwrap();

        assert!(store.get_all_settings(owner_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn init_toml_creates_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");

        init_toml(None, DEFAULT_OWNER_ID, Some(path.clone()), false)
            .await
            .unwrap();
        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[agent]"));
    }

    #[tokio::test]
    async fn init_toml_refuses_overwrite_without_force() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "existing").unwrap();

        let result = init_toml(None, DEFAULT_OWNER_ID, Some(path.clone()), false).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn init_toml_force_overwrites() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "old content").unwrap();

        init_toml(None, DEFAULT_OWNER_ID, Some(path.clone()), true)
            .await
            .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[agent]"));
    }
}
