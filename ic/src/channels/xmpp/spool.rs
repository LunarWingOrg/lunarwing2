//! Durable outbound spool for the XMPP channel (WI#277).
//!
//! The outbound mpsc queue is memory-only: a daemon/bridge restart between the
//! tool's enqueue ack and the actual stanza send silently loses every queued
//! message (the WI#270 PR Digest #59 incident class). This module persists
//! each outbound message to `<omemo_store_dir>/spool/` *before* the enqueue
//! ack is returned, and the client task drains the spool on every
//! (`Event::Online`) bind, so anything acked by a previous process is sent
//! after restart.
//!
//! Semantics are at-least-once: a crash after the stanza reached the wire but
//! before the spool record was cleared re-sends the message on the next bind.
//! Sends are claimed by atomic rename (`<key>.json` -> `<key>.json.claiming`)
//! so a spool replay and a late in-memory queue copy of the same record can
//! never both send it within one process.
//!
//! Records are plaintext JSON (same on-disk exposure class as the adjacent
//! OMEMO identity store); the directory is created `0700` and files `0600`.
//! Records that exhaust [`SPOOL_MAX_ATTEMPTS`] send attempts are renamed to
//! `<key>.failed.json` for operator inspection instead of being dropped.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{OutboundAttachment, OutboundMessage};

/// Per-process sequence number in spool keys: two persists in the same
/// millisecond must still sort in enqueue order.
static SPOOL_KEY_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Send attempts before a record is parked as `.failed.json`. With the
/// exponential backoff below, 24 attempts span hours of reconnect churn.
const SPOOL_MAX_ATTEMPTS: u32 = 24;
const SPOOL_RETRY_BASE_SECS: u64 = 30;
const SPOOL_RETRY_MAX_SECS: u64 = 3600;
/// `last_error` is operator-facing; keep records small and single-line-ish.
const MAX_ERROR_LEN: usize = 500;
const SPOOL_RECORD_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpoolAttachment {
    filename: String,
    mime_type: String,
    data_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpoolRecord {
    version: u32,
    id: Uuid,
    queued_at_unix_secs: u64,
    to: String,
    body: String,
    groupchat: bool,
    preferred_device_id: Option<u32>,
    attachments: Vec<SpoolAttachment>,
    attempts: u32,
    last_error: Option<String>,
    next_attempt_after_unix_secs: u64,
}

impl SpoolRecord {
    fn from_message(msg: &OutboundMessage, id: Uuid, queued_at_unix_secs: u64) -> Self {
        Self {
            version: SPOOL_RECORD_VERSION,
            id,
            queued_at_unix_secs,
            to: msg.to.clone(),
            body: msg.body.clone(),
            groupchat: msg.groupchat,
            preferred_device_id: msg.preferred_device_id,
            attachments: msg
                .attachments
                .iter()
                .map(|attachment| SpoolAttachment {
                    filename: attachment.filename.clone(),
                    mime_type: attachment.mime_type.clone(),
                    data_b64: base64::engine::general_purpose::STANDARD.encode(&attachment.data),
                })
                .collect(),
            attempts: 0,
            last_error: None,
            next_attempt_after_unix_secs: 0,
        }
    }

    fn into_message(self, key: String) -> OutboundMessage {
        OutboundMessage {
            to: self.to,
            body: self.body,
            groupchat: self.groupchat,
            preferred_device_id: self.preferred_device_id,
            attachments: self
                .attachments
                .into_iter()
                .map(|attachment| OutboundAttachment {
                    filename: attachment.filename,
                    mime_type: attachment.mime_type,
                    // Validated as decodable before the record was handed out
                    // (`attachments_decodable`), so this never defaults.
                    data: base64::engine::general_purpose::STANDARD
                        .decode(attachment.data_b64)
                        .unwrap_or_default(),
                })
                .collect(),
            spool_key: Some(key),
        }
    }

    /// Records with undecodable attachment payloads are treated as corrupt:
    /// silently decoding to empty bytes would deliver a hollow attachment.
    fn attachments_decodable(&self) -> bool {
        self.attachments.iter().all(|attachment| {
            base64::engine::general_purpose::STANDARD
                .decode(&attachment.data_b64)
                .is_ok()
        })
    }
}

/// A spooled message claimed for a send attempt. The record file has been
/// renamed to `<key>.json.claiming`; resolving the attempt via
/// [`OutboundSpool::complete`], [`OutboundSpool::fail_transient`], or
/// [`OutboundSpool::fail_permanent`] releases the claim.
#[derive(Debug)]
pub struct SpooledMessage {
    pub key: String,
    record: SpoolRecord,
}

impl SpooledMessage {
    pub fn message(&self) -> OutboundMessage {
        self.record.clone().into_message(self.key.clone())
    }
}

/// File-backed durable spool for outbound XMPP messages.
///
/// All operations are synchronous filesystem calls over small JSON records;
/// the volume is bounded by the hourly outbound rate limiter, so this never
/// blocks an async worker meaningfully.
#[derive(Debug, Clone)]
pub struct OutboundSpool {
    dir: PathBuf,
}

impl OutboundSpool {
    pub fn new(dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        set_private_dir_permissions(&dir)?;
        Ok(Self { dir })
    }

    #[cfg(test)]
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Persist a message and return its spool key. Must be called *before* the
    /// enqueue is acked to the caller; on failure the enqueue must be refused.
    pub fn persist(&self, msg: &OutboundMessage) -> io::Result<String> {
        let id = Uuid::new_v4();
        let key = format!(
            "{:013}-{:06}-{}",
            unix_millis(),
            SPOOL_KEY_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            id
        );
        let record = SpoolRecord::from_message(msg, id, unix_secs());
        let data = serde_json::to_vec_pretty(&record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.atomic_write(&self.pending_path(&key), &data)?;
        Ok(key)
    }

    /// Drop a pending record without sending. Used when the in-memory enqueue
    /// fails right after `persist` (the caller gets an error, so no ack was
    /// issued and nothing must be replayed later).
    pub fn discard(&self, key: &str) {
        remove_if_exists(&self.pending_path(key));
    }

    /// Claim a pending record for a send attempt.
    ///
    /// `Ok(Some)` — claimed; the caller owns the send and must resolve the
    /// claim. `Ok(None)` — the record is gone (already sent, claimed by the
    /// spool replay, or quarantined as corrupt); the caller must skip its copy.
    pub fn claim(&self, key: &str) -> io::Result<Option<SpooledMessage>> {
        let pending = self.pending_path(key);
        let Some(record) = self.read_valid_record(&pending, key)? else {
            return Ok(None);
        };
        // Claim by atomic rename AFTER parsing, so a corrupt record is never
        // claimed and a racing claimer loses cleanly with NotFound.
        match fs::rename(&pending, self.claim_path(key)) {
            Ok(()) => Ok(Some(SpooledMessage {
                key: key.to_string(),
                record,
            })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The claimed message was sent; remove the record.
    pub fn complete(&self, key: &str) -> io::Result<()> {
        remove_if_exists(&self.claim_path(key));
        remove_if_exists(&self.pending_path(key));
        Ok(())
    }

    /// The send failed transiently: bump the attempt counter, schedule the
    /// next attempt with exponential backoff, and return the record to the
    /// pending set. Exhausted records become permanent failures instead.
    pub fn fail_transient(&self, claimed: &SpooledMessage, error: &str) -> io::Result<()> {
        let mut record = claimed.record.clone();
        record.attempts = record.attempts.saturating_add(1);
        record.last_error = Some(truncate_error(error));
        if record.attempts >= SPOOL_MAX_ATTEMPTS {
            return self.write_failed(&claimed.key, &record);
        }
        record.next_attempt_after_unix_secs =
            unix_secs().saturating_add(retry_delay_secs(record.attempts));
        let data = serde_json::to_vec_pretty(&record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.atomic_write(&self.pending_path(&claimed.key), &data)?;
        remove_if_exists(&self.claim_path(&claimed.key));
        Ok(())
    }

    /// The send can never succeed (e.g. invalid JID): park the record as
    /// `<key>.failed.json` so it is never retried but remains inspectable.
    pub fn fail_permanent(&self, claimed: &SpooledMessage, error: &str) -> io::Result<()> {
        let mut record = claimed.record.clone();
        record.attempts = record.attempts.saturating_add(1);
        record.last_error = Some(truncate_error(error));
        self.write_failed(&claimed.key, &record)
    }

    /// List spool keys ready for a send attempt, oldest first.
    ///
    /// Also performs spool hygiene: stale claims from a dead process are
    /// recovered (at most one client task is live per process, so any claim
    /// visible here was orphaned), duplicate claim/pending pairs collapse to
    /// the newer pending record, and unparseable records are quarantined.
    /// Records whose backoff has not elapsed yet are skipped.
    pub fn list_pending(&self) -> io::Result<Vec<String>> {
        let mut keys = self.collect_candidate_keys()?;
        keys.sort();
        let now = unix_secs();
        let mut ready = Vec::with_capacity(keys.len());
        for key in keys {
            match self.read_pending_record(&key) {
                Ok(Some(record)) if record.next_attempt_after_unix_secs <= now => ready.push(key),
                Ok(Some(_)) => {}
                Ok(None) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(ready)
    }

    /// Number of pending records (all states awaiting a send), for tests and
    /// diagnostics.
    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        fs::read_dir(&self.dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
                    .filter(|entry| {
                        !entry
                            .file_name()
                            .to_string_lossy()
                            .ends_with(".failed.json")
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    fn pending_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    fn claim_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json.claiming"))
    }

    fn failed_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.failed.json"))
    }

    fn collect_candidate_keys(&self) -> io::Result<Vec<String>> {
        let mut keys = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(claim_stem) = name.strip_suffix(".claiming") {
                self.recover_claim(claim_stem, &mut keys)?;
            } else if name.ends_with(".failed.json")
                || name.ends_with(".corrupt")
                || name.ends_with(".tmp")
            {
                continue;
            } else if let Some(key) = name.strip_suffix(".json") {
                keys.push(key.to_string());
            }
        }
        Ok(keys)
    }

    /// Recover a `<key>.json.claiming` file orphaned by a dead process. When
    /// the pending file also exists, the pending record has newer attempt
    /// bookkeeping (a `fail_transient` write interrupted mid-flight), so the
    /// claim is the duplicate and is removed.
    fn recover_claim(&self, claim_stem: &str, keys: &mut Vec<String>) -> io::Result<()> {
        let claim_path = self.dir.join(format!("{claim_stem}.claiming"));
        let pending_path = self.dir.join(claim_stem);
        if pending_path.exists() {
            remove_if_exists(&claim_path);
            return Ok(());
        }
        match fs::rename(&claim_path, &pending_path) {
            Ok(()) => {
                if let Some(key) = claim_stem.strip_suffix(".json") {
                    keys.push(key.to_string());
                }
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn read_pending_record(&self, key: &str) -> io::Result<Option<SpoolRecord>> {
        self.read_valid_record(&self.pending_path(key), key)
    }

    /// Read and parse a record, quarantining it (and returning `Ok(None)`)
    /// when the JSON or the attachment payload encoding is undecodable.
    fn read_valid_record(
        &self,
        path: &std::path::Path,
        key: &str,
    ) -> io::Result<Option<SpoolRecord>> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        match serde_json::from_slice::<SpoolRecord>(&data) {
            Ok(record) if record.attachments_decodable() => Ok(Some(record)),
            Ok(_) => {
                tracing::warn!(
                    key,
                    "XMPP spool record has undecodable attachment data; quarantining"
                );
                self.quarantine(path, key)?;
                Ok(None)
            }
            Err(e) => {
                tracing::warn!(key, error = %e, "XMPP spool record is corrupt; quarantining");
                self.quarantine(path, key)?;
                Ok(None)
            }
        }
    }

    fn quarantine(&self, path: &std::path::Path, key: &str) -> io::Result<()> {
        let quarantined = self.dir.join(format!("{key}.corrupt"));
        match fs::rename(path, &quarantined) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn write_failed(&self, key: &str, record: &SpoolRecord) -> io::Result<()> {
        let data = serde_json::to_vec_pretty(record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.atomic_write(&self.failed_path(key), &data)?;
        remove_if_exists(&self.claim_path(key));
        remove_if_exists(&self.pending_path(key));
        tracing::error!(
            key,
            attempts = record.attempts,
            error = record.last_error.as_deref().unwrap_or(""),
            "XMPP outbound message permanently failed; parked in spool as .failed.json"
        );
        Ok(())
    }

    fn atomic_write(&self, path: &std::path::Path, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        // Suffix `.tmp` (not a `.tmp-` prefix) so directory scans that filter
        // on a `.json` suffix never mistake an in-flight write for a record.
        let tmp = path.with_extension("tmp");
        let mut file = create_private_file(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        if let Ok(dir) = fs::File::open(&self.dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

fn retry_delay_secs(attempts: u32) -> u64 {
    let shift = attempts.saturating_sub(1).min(10);
    SPOOL_RETRY_BASE_SECS
        .saturating_mul(1u64 << shift)
        .min(SPOOL_RETRY_MAX_SECS)
}

fn truncate_error(error: &str) -> String {
    if error.chars().count() <= MAX_ERROR_LEN {
        return error.to_string();
    }
    error.chars().take(MAX_ERROR_LEN).collect()
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn remove_if_exists(path: &std::path::Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(path = %path.display(), error = %e, "XMPP spool cleanup failed"),
    }
}

#[cfg(unix)]
fn create_private_file(path: &std::path::Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &std::path::Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn set_private_dir_permissions(dir: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_dir: &std::path::Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_message(body: &str) -> OutboundMessage {
        OutboundMessage {
            to: "alice@example.com".to_string(),
            body: body.to_string(),
            groupchat: false,
            preferred_device_id: None,
            attachments: Vec::new(),
            spool_key: None,
        }
    }

    fn test_spool() -> (TempDir, OutboundSpool) {
        let dir = TempDir::new().expect("tempdir");
        let spool = OutboundSpool::new(dir.path().join("spool")).expect("spool init");
        (dir, spool)
    }

    #[test]
    fn persist_then_claim_roundtrips_message() {
        let (_dir, spool) = test_spool();
        let mut msg = test_message("hello");
        msg.preferred_device_id = Some(7);
        msg.attachments.push(OutboundAttachment {
            filename: "note.txt".to_string(),
            mime_type: "text/plain".to_string(),
            data: b"attachment-bytes".to_vec(),
        });

        let key = spool.persist(&msg).expect("persist");
        assert_eq!(spool.pending_count(), 1);

        let claimed = spool.claim(&key).expect("claim").expect("present");
        let restored = claimed.message();
        assert_eq!(restored.to, "alice@example.com");
        assert_eq!(restored.body, "hello");
        assert!(!restored.groupchat);
        assert_eq!(restored.preferred_device_id, Some(7));
        assert_eq!(restored.spool_key.as_deref(), Some(key.as_str()));
        assert_eq!(restored.attachments.len(), 1);
        assert_eq!(restored.attachments[0].data, b"attachment-bytes");

        // The claim moved the record out of the pending set.
        assert_eq!(spool.pending_count(), 0);
        assert!(spool.claim(&key).expect("re-claim").is_none());

        spool.complete(&key).expect("complete");
        assert_eq!(spool.pending_count(), 0);
        assert!(spool.list_pending().expect("list").is_empty());
    }

    #[test]
    fn claim_missing_key_returns_none() {
        let (_dir, spool) = test_spool();
        assert!(
            spool
                .claim("0000000000000-00000000-0000-0000-0000-000000000000")
                .expect("claim")
                .is_none()
        );
    }

    #[test]
    fn transient_failure_keeps_record_with_backoff() {
        let (_dir, spool) = test_spool();
        let key = spool.persist(&test_message("retry me")).expect("persist");
        let claimed = spool.claim(&key).expect("claim").expect("present");

        spool
            .fail_transient(&claimed, "connection reset")
            .expect("fail");

        // Backdated backoff has not elapsed: not listed yet.
        assert!(spool.list_pending().expect("list").is_empty());
        assert_eq!(spool.pending_count(), 1);

        let data = fs::read(spool.pending_path(&key)).expect("record still pending");
        let record: SpoolRecord = serde_json::from_slice(&data).expect("parse");
        assert_eq!(record.attempts, 1);
        assert_eq!(record.last_error.as_deref(), Some("connection reset"));
        assert!(record.next_attempt_after_unix_secs > unix_secs());
        // The claim file is gone after resolution.
        assert!(!spool.claim_path(&key).exists());
    }

    #[test]
    fn exhausted_attempts_park_record_as_failed() {
        let (_dir, spool) = test_spool();
        let key = spool.persist(&test_message("doomed")).expect("persist");

        for attempt in 1..=SPOOL_MAX_ATTEMPTS {
            let claimed = spool.claim(&key).expect("claim").expect("present");
            assert_eq!(claimed.record.attempts, attempt - 1);
            spool.fail_transient(&claimed, "still down").expect("fail");
            // Force the backoff to be due for the next iteration.
            if attempt < SPOOL_MAX_ATTEMPTS {
                let path = spool.pending_path(&key);
                let data = fs::read(&path).expect("read record");
                let mut record: SpoolRecord = serde_json::from_slice(&data).expect("parse");
                record.next_attempt_after_unix_secs = 0;
                fs::write(&path, serde_json::to_vec(&record).expect("encode"))
                    .expect("rewrite record");
            }
        }

        assert_eq!(spool.pending_count(), 0);
        assert!(spool.failed_path(&key).exists());
        assert!(spool.list_pending().expect("list").is_empty());
        assert!(spool.claim(&key).expect("claim").is_none());
    }

    #[test]
    fn permanent_failure_parks_record_immediately() {
        let (_dir, spool) = test_spool();
        let key = spool.persist(&test_message("bad jid")).expect("persist");
        let claimed = spool.claim(&key).expect("claim").expect("present");

        spool.fail_permanent(&claimed, "invalid JID").expect("fail");

        assert_eq!(spool.pending_count(), 0);
        assert!(spool.failed_path(&key).exists());
        let data = fs::read(spool.failed_path(&key)).expect("failed record");
        let record: SpoolRecord = serde_json::from_slice(&data).expect("parse");
        assert_eq!(record.last_error.as_deref(), Some("invalid JID"));
        assert!(spool.list_pending().expect("list").is_empty());
    }

    #[test]
    fn corrupt_records_are_quarantined_not_dropped() {
        let (_dir, spool) = test_spool();
        let key = spool.persist(&test_message("good")).expect("persist");
        let bad_key = "0000000000001-deadbeef-dead-beef-dead-beefdeadbeef";
        fs::write(spool.pending_path(bad_key), b"not json").expect("write corrupt");

        let ready = spool.list_pending().expect("list");
        assert_eq!(ready, vec![key]);
        assert!(spool.dir().join(format!("{bad_key}.corrupt")).exists());
        // Quarantined files are not re-scanned as candidates.
        let ready = spool.list_pending().expect("list again");
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn undecodable_attachment_payload_is_quarantined() {
        let (_dir, spool) = test_spool();
        let key = spool
            .persist(&test_message("will corrupt"))
            .expect("persist");
        // Corrupt only the attachment encoding, leaving valid JSON behind.
        let path = spool.pending_path(&key);
        let data = fs::read(&path).expect("read record");
        let mut record: SpoolRecord = serde_json::from_slice(&data).expect("parse");
        record.attachments.push(SpoolAttachment {
            filename: "broken.bin".to_string(),
            mime_type: "application/octet-stream".to_string(),
            data_b64: "!!!not-base64!!!".to_string(),
        });
        fs::write(&path, serde_json::to_vec(&record).expect("encode")).expect("rewrite");

        assert!(spool.list_pending().expect("list").is_empty());
        assert!(spool.claim(&key).expect("claim").is_none());
        assert!(spool.dir().join(format!("{key}.corrupt")).exists());
    }

    #[test]
    fn orphaned_claim_is_recovered_on_next_scan() {
        let (_dir, spool) = test_spool();
        let key = spool
            .persist(&test_message("claimed then died"))
            .expect("persist");
        let claimed = spool.claim(&key).expect("claim").expect("present");
        drop(claimed); // process "dies" holding the claim

        let ready = spool.list_pending().expect("list");
        assert_eq!(ready, vec![key]);
    }

    #[test]
    fn pending_record_wins_over_stale_claim_duplicate() {
        let (_dir, spool) = test_spool();
        let key = spool.persist(&test_message("dup")).expect("persist");
        let claimed = spool.claim(&key).expect("claim").expect("present");
        spool.fail_transient(&claimed, "down").expect("fail");
        // Simulate a crash between the pending rewrite and the claim removal:
        // recreate the claim file alongside the pending record.
        fs::write(spool.claim_path(&key), b"{}").expect("recreate stale claim");

        let ready = spool.list_pending().expect("list");
        // Stale claim removed; pending record kept (backoff still applies, so
        // it is not ready yet — but the claim duplicate must be gone).
        assert!(ready.is_empty());
        assert!(!spool.claim_path(&key).exists());
        assert_eq!(spool.pending_count(), 1);
    }

    #[test]
    fn pending_records_list_oldest_first() {
        let (_dir, spool) = test_spool();
        // No sleep between persists: same-millisecond enqueues must still
        // come back in enqueue order via the key's sequence component.
        let mut keys = Vec::new();
        for body in ["first", "second", "third"] {
            keys.push(spool.persist(&test_message(body)).expect("persist"));
        }
        assert_eq!(spool.list_pending().expect("list"), keys);
    }

    #[test]
    fn discard_removes_pending_record() {
        let (_dir, spool) = test_spool();
        let key = spool
            .persist(&test_message("never queued"))
            .expect("persist");
        spool.discard(&key);
        assert_eq!(spool.pending_count(), 0);
        assert!(spool.list_pending().expect("list").is_empty());
    }

    #[test]
    fn init_fails_when_dir_is_a_file() {
        let dir = TempDir::new().expect("tempdir");
        let blocker = dir.path().join("spool");
        fs::write(&blocker, b"occupied").expect("write blocker file");
        assert!(OutboundSpool::new(blocker).is_err());
    }
}
