//! File-backed persistence for pending gates.
//!
//! Writes are atomic: state is serialized to a unique sibling temp file,
//! fsynced, then renamed over the live file (same pattern as
//! `violation_store`), so a crash mid-write can never leave a truncated
//! `pending-gates.json`. Mutual exclusion uses a dedicated
//! `pending-gates.lock` sibling — the data file itself cannot carry the lock
//! because rename replaces its inode, which would let a second opener lock
//! the new inode while a stale handle still holds the old one.
//!
//! On-disk layout under `$LUNARWING_BASE_DIR/`:
//! - `pending-gates.json` — live state (mode 0600 on unix; it carries
//!   unredacted tool parameters). Exists only after the first save.
//! - `pending-gates.lock` — persistent flock sibling, created on first use.
//! - `pending-gates.json.tmp-*` — transient write targets: consumed by the
//!   rename on success, removed on failure; a crash may strand one, which is
//!   ignored on later reads.
//! - `pending-gates.json.corrupt-<timestamp>` — an unparseable live file
//!   quarantined at read time; the store then falls back to empty state so
//!   pending-gate saves keep working (boot reconciliation fails the orphaned
//!   Waiting threads — the designed degraded path).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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

    /// The live state file this instance persists to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `pending-gates.json` → `pending-gates.lock`.
    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    /// Unique temp target: `pending-gates.json` →
    /// `pending-gates.json.tmp-<pid>-<nanos>-<seq>`. Unique names plus
    /// `create_new` keep a stranded temp file or a pre-planted symlink from
    /// being opened and truncated by a later save.
    fn fresh_tmp_path(&self) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        self.path
            .with_extension(format!("json.tmp-{}-{nanos}-{seq}", std::process::id()))
    }

    /// Quarantine target for an unparseable live file: `pending-gates.json` →
    /// `pending-gates.json.corrupt-<unix-millis>`.
    fn corrupt_path(&self) -> PathBuf {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        self.path.with_extension(format!("json.corrupt-{millis}"))
    }

    /// Serialize concurrent read-modify-write cycles with a stable lock file.
    fn open_locked_file(&self) -> Result<std::fs::File, GateStoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| GateStoreError::Persistence {
                reason: format!("create parent dir '{}': {e}", parent.display()),
            })?;
        }

        let lock_path = self.lock_path();
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts
            .open(&lock_path)
            .map_err(|e| GateStoreError::Persistence {
                reason: format!("open '{}': {e}", lock_path.display()),
            })?;

        file.lock().map_err(|e| GateStoreError::Persistence {
            reason: format!("lock '{}': {e}", lock_path.display()),
        })?;
        Ok(file)
    }

    fn empty_state() -> PendingGateFile {
        PendingGateFile {
            version: 1,
            gates: Vec::new(),
        }
    }

    fn read_state(&self) -> Result<PendingGateFile, GateStoreError> {
        let content = match fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty_state());
            }
            Err(e) => {
                return Err(GateStoreError::Persistence {
                    reason: format!("read '{}': {e}", self.path.display()),
                });
            }
        };

        if content.trim().is_empty() {
            return Ok(Self::empty_state());
        }

        match serde_json::from_str(&content) {
            Ok(state) => Ok(state),
            Err(e) => {
                // An unparseable live file (manual edit, fs corruption,
                // partial backup restore) must not brick every gated tool
                // until an operator deletes it (WI#299): quarantine it and
                // fall back to empty state so saves proceed. If the
                // quarantine rename itself fails, the next successful save's
                // rename still replaces the bad file.
                let quarantine = self.corrupt_path();
                match fs::rename(&self.path, &quarantine) {
                    Ok(()) => {
                        tracing::warn!(
                            error = %e,
                            path = %self.path.display(),
                            quarantine = %quarantine.display(),
                            "unparseable pending-gates file set aside; restoring with empty state"
                        );
                    }
                    Err(rename_error) => {
                        tracing::warn!(
                            error = %e,
                            path = %self.path.display(),
                            quarantine = %quarantine.display(),
                            quarantine_error = %rename_error,
                            "unparseable pending-gates file could not be set aside; restoring with empty state"
                        );
                    }
                }
                Ok(Self::empty_state())
            }
        }
    }

    /// Atomic write: serialize to a unique temp file in the same directory,
    /// fsync, then rename over the live file so a crash mid-write keeps the
    /// last complete state. The temp file is removed when the write or the
    /// rename fails.
    fn write_state(&self, state: &PendingGateFile) -> Result<(), GateStoreError> {
        let json = serde_json::to_vec_pretty(state).map_err(|e| GateStoreError::Persistence {
            reason: format!("serialize '{}': {e}", self.path.display()),
        })?;

        let mut opened = None;
        for _ in 0..5 {
            let tmp = self.fresh_tmp_path();
            let mut opts = OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            match opts.open(&tmp) {
                Ok(file) => {
                    opened = Some((tmp, file));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(GateStoreError::Persistence {
                        reason: format!("open '{}': {e}", tmp.display()),
                    });
                }
            }
        }
        let Some((tmp, mut file)) = opened else {
            return Err(GateStoreError::Persistence {
                reason: format!(
                    "could not create a unique temp file near '{}' after 5 attempts",
                    self.path.display()
                ),
            });
        };

        if let Err(e) = file.write_all(&json).and_then(|_| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&tmp);
            return Err(GateStoreError::Persistence {
                reason: format!("write '{}': {e}", tmp.display()),
            });
        }
        drop(file);

        if let Err(e) = fs::rename(&tmp, &self.path) {
            let _ = fs::remove_file(&tmp);
            return Err(GateStoreError::Persistence {
                reason: format!(
                    "rename '{}' to '{}': {e}",
                    tmp.display(),
                    self.path.display()
                ),
            });
        }

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

    fn dir_entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn file_persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-gates.json");
        let persistence = FileGatePersistence::new(path.clone());
        let gate = sample_gate();
        let key = gate.key();

        persistence.save(&gate).await.unwrap();
        let loaded = persistence.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].request_id, gate.request_id);

        // The persisted state carries unredacted tool parameters, so the
        // data file and the lock sibling must be owner-only (WI#299).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let data_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(data_mode, 0o600, "data file mode");
            let lock_mode = std::fs::metadata(path.with_extension("lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(lock_mode, 0o600, "lock file mode");
        }

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
        // A completed write consumes the temp file via rename; only the live
        // file and the stable lock sibling remain.
        assert_eq!(
            dir_entries(dir.path()),
            vec![
                "pending-gates.json".to_string(),
                "pending-gates.lock".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn interrupted_write_preserves_last_good_state() {
        // Regression (work item #299): a crash mid-write may strand a partial
        // temp file; the live file must still hold the last complete state.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-gates.json");
        let persistence = FileGatePersistence::new(path.clone());
        let gate = sample_gate();
        persistence.save(&gate).await.unwrap();

        // Simulate a kill mid-write: partial JSON stranded in a temp sibling.
        std::fs::write(
            path.with_extension("json.tmp-dead"),
            b"{\"version\":1,\"gates\":[{\"request_id\":\"",
        )
        .unwrap();

        let loaded = persistence.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].request_id, gate.request_id);

        // A later save is unaffected by the stranded temp file.
        let second = sample_gate();
        persistence.save(&second).await.unwrap();
        let loaded = persistence.load_all().await.unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_replaces_live_inode_and_old_readers_keep_last_good_state() {
        use std::io::Read as _;
        use std::os::unix::fs::MetadataExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-gates.json");
        let persistence = FileGatePersistence::new(path.clone());

        persistence.save(&sample_gate()).await.unwrap();

        // A reader holding the pre-save file handle...
        let mut old_reader = std::fs::File::open(&path).unwrap();
        let old_ino = std::fs::metadata(&path).unwrap().ino();

        persistence.save(&sample_gate()).await.unwrap();

        // ...still reads the old, complete JSON from the replaced inode...
        let mut old_contents = String::new();
        old_reader.read_to_string(&mut old_contents).unwrap();
        let old_state: serde_json::Value = serde_json::from_str(&old_contents).unwrap();
        assert_eq!(old_state["gates"].as_array().unwrap().len(), 1);

        // ...while the live path is a new inode carrying both gates.
        let new_ino = std::fs::metadata(&path).unwrap().ino();
        assert_ne!(old_ino, new_ino);
        let loaded = persistence.load_all().await.unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[tokio::test]
    async fn corrupt_live_file_is_quarantined_and_saves_proceed() {
        // Regression (work item #299): an unparseable live file (manual edit,
        // fs corruption, partial backup restore) must not brick every gated
        // tool until an operator deletes it — restore falls back to empty
        // state and sets the bad file aside.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-gates.json");
        let persistence = FileGatePersistence::new(path.clone());
        let gate = sample_gate();
        persistence.save(&gate).await.unwrap();

        std::fs::write(&path, b"{\"version\":1,\"gates\":[{").unwrap();

        assert!(persistence.load_all().await.unwrap().is_empty());
        assert!(!path.exists());
        let quarantined: Vec<String> = dir_entries(dir.path())
            .into_iter()
            .filter(|name| name.starts_with("pending-gates.json.corrupt-"))
            .collect();
        assert_eq!(quarantined.len(), 1);
        assert_eq!(
            std::fs::read(dir.path().join(&quarantined[0])).unwrap(),
            b"{\"version\":1,\"gates\":[{"
        );

        // Saves proceed again, recreating the live file.
        persistence.save(&gate).await.unwrap();
        let loaded = persistence.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].request_id, gate.request_id);
    }
}
