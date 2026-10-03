//! File-backed persistence for pending gates.
//!
//! Writes are atomic: state is serialized to a sibling temp file, fsynced,
//! then renamed over the live file (same pattern as `violation_store`), so a
//! crash mid-write can never leave a truncated `pending-gates.json`. Mutual
//! exclusion uses a dedicated `pending-gates.lock` sibling — the data file
//! itself cannot carry the lock because rename replaces its inode, which would
//! let a second opener lock the new inode while a stale handle still holds the
//! old one.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::bootstrap::lunarwing_base_dir;

use super::pending::{PendingGate, PendingGateKey};
use super::store::{GatePersistence, GateStoreError};

#[derive(Debug, Default, Serialize, Deserialize)]
struct PendingGateFile {
    version: u8,
    gates: Vec<PendingGate>,
}

/// JSON-file persistence for pending gates under `~/.lunarwing/`.
#[derive(Debug, Clone)]
pub struct FileGatePersistence {
    path: PathBuf,
}

impl FileGatePersistence {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn default_path() -> PathBuf {
        lunarwing_base_dir().join("pending-gates.json")
    }

    pub fn with_default_path() -> Self {
        Self::new(Self::default_path())
    }

    /// `pending-gates.json` → `pending-gates.lock`.
    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    /// `pending-gates.json` → `pending-gates.json.tmp`.
    fn tmp_path(&self) -> PathBuf {
        self.path.with_extension("json.tmp")
    }

    /// Serialize concurrent read-modify-write cycles with a stable lock file.
    fn open_locked_file(&self) -> Result<std::fs::File, GateStoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| GateStoreError::Persistence {
                reason: format!("create parent dir '{}': {e}", parent.display()),
            })?;
        }

        let lock_path = self.lock_path();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| GateStoreError::Persistence {
                reason: format!("open '{}': {e}", lock_path.display()),
            })?;

        file.lock().map_err(|e| GateStoreError::Persistence {
            reason: format!("lock '{}': {e}", lock_path.display()),
        })?;
        Ok(file)
    }

    fn read_state(&self) -> Result<PendingGateFile, GateStoreError> {
        let content = match fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PendingGateFile {
                    version: 1,
                    gates: Vec::new(),
                });
            }
            Err(e) => {
                return Err(GateStoreError::Persistence {
                    reason: format!("read '{}': {e}", self.path.display()),
                });
            }
        };

        if content.trim().is_empty() {
            return Ok(PendingGateFile {
                version: 1,
                gates: Vec::new(),
            });
        }

        serde_json::from_str(&content).map_err(|e| GateStoreError::Persistence {
            reason: format!("parse '{}': {e}", self.path.display()),
        })
    }

    /// Atomic write: serialize to a temp file in the same directory, fsync,
    /// then rename over the live file so a crash mid-write keeps the last
    /// complete state.
    fn write_state(&self, state: &PendingGateFile) -> Result<(), GateStoreError> {
        let json = serde_json::to_vec_pretty(state).map_err(|e| GateStoreError::Persistence {
            reason: format!("serialize '{}': {e}", self.path.display()),
        })?;

        let tmp = self.tmp_path();
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| GateStoreError::Persistence {
                reason: format!("open '{}': {e}", tmp.display()),
            })?;
        file.write_all(&json)
            .and_then(|_| file.sync_all())
            .map_err(|e| GateStoreError::Persistence {
                reason: format!("write '{}': {e}", tmp.display()),
            })?;
        drop(file);

        fs::rename(&tmp, &self.path).map_err(|e| GateStoreError::Persistence {
            reason: format!("rename '{}' to '{}': {e}", tmp.display(), self.path.display()),
        })?;

        // Best-effort directory fsync so the rename itself survives a crash.
        if let Some(parent) = self.path.parent()
            && let Ok(dir) = std::fs::File::open(parent)
        {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

#[async_trait]
impl GatePersistence for FileGatePersistence {
    async fn save(&self, gate: &PendingGate) -> Result<(), GateStoreError> {
        let _lock = self.open_locked_file()?;
        let mut state = self.read_state()?;
        state.gates.retain(|existing| existing.key() != gate.key());
        state.gates.push(gate.clone());
        self.write_state(&state)
    }

    async fn remove(&self, key: &PendingGateKey) -> Result<(), GateStoreError> {
        let _lock = self.open_locked_file()?;
        let mut state = self.read_state()?;
        state.gates.retain(|existing| existing.key() != *key);
        self.write_state(&state)
    }

    async fn load_all(&self) -> Result<Vec<PendingGate>, GateStoreError> {
        let _lock = self.open_locked_file()?;
        Ok(self.read_state()?.gates)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use lunarwing_engine::{ConversationId, ResumeKind, ThreadId};

    use super::*;

    fn sample_gate() -> PendingGate {
        PendingGate {
            request_id: uuid::Uuid::new_v4(),
            gate_name: "approval".into(),
            user_id: "user1".into(),
            thread_id: ThreadId::new(),
            conversation_id: ConversationId::new(),
            source_channel: "web".into(),
            action_name: "shell".into(),
            call_id: "call_1".into(),
            parameters: serde_json::json!({"cmd":"ls"}),
            display_parameters: None,
            description: "pending".into(),
            resume_kind: ResumeKind::Approval { allow_always: true },
            created_at: Utc::now(),
            expires_at: Utc::now() + Duration::minutes(30),
            original_message: None,
            resume_output: None,
        }
    }

    #[tokio::test]
    async fn file_persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let persistence = FileGatePersistence::new(dir.path().join("pending-gates.json"));
        let gate = sample_gate();
        let key = gate.key();

        persistence.save(&gate).await.unwrap();
        let loaded = persistence.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].request_id, gate.request_id);

        persistence.remove(&key).await.unwrap();
        assert!(persistence.load_all().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn save_writes_via_atomic_temp_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-gates.json");
        let persistence = FileGatePersistence::new(path.clone());

        persistence.save(&sample_gate()).await.unwrap();

        assert!(path.exists());
        // A completed write consumes the temp file via rename.
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[tokio::test]
    async fn interrupted_write_preserves_last_good_state() {
        // Regression (work item #299): a crash mid-write must not corrupt the
        // pending-gates file. The temp file may be left truncated; the live
        // file must still hold the last complete state.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-gates.json");
        let persistence = FileGatePersistence::new(path.clone());
        let gate = sample_gate();
        persistence.save(&gate).await.unwrap();

        // Simulate a kill mid-write: partial JSON stranded in the temp file.
        std::fs::write(
            path.with_extension("json.tmp"),
            b"{\"version\":1,\"gates\":[{\"request_id\":\"",
        )
        .unwrap();

        let loaded = persistence.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].request_id, gate.request_id);

        // A later successful save reuses and renames over the stale temp file.
        let second = sample_gate();
        persistence.save(&second).await.unwrap();
        let loaded = persistence.load_all().await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(!path.with_extension("json.tmp").exists());
    }
}
