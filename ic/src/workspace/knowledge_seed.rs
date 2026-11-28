//! One-shot study-notes knowledge seed for newly provisioned tenants.
//!
//! Multi-tenant provisioning (`lunarwing-mt-admin.sh add-tenant
//! --with-study-mcp`) writes `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` into the new
//! tenant's env. On startup the daemon then imports every note from the
//! synced lw2studymcp checkout (`$LW2STUDYMCP_HOME/agent-notes-by-agent/
//! <agent>/<topic>.md`) into the tenant's private knowledge store, so a new
//! LunarWing starts with current knowledge of its own codebase. The import
//! is dynamic: whatever the checkout contains at first-boot time is seeded,
//! preserving the canonical lw2studymcp metadata (status, confidence,
//! studied timestamp, source commit, tags, staleness thresholds).
//!
//! Safety invariants:
//! - **New tenants only.** The env flag is written exclusively on the
//!   add-tenant path; existing tenants upgraded to a seed-capable binary
//!   never carry it and are never seeded.
//! - **Runs once.** A valid JSON state file records a completed pass and
//!   suppresses bootstrap on later startups. Per-note failures are logged
//!   and recorded without blocking the marker, which is published atomically
//!   after the pass. If no valid marker is present, the next enabled startup
//!   retries the import. An operator can force a re-seed by deleting the
//!   state file.
//! - **Never overwrites.** A note whose workspace path already exists is
//!   skipped, so operator-authored notes survive even a forced re-seed. The
//!   only writes to existing paths are repairs of a previous pass's own
//!   stranded rows: an exact byte-for-byte body missing provenance metadata
//!   gets its metadata restored, and an empty placeholder row (created just
//!   before a crash) gets filled in. Operator content never matches either
//!   shape byte-for-byte, so it is left untouched.
//! - **Wire namespace stays reserved.** Checkout directories under the
//!   coordination wire prefix (`coord-*`) are never imported, so seeded
//!   notes cannot collide with wire-ingested notes.
//!
//! Best-effort: individual note failures are logged and counted but never
//! propagated, so the agent never crashes because a seed pass failed.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::error::WorkspaceError;
use crate::workspace::{
    KnowledgeNoteMeta, KnowledgeStatus, MAX_COMMIT_BYTES, MAX_CONTENT_BYTES, MAX_SLUG_BYTES,
    MAX_STALE_AFTER_DAYS, MAX_STATUS_REASON_BYTES, MAX_TAG_BYTES, MAX_TAGS, MAX_TITLE_BYTES,
    MemoryDocument, WIRE_AGENT_PREFIX, Workspace, is_valid_slug, note_path, render_note_markdown,
};

/// Directory inside the lw2studymcp checkout holding source Markdown notes.
const NOTES_DIR: &str = "agent-notes-by-agent";

/// State file recording a completed seed pass (JSON).
const SEED_STATE_FILE: &str = "knowledge_seed_state.json";
const SEED_STATE_SCHEMA_VERSION: u32 = 1;

/// Metadata comment markers of the canonical lw2studymcp note format.
const METADATA_MARKER: &str = "<!-- study-notes-metadata";
const METADATA_CLOSE: &str = "\n-->";

/// One-shot knowledge-seed configuration.
///
/// Produced by `crate::config::KnowledgeSeedConfig::to_workspace_config`
/// only when the env flag and a source checkout are both present.
#[derive(Debug, Clone)]
pub struct KnowledgeSeedConfig {
    /// Whether the seed is enabled at all.
    pub enabled: bool,
    /// lw2studymcp checkout to import notes from.
    pub source_home: PathBuf,
    /// Directory for the seed state file (default: `~/.lunarwing`).
    pub state_dir: PathBuf,
}

/// Limits for one additive checkout-import pass.
#[derive(Debug, Clone, Copy)]
pub struct KnowledgeImportLimits {
    /// Maximum new notes written during one pass.
    pub max_new_notes: u32,
    /// Maximum source bytes represented by new notes during one pass.
    pub max_new_bytes: usize,
}

impl KnowledgeImportLimits {
    fn unlimited() -> Self {
        Self {
            max_new_notes: u32::MAX,
            max_new_bytes: usize::MAX,
        }
    }
}

/// Summary of one seed or additive refresh pass.
#[derive(Debug, Default)]
pub struct KnowledgeSeedReport {
    /// Note files discovered under `agent-notes-by-agent/`.
    pub notes_found: u32,
    /// Notes newly written into the knowledge store.
    pub notes_seeded: u32,
    /// Source bytes represented by newly written notes.
    pub bytes_seeded: usize,
    /// Notes skipped because their workspace path already existed.
    pub notes_skipped_existing: u32,
    /// Missing notes deferred by per-pass limits.
    pub notes_deferred: u32,
    /// Notes that failed to parse or write (see logs).
    pub notes_failed: u32,
    /// Whether the pass was skipped (disabled, already seeded, no checkout).
    pub skipped: bool,
}

impl KnowledgeSeedReport {
    /// True when any note was seeded.
    pub fn had_work(&self) -> bool {
        self.notes_seeded > 0
    }
}

/// Persisted proof that the seed already ran.
#[derive(Debug, Serialize, Deserialize)]
struct KnowledgeSeedState {
    #[serde(default = "legacy_seed_state_version")]
    schema_version: u32,
    seeded_at: DateTime<Utc>,
    source_home: PathBuf,
    notes_seeded: u32,
    notes_skipped_existing: u32,
    /// Notes that failed to import (logged during the pass).
    notes_failed: u32,
}

fn legacy_seed_state_version() -> u32 {
    1
}

/// One candidate note file discovered in the checkout.
#[derive(Debug)]
struct NoteFile {
    agent: String,
    topic: String,
    path: PathBuf,
}

/// Result of importing a single note.
#[derive(Debug)]
enum SeedOutcome {
    Seeded(usize),
    AlreadyExists,
    Deferred,
}

/// Run the one-shot seed when enabled and not yet completed.
///
/// Writes are plain workspace documents, so chunking/FTS/embedding behave
/// exactly as for `knowledge_submit_note`. The pass is idempotent: existing
/// notes are skipped. The state file is published once the pass has walked
/// every discovered note, even if individual notes failed. Later startups
/// skip the pass only when that file contains a valid completion marker.
pub async fn run_knowledge_seed_once(
    workspace: &Workspace,
    config: &KnowledgeSeedConfig,
) -> KnowledgeSeedReport {
    if !config.enabled {
        return KnowledgeSeedReport {
            skipped: true,
            ..Default::default()
        };
    }
    let state_file = config.state_dir.join(SEED_STATE_FILE);
    match load_state(&state_file) {
        Ok(Some(_)) => {
            tracing::debug!("knowledge seed: skipping (already seeded)");
            return KnowledgeSeedReport {
                skipped: true,
                ..Default::default()
            };
        }
        Ok(None) => {}
        Err(error) => tracing::warn!(
            path = %state_file.display(),
            %error,
            "knowledge seed: invalid or unreadable completion marker; retrying bootstrap"
        ),
    }

    let report = run_additive_knowledge_import(
        workspace,
        &config.source_home,
        KnowledgeImportLimits::unlimited(),
    )
    .await;
    if report.skipped {
        tracing::warn!(
            "knowledge seed: source notes not found under {}; will retry on next startup",
            config.source_home.display()
        );
        return report;
    }
    if let Err(error) = save_state(&state_file, config, &report) {
        tracing::warn!(
            path = %state_file.display(),
            %error,
            "knowledge seed: failed to persist completion marker"
        );
    } else if report.notes_failed > 0 {
        tracing::warn!(
            "knowledge seed: {} note(s) failed (recorded in {}); seed is complete and will not rerun",
            report.notes_failed,
            state_file.display()
        );
    }
    report
}

/// Add missing checkout notes without changing any existing workspace path.
///
/// This is shared by the first-boot seed and the periodic additive synchronizer.
/// Per-pass limits count only new writes, so repeated bounded passes eventually
/// traverse the entire source even when earlier paths already exist.
pub async fn run_additive_knowledge_import(
    workspace: &Workspace,
    source_home: &Path,
    limits: KnowledgeImportLimits,
) -> KnowledgeSeedReport {
    let notes_dir = source_home.join(NOTES_DIR);
    if !notes_dir.is_dir() {
        return KnowledgeSeedReport {
            skipped: true,
            ..Default::default()
        };
    }

    let mut report = KnowledgeSeedReport::default();
    for note in collect_note_files(&notes_dir) {
        report.notes_found = report.notes_found.saturating_add(1);
        let remaining_bytes = limits.max_new_bytes.saturating_sub(report.bytes_seeded);
        let allow_new_note = report.notes_seeded < limits.max_new_notes;
        match seed_note(workspace, &note, allow_new_note, remaining_bytes).await {
            Ok(SeedOutcome::Seeded(bytes)) => {
                report.notes_seeded = report.notes_seeded.saturating_add(1);
                report.bytes_seeded = report.bytes_seeded.saturating_add(bytes);
            }
            Ok(SeedOutcome::AlreadyExists) => {
                report.notes_skipped_existing = report.notes_skipped_existing.saturating_add(1);
            }
            Ok(SeedOutcome::Deferred) => {
                report.notes_deferred = report.notes_deferred.saturating_add(1);
            }
            Err(error) => {
                report.notes_failed = report.notes_failed.saturating_add(1);
                tracing::warn!("knowledge import: {}: {error}", note.path.display());
            }
        }
    }
    report
}

/// One checkout note parsed and validated, ready to reconcile with the store.
struct ParsedNote {
    meta: KnowledgeNoteMeta,
    markdown: String,
    source_bytes: usize,
}

/// Import one checkout note into the private knowledge store.
async fn seed_note(
    workspace: &Workspace,
    note: &NoteFile,
    allow_new_note: bool,
    remaining_bytes: usize,
) -> Result<SeedOutcome, String> {
    let raw =
        std::fs::read_to_string(&note.path).map_err(|error| format!("read failed: {error}"))?;
    let parsed = parse_note(note, &raw)?;
    let path = note_path(&note.agent, &note.topic);
    let lookup = workspace.read_primary(&path).await;
    apply_parsed_note(
        workspace,
        &path,
        &parsed,
        lookup,
        allow_new_note,
        remaining_bytes,
    )
    .await
}

/// Parse and validate one checkout note's title, body, and metadata.
fn parse_note(note: &NoteFile, raw: &str) -> Result<ParsedNote, String> {
    let source_bytes = raw.len();
    let (metadata_json, markdown) = split_note(raw)?;
    let (title, body) = extract_title(markdown, &note.topic);
    if title.len() > MAX_TITLE_BYTES {
        return Err(format!("title exceeds {MAX_TITLE_BYTES} UTF-8 bytes"));
    }
    if body.is_empty() {
        return Err("note body is empty".to_string());
    }
    if body.len() > MAX_CONTENT_BYTES {
        return Err(format!("body exceeds {MAX_CONTENT_BYTES} UTF-8 bytes"));
    }
    let meta = build_meta(&note.agent, &note.topic, &title, metadata_json.as_ref())?;
    Ok(ParsedNote {
        meta,
        markdown: render_note_markdown(&title, &body),
        source_bytes,
    })
}

/// Reconcile one parsed note with its primary-scope row.
///
/// `lookup` is the result of the primary-scope read. Only a confirmed
/// `DocumentNotFound` may take the missing-row branch: any other lookup
/// error is reported as a per-note failure so a storage fault can never be
/// mistaken for an absent note and overwrite existing content.
async fn apply_parsed_note(
    workspace: &Workspace,
    path: &str,
    parsed: &ParsedNote,
    lookup: Result<MemoryDocument, WorkspaceError>,
    allow_new_note: bool,
    remaining_bytes: usize,
) -> Result<SeedOutcome, String> {
    let existing = match lookup {
        Ok(document) => Some(document),
        Err(WorkspaceError::DocumentNotFound { .. }) => None,
        Err(error) => return Err(format!("lookup failed: {error}")),
    };
    let meta_json = parsed.meta.to_metadata_json();
    if let Some(existing) = existing {
        if KnowledgeNoteMeta::from_metadata_json(&existing.metadata).is_some() {
            return Ok(SeedOutcome::AlreadyExists);
        }
        if existing.content == parsed.markdown {
            workspace
                .set_document_metadata(path, &meta_json)
                .await
                .map_err(|error| format!("metadata repair failed: {error}"))?;
        } else if existing.content.trim().is_empty() {
            workspace
                .write(path, &parsed.markdown)
                .await
                .map_err(|error| format!("stranded-row write failed: {error}"))?;
            workspace
                .set_document_metadata(path, &meta_json)
                .await
                .map_err(|error| format!("metadata repair failed: {error}"))?;
        }
        return Ok(SeedOutcome::AlreadyExists);
    }
    if !allow_new_note || parsed.source_bytes > remaining_bytes {
        return Ok(SeedOutcome::Deferred);
    }
    workspace
        .write(path, &parsed.markdown)
        .await
        .map_err(|error| format!("write failed: {error}"))?;
    workspace
        .set_document_metadata(path, &meta_json)
        .await
        .map_err(|error| format!("metadata write failed: {error}"))?;
    Ok(SeedOutcome::Seeded(parsed.source_bytes))
}

/// Enumerate importable note files, sorted for a deterministic pass order.
///
/// Mirrors the lw2studymcp slug rules (lowercase, `_` → `-`, collapsed
/// dashes). Dotfiles, non-Markdown files, symlinks, invalid slugs, and the
/// reserved coordination wire namespace are skipped.
fn collect_note_files(notes_dir: &Path) -> Vec<NoteFile> {
    let mut notes = Vec::new();
    let wire_prefix = format!("{WIRE_AGENT_PREFIX}-");
    let Ok(agents) = std::fs::read_dir(notes_dir) else {
        return notes;
    };
    for agent_entry in agents.flatten() {
        let Ok(agent_type) = agent_entry.file_type() else {
            continue;
        };
        if !agent_type.is_dir() {
            continue;
        }
        let agent_dir = agent_entry.path();
        let Some(raw_agent) = agent_entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let agent = normalize_slug(&raw_agent);
        if raw_agent.starts_with('.')
            || agent == WIRE_AGENT_PREFIX
            || agent.starts_with(&wire_prefix)
            || !is_valid_slug(&agent, MAX_SLUG_BYTES)
        {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&agent_dir) else {
            continue;
        };
        for file_entry in files.flatten() {
            let Ok(file_type) = file_entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let file_name = file_entry.file_name();
            let Some(stem) = file_name.to_str().and_then(|name| name.strip_suffix(".md")) else {
                continue;
            };
            if stem.starts_with('.') {
                continue;
            }
            let topic = normalize_slug(stem);
            if !is_valid_slug(&topic, MAX_SLUG_BYTES) {
                continue;
            }
            notes.push(NoteFile {
                agent: agent.clone(),
                topic,
                path: file_entry.path(),
            });
        }
    }
    notes.sort_by(|a, b| a.path.cmp(&b.path));
    notes
}

/// Normalize a directory or file-stem name into a canonical slug, matching
/// lw2studymcp `topic_slug` semantics.
fn normalize_slug(raw: &str) -> String {
    let mut slug = String::with_capacity(raw.len());
    let mut last_dash = true; // trims leading dashes too
    for ch in raw.trim().chars() {
        let ch = match ch {
            '_' => '-',
            other => other.to_ascii_lowercase(),
        };
        if ch == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        slug.push(ch);
    }
    slug.trim_end_matches('-').to_string()
}

/// Split a canonical lw2studymcp note file into its metadata JSON and the
/// Markdown body. Files without a metadata comment import with defaults.
fn split_note(raw: &str) -> Result<(Option<serde_json::Value>, &str), String> {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with(METADATA_MARKER) {
        return Ok((None, raw));
    }
    let Some(open_nl) = trimmed.find('\n') else {
        return Err("study-notes-metadata comment is not followed by JSON".to_string());
    };
    let after_open = &trimmed[open_nl + 1..];
    let Some(close) = after_open.find(METADATA_CLOSE) else {
        return Err("study-notes-metadata comment is not closed".to_string());
    };
    let json_src = after_open[..close].trim();
    // The canonical pattern consumes all whitespace after the closing
    // marker, so the Markdown body starts at the first content byte.
    let markdown = after_open[close + METADATA_CLOSE.len()..].trim_start();
    let value: serde_json::Value = serde_json::from_str(json_src)
        .map_err(|error| format!("invalid study-notes-metadata JSON: {error}"))?;
    if !value.is_object() {
        return Err("study-notes-metadata must contain a JSON object".to_string());
    }
    Ok((Some(value), markdown))
}

/// Extract the note title from the first `# ` heading (removed from the
/// body), falling back to the topic slug like lw2studymcp `parse_markdown`.
fn extract_title(markdown: &str, fallback: &str) -> (String, String) {
    let mut title: Option<String> = None;
    let mut body: Vec<&str> = Vec::new();
    for line in markdown.trim().lines() {
        if title.is_none()
            && let Some(heading) = line.strip_prefix("# ")
        {
            title = Some(heading.trim().trim_end_matches('#').trim().to_string());
            continue;
        }
        body.push(line);
    }
    let title = title
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    (title, body.join("\n").trim().to_string())
}

/// Build workspace knowledge metadata from canonical lw2studymcp JSON,
/// enforcing the same limits as `knowledge_submit_note`.
fn build_meta(
    agent: &str,
    topic: &str,
    title: &str,
    json: Option<&serde_json::Value>,
) -> Result<KnowledgeNoteMeta, String> {
    let status = match json
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
    {
        None => KnowledgeStatus::Active,
        Some(raw) => KnowledgeStatus::parse(raw)
            .ok_or_else(|| format!("unknown study-note status '{raw}'"))?,
    };
    // lw2studymcp caps superseded_by at 200 bytes, same as source_commit.
    let superseded_by = bounded_str(json, "superseded_by", MAX_COMMIT_BYTES)?;
    if status == KnowledgeStatus::Superseded && superseded_by.is_none() {
        return Err("superseded notes require superseded_by".to_string());
    }
    Ok(KnowledgeNoteMeta {
        topic: topic.to_string(),
        agent: agent.to_string(),
        title: title.to_string(),
        status,
        confidence: parse_confidence(json)?,
        studied_at: parse_studied_at(json.and_then(|value| value.get("studied_at"))),
        source_commit: bounded_str(json, "source_commit", MAX_COMMIT_BYTES)?,
        tags: parse_tags(json)?,
        superseded_by,
        stale_after_days: parse_stale_after_days(json)?,
        status_reason: bounded_str(json, "status_reason", MAX_STATUS_REASON_BYTES)?,
        submitted_by: None,
        session_id: None,
    })
}

/// Read an optional string field with a byte cap.
fn bounded_str(
    json: Option<&serde_json::Value>,
    key: &str,
    max: usize,
) -> Result<Option<String>, String> {
    let Some(value) = json
        .and_then(|json| json.get(key))
        .and_then(|value| value.as_str())
    else {
        return Ok(None);
    };
    if value.len() > max {
        return Err(format!("{key} exceeds {max} UTF-8 bytes"));
    }
    Ok(Some(value.to_string()))
}

/// Parse `confidence` (0.0-1.0), rejecting out-of-range values.
fn parse_confidence(json: Option<&serde_json::Value>) -> Result<Option<f32>, String> {
    match json.and_then(|value| value.get("confidence")) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => {
            let confidence = value
                .as_f64()
                .ok_or_else(|| "confidence must be a number".to_string())?;
            if !(0.0..=1.0).contains(&confidence) {
                return Err("confidence must be between 0.0 and 1.0".to_string());
            }
            Ok(Some(confidence as f32))
        }
    }
}

/// Parse `studied_at`: RFC 3339, or the bare ISO date lw2studymcp also
/// accepts (interpreted as midnight UTC).
fn parse_studied_at(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    let raw = value?.as_str()?;
    if let Ok(ts) = DateTime::parse_from_rfc3339(raw) {
        return Some(ts.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|dt| DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
}

/// Parse `stale_after_days` (1-36500), matching canonical semantics.
fn parse_stale_after_days(json: Option<&serde_json::Value>) -> Result<Option<u32>, String> {
    match json.and_then(|value| value.get("stale_after_days")) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => {
            let days = value
                .as_u64()
                .and_then(|days| u32::try_from(days).ok())
                .ok_or_else(|| "stale_after_days must be a positive integer".to_string())?;
            if !(1..=MAX_STALE_AFTER_DAYS).contains(&days) {
                return Err(format!(
                    "stale_after_days must be from 1 to {MAX_STALE_AFTER_DAYS}"
                ));
            }
            Ok(Some(days))
        }
    }
}

/// Parse `tags`, enforcing the submit-tool caps and slug form.
fn parse_tags(json: Option<&serde_json::Value>) -> Result<Vec<String>, String> {
    let Some(values) = json.and_then(|value| value.get("tags")) else {
        return Ok(Vec::new());
    };
    if values.is_null() {
        return Ok(Vec::new());
    }
    let array = values
        .as_array()
        .ok_or_else(|| "tags must be an array".to_string())?;
    if array.len() > MAX_TAGS {
        return Err(format!("tags exceed {MAX_TAGS} entries"));
    }
    let mut tags = Vec::with_capacity(array.len());
    for value in array {
        let tag = value
            .as_str()
            .ok_or_else(|| "tags must be strings".to_string())?;
        if !is_valid_slug(tag, MAX_TAG_BYTES) {
            return Err(format!("tag '{tag}' is not a lowercase slug"));
        }
        tags.push(tag.to_string());
    }
    Ok(tags)
}

fn load_state(path: &Path) -> anyhow::Result<Option<KnowledgeSeedState>> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let state: KnowledgeSeedState = serde_json::from_slice(&raw)?;
    anyhow::ensure!(
        state.schema_version == SEED_STATE_SCHEMA_VERSION,
        "unsupported schema version {}",
        state.schema_version
    );
    Ok(Some(state))
}

fn save_state(
    path: &Path,
    config: &KnowledgeSeedConfig,
    report: &KnowledgeSeedReport,
) -> anyhow::Result<()> {
    let state = KnowledgeSeedState {
        schema_version: SEED_STATE_SCHEMA_VERSION,
        seeded_at: Utc::now(),
        source_home: config.source_home.clone(),
        notes_seeded: report.notes_seeded,
        notes_skipped_existing: report.notes_skipped_existing,
        notes_failed: report.notes_failed,
    };
    let json = serde_json::to_vec_pretty(&state).context("serialize completion marker")?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).context("create completion marker directory")?;
    let mut temp = tempfile::Builder::new()
        .prefix(&format!(".{SEED_STATE_FILE}.{}.", std::process::id()))
        .suffix(".tmp")
        .tempfile_in(parent)
        .context("create temporary completion marker")?;
    temp.write_all(&json)
        .and_then(|()| temp.as_file().sync_all())
        .context("write and sync temporary completion marker")?;
    temp.persist(path)
        .map_err(|error| error.error)
        .context("publish completion marker")?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .context("sync completion marker directory")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_note_without_marker_keeps_raw_body() {
        let (meta, body) = split_note("# Title\n\nBody.\n").expect("splits");
        assert!(meta.is_none());
        assert_eq!(body, "# Title\n\nBody.\n");
    }

    #[test]
    fn split_note_parses_metadata_comment() {
        let raw = "<!-- study-notes-metadata\n{\n  \"status\": \"stale\"\n}\n-->\n\n# T\n\nB\n";
        let (meta, body) = split_note(raw).expect("splits");
        assert_eq!(
            meta.and_then(|value| value.get("status").map(|s| s.to_string()))
                .as_deref(),
            Some("\"stale\"")
        );
        assert_eq!(body, "# T\n\nB\n");
    }

    #[test]
    fn split_note_rejects_broken_json() {
        let raw = "<!-- study-notes-metadata\n{not json}\n-->\n\n# T\n";
        assert!(split_note(raw).is_err());
    }

    #[test]
    fn split_note_rejects_unclosed_comment() {
        let raw = "<!-- study-notes-metadata\n{}\n# T\n";
        assert!(split_note(raw).is_err());
    }

    #[test]
    fn extract_title_uses_first_h1_and_strips_it() {
        let (title, body) = extract_title("# My Title\n\nLine one.\n# Later\n", "fallback");
        assert_eq!(title, "My Title");
        assert_eq!(body, "Line one.\n# Later");
    }

    #[test]
    fn extract_title_falls_back_to_topic() {
        let (title, body) = extract_title("No heading here.\n", "my-topic");
        assert_eq!(title, "my-topic");
        assert_eq!(body, "No heading here.");
    }

    #[test]
    fn studied_at_accepts_rfc3339_and_bare_dates() {
        let ts = parse_studied_at(Some(&serde_json::json!("2026-08-22T17:00:00Z")));
        assert!(ts.is_some());
        let date = parse_studied_at(Some(&serde_json::json!("2026-08-22"))).expect("date parses");
        assert_eq!(
            date.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-08-22 00:00:00"
        );
        assert!(parse_studied_at(Some(&serde_json::json!("not a date"))).is_none());
        assert!(parse_studied_at(None).is_none());
    }

    #[test]
    fn build_meta_defaults_to_active_without_metadata() {
        let meta = build_meta("rarity", "topic", "Title", None).expect("meta");
        assert_eq!(meta.status, KnowledgeStatus::Active);
        assert!(meta.confidence.is_none());
        assert!(meta.tags.is_empty());
        assert!(meta.studied_at.is_none());
    }

    #[test]
    fn build_meta_preserves_canonical_fields() {
        let json = serde_json::json!({
            "status": "needs-verification",
            "confidence": 0.9,
            "studied_at": "2026-08-22",
            "source_commit": "abc123",
            "stale_after_days": 180,
            "status_reason": "needs re-check",
            "tags": ["mt-admin", "openrc"]
        });
        let meta = build_meta("rarity", "topic", "Title", Some(&json)).expect("meta");
        assert_eq!(meta.status, KnowledgeStatus::NeedsVerification);
        assert_eq!(meta.confidence, Some(0.9));
        assert_eq!(meta.source_commit.as_deref(), Some("abc123"));
        assert_eq!(meta.stale_after_days, Some(180));
        assert_eq!(
            meta.tags,
            vec!["mt-admin".to_string(), "openrc".to_string()]
        );
    }

    #[test]
    fn build_meta_rejects_superseded_without_replacement() {
        let json = serde_json::json!({"status": "superseded"});
        assert!(build_meta("a", "t", "T", Some(&json)).is_err());
    }

    #[test]
    fn build_meta_rejects_invalid_confidence_and_tags() {
        let bad_conf = serde_json::json!({"confidence": 1.5});
        assert!(build_meta("a", "t", "T", Some(&bad_conf)).is_err());
        let bad_tag = serde_json::json!({"tags": ["Not A Slug"]});
        assert!(build_meta("a", "t", "T", Some(&bad_tag)).is_err());
    }

    #[test]
    fn normalize_slug_matches_lw2studymcp_rules() {
        assert_eq!(normalize_slug("My_Topic__Name"), "my-topic-name");
        assert_eq!(normalize_slug("--lead_trail--"), "lead-trail");
        assert_eq!(normalize_slug("plain"), "plain");
    }

    #[test]
    fn collect_note_files_skips_wire_namespace_and_non_notes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let notes = dir.path().join(NOTES_DIR);
        let write = |agent: &str, file: &str| {
            let agent_dir = notes.join(agent);
            std::fs::create_dir_all(&agent_dir).expect("agent dir");
            std::fs::write(agent_dir.join(file), "# T\n\nB\n").expect("note");
        };
        write("rarity", "real-note.md");
        write("rarity", "UPPER_Slug.md");
        write("coord-rarity-deadbeef", "wire-note.md");
        write(".hidden-agent", "hidden.md");
        write("rarity", "not-a-note.txt");
        std::fs::write(notes.join("loose-file.md"), "# T\n").expect("loose");
        #[cfg(unix)]
        {
            let outside = dir.path().join("outside.md");
            std::fs::write(&outside, "# Outside\n\nsecret\n").expect("outside");
            std::os::unix::fs::symlink(&outside, notes.join("rarity/link.md"))
                .expect("file symlink");
            let outside_agent = dir.path().join("outside-agent");
            std::fs::create_dir(&outside_agent).expect("outside agent");
            std::fs::write(outside_agent.join("linked.md"), "# Linked\n\nsecret\n")
                .expect("linked note");
            std::os::unix::fs::symlink(&outside_agent, notes.join("linked-agent"))
                .expect("directory symlink");
        }

        let found = collect_note_files(&notes);
        let mut paths: Vec<String> = found
            .iter()
            .map(|note| format!("{}/{}", note.agent, note.topic))
            .collect();
        paths.sort();
        assert_eq!(paths, vec!["rarity/real-note", "rarity/upper-slug"]);
    }

    #[cfg(feature = "libsql")]
    mod libsql_integration {
        use super::*;

        fn write_note(notes_dir: &Path, agent: &str, file: &str, content: &str) {
            let agent_dir = notes_dir.join(agent);
            std::fs::create_dir_all(&agent_dir).expect("agent dir");
            std::fs::write(agent_dir.join(file), content).expect("note");
        }

        #[tokio::test]
        async fn seeds_new_store_once_and_never_overwrites() {
            let (db, _db_dir) = crate::testing::test_db().await;
            let workspace = Workspace::new_with_db("alice", db);
            let fixture = tempfile::tempdir().expect("fixture");
            let notes_dir = fixture.path().join(NOTES_DIR);
            write_note(
                &notes_dir,
                "rarity",
                "mt-admin-lock.md",
                "<!-- study-notes-metadata\n{\n  \"confidence\": 0.9,\n  \"status\": \"active\",\n  \"studied_at\": \"2026-08-22\",\n  \"tags\": [\"mt-admin\"]\n}\n-->\n\n# mt-admin lock\n\nBody one.\n",
            );
            write_note(
                &notes_dir,
                "sweetiebot",
                "legacy-note.md",
                "# Legacy note\n\nNo metadata header.\n",
            );
            // Reserved wire namespace must never be imported.
            write_note(
                &notes_dir,
                "coord-alice-deadbeef",
                "wire.md",
                "# Wire\n\nskip me\n",
            );

            let config = KnowledgeSeedConfig {
                enabled: true,
                source_home: fixture.path().to_path_buf(),
                state_dir: fixture.path().join("state"),
            };
            let state_file = config.state_dir.join(SEED_STATE_FILE);

            let report = run_knowledge_seed_once(&workspace, &config).await;
            assert_eq!(report.notes_found, 2);
            assert_eq!(report.notes_seeded, 2);
            assert_eq!(report.notes_failed, 0);
            assert!(!report.skipped);
            assert!(state_file.is_file());

            // Metadata survives the round trip.
            let doc = workspace
                .read_primary("study-notes/rarity/mt-admin-lock.md")
                .await
                .expect("seeded doc");
            let meta = KnowledgeNoteMeta::from_metadata_json(&doc.metadata).expect("meta parses");
            assert_eq!(meta.status, KnowledgeStatus::Active);
            assert_eq!(meta.confidence, Some(0.9));
            assert_eq!(meta.tags, vec!["mt-admin".to_string()]);
            assert!(meta.studied_at.is_some());
            assert!(doc.content.contains("Body one."));
            let legacy = workspace
                .read_primary("study-notes/sweetiebot/legacy-note.md")
                .await
                .expect("legacy doc");
            assert!(legacy.content.contains("No metadata header."));

            // Second run: the marker short-circuits even when the checkout
            // gained a note — the store is never repopulated.
            write_note(
                &notes_dir,
                "rarity",
                "late-note.md",
                "# Late\n\nmust not import\n",
            );
            let report = run_knowledge_seed_once(&workspace, &config).await;
            assert!(report.skipped);
            assert!(
                workspace
                    .read_primary("study-notes/rarity/late-note.md")
                    .await
                    .is_err()
            );

            // Forced re-seed (operator deletes the marker) imports only new
            // notes and never overwrites existing ones.
            std::fs::remove_file(state_file).expect("remove marker");
            workspace
                .write(
                    "study-notes/rarity/mt-admin-lock.md",
                    "# custom\n\noperator edit\n",
                )
                .await
                .expect("operator write");
            let report = run_knowledge_seed_once(&workspace, &config).await;
            assert_eq!(report.notes_seeded, 1);
            assert_eq!(report.notes_skipped_existing, 2);
            assert_eq!(report.notes_failed, 0);
            let doc = workspace
                .read_primary("study-notes/rarity/mt-admin-lock.md")
                .await
                .expect("doc");
            assert!(doc.content.contains("operator edit"));
            assert!(
                workspace
                    .read_primary("study-notes/rarity/late-note.md")
                    .await
                    .is_ok()
            );
        }

        #[tokio::test]
        async fn retries_bootstrap_and_replaces_invalid_completion_markers() {
            let (db, _db_dir) = crate::testing::test_db().await;
            let workspace = Workspace::new_with_db("alice", db);
            let fixture = tempfile::tempdir().expect("fixture");
            let config = KnowledgeSeedConfig {
                enabled: true,
                source_home: fixture.path().to_path_buf(),
                state_dir: fixture.path().to_path_buf(),
            };
            let state_file = config.state_dir.join(SEED_STATE_FILE);
            let unsupported = serde_json::json!({
                "schema_version": SEED_STATE_SCHEMA_VERSION + 1,
                "seeded_at": "2026-08-22T17:00:00Z",
                "source_home": fixture.path(),
                "notes_seeded": 0,
                "notes_skipped_existing": 0,
                "notes_failed": 0,
            })
            .to_string();
            for (topic, marker) in [
                ("empty", ""),
                ("truncated", r#"{"seeded_at":"#),
                ("incomplete", r#"{"schema_version":1}"#),
                ("unsupported-version", unsupported.as_str()),
            ] {
                std::fs::write(&state_file, marker).expect("invalid marker fixture");
                write_note(
                    &fixture.path().join(NOTES_DIR),
                    "rarity",
                    &format!("{topic}.md"),
                    "# Fixture\n\nRecovered note.\n",
                );

                let report = run_knowledge_seed_once(&workspace, &config).await;
                assert!(!report.skipped, "{topic}");
                assert_eq!(report.notes_seeded, 1, "{topic}");
                let note = workspace
                    .read_primary(&note_path("rarity", topic))
                    .await
                    .expect("missing note was imported");
                assert!(note.content.contains("Recovered note."));
                let state = load_state(&state_file)
                    .expect("valid replacement")
                    .expect("completion marker");
                assert_eq!(state.notes_seeded, 1, "{topic}");
            }
        }

        #[tokio::test]
        async fn complete_legacy_marker_still_skips_bootstrap() {
            let (db, _db_dir) = crate::testing::test_db().await;
            let workspace = Workspace::new_with_db("alice", db);
            let fixture = tempfile::tempdir().expect("fixture");
            write_note(
                &fixture.path().join(NOTES_DIR),
                "rarity",
                "late-note.md",
                "# Late\n\nmust not import\n",
            );
            let config = KnowledgeSeedConfig {
                enabled: true,
                source_home: fixture.path().to_path_buf(),
                state_dir: fixture.path().to_path_buf(),
            };
            let legacy = serde_json::json!({
                "seeded_at": "2026-08-22T17:00:00Z",
                "source_home": "/previous/checkout",
                "notes_seeded": 42,
                "notes_skipped_existing": 3,
                "notes_failed": 1,
            });
            std::fs::write(config.state_dir.join(SEED_STATE_FILE), legacy.to_string())
                .expect("legacy marker fixture");

            let report = run_knowledge_seed_once(&workspace, &config).await;
            assert!(report.skipped);
            assert_eq!(report.notes_found, 0);
        }

        #[tokio::test]
        async fn repairs_stranded_write_but_never_touches_operator_content() {
            let (db, _db_dir) = crate::testing::test_db().await;
            let workspace = Workspace::new_with_db("alice", db);
            let fixture = tempfile::tempdir().expect("fixture");
            let notes_dir = fixture.path().join(NOTES_DIR);
            write_note(
                &notes_dir,
                "rarity",
                "stranded.md",
                "<!-- study-notes-metadata\n{\n  \"status\": \"active\",\n  \"tags\": [\"repair\"]\n}\n-->\n\n# Repair me\n\nstranded body\n",
            );
            write_note(
                &notes_dir,
                "rarity",
                "operator.md",
                "# Operator\n\nseed body\n",
            );

            // A previous pass stranded between the content and metadata
            // commits: exact seeded body present, provenance metadata absent.
            workspace
                .write(
                    "study-notes/rarity/stranded.md",
                    &render_note_markdown("Repair me", "stranded body"),
                )
                .await
                .expect("stranded write");
            // A previous pass stranded between document creation and the
            // content commit: an empty placeholder row.
            write_note(
                &notes_dir,
                "rarity",
                "placeholder.md",
                "# Placeholder\n\nplaceholder body\n",
            );
            workspace
                .write("study-notes/rarity/placeholder.md", "")
                .await
                .expect("placeholder write");

            // An operator-authored document whose content differs from the
            // checkout note must keep both its content and its metadata.
            workspace
                .write(
                    "study-notes/rarity/operator.md",
                    "# Operator\n\noperator edits\n",
                )
                .await
                .expect("operator write");

            let state_dir = tempfile::tempdir().expect("state");
            let config = KnowledgeSeedConfig {
                enabled: true,
                source_home: fixture.path().to_path_buf(),
                state_dir: state_dir.path().to_path_buf(),
            };
            let report = run_knowledge_seed_once(&workspace, &config).await;
            assert_eq!(report.notes_seeded, 0);
            assert_eq!(report.notes_skipped_existing, 3);
            assert_eq!(report.notes_failed, 0);

            let repaired = workspace
                .read_primary("study-notes/rarity/stranded.md")
                .await
                .expect("stranded doc");
            let meta =
                KnowledgeNoteMeta::from_metadata_json(&repaired.metadata).expect("repaired meta");
            assert_eq!(meta.tags, vec!["repair".to_string()]);

            let filled = workspace
                .read_primary("study-notes/rarity/placeholder.md")
                .await
                .expect("placeholder doc");
            assert!(filled.content.contains("placeholder body"));
            let meta =
                KnowledgeNoteMeta::from_metadata_json(&filled.metadata).expect("placeholder meta");
            assert_eq!(meta.topic, "placeholder");

            let operator = workspace
                .read_primary("study-notes/rarity/operator.md")
                .await
                .expect("operator doc");
            assert!(operator.content.contains("operator edits"));
            assert!(KnowledgeNoteMeta::from_metadata_json(&operator.metadata).is_none());
        }

        #[tokio::test]
        async fn additive_import_respects_limits_and_never_overwrites() {
            let (db, _db_dir) = crate::testing::test_db().await;
            let workspace = Workspace::new_with_db("alice", db);
            let fixture = tempfile::tempdir().expect("fixture");
            let notes_dir = fixture.path().join(NOTES_DIR);
            for topic in ["a.md", "b.md", "c.md"] {
                write_note(
                    &notes_dir,
                    "rarity",
                    topic,
                    &format!("# {topic}\n\nsource body\n"),
                );
            }
            let limits = KnowledgeImportLimits {
                max_new_notes: 1,
                max_new_bytes: 1_024,
            };

            let first = run_additive_knowledge_import(&workspace, fixture.path(), limits).await;
            assert_eq!(first.notes_seeded, 1);
            assert_eq!(first.notes_deferred, 2);
            workspace
                .write("study-notes/rarity/a.md", "# Local\n\noperator edit\n")
                .await
                .expect("operator edit");
            write_note(
                &notes_dir,
                "rarity",
                "a.md",
                "# Upstream changed\n\nnew source body\n",
            );

            let second = run_additive_knowledge_import(&workspace, fixture.path(), limits).await;
            assert_eq!(second.notes_seeded, 1);
            assert_eq!(second.notes_skipped_existing, 1);
            assert_eq!(second.notes_deferred, 1);
            let third = run_additive_knowledge_import(&workspace, fixture.path(), limits).await;
            assert_eq!(third.notes_seeded, 1);
            assert_eq!(third.notes_skipped_existing, 2);
            assert_eq!(third.notes_deferred, 0);

            let local = workspace
                .read_primary("study-notes/rarity/a.md")
                .await
                .expect("local note");
            assert!(local.content.contains("operator edit"));
        }

        /// A lookup that fails for a reason other than `DocumentNotFound` is
        /// a storage fault, not a missing note. Acting on it as "absent"
        /// would write checkout content over an operator's customized row,
        /// so the fault must abort the note without touching the store.
        #[tokio::test]
        async fn lookup_failure_never_overwrites_an_existing_note() {
            let (db, _db_dir) = crate::testing::test_db().await;
            let workspace = Workspace::new_with_db("alice", db);
            let fixture = tempfile::tempdir().expect("fixture");
            let notes_dir = fixture.path().join(NOTES_DIR);
            write_note(
                &notes_dir,
                "rarity",
                "customized.md",
                "# Checkout title\n\ncheckout body\n",
            );
            write_note(&notes_dir, "rarity", "brand-new.md", "# New\n\nnew body\n");

            // Operator-customized row: body differs from the checkout and it
            // carries no knowledge provenance, so a missing-row branch would
            // overwrite it rather than short-circuiting as AlreadyExists.
            let customized = note_path("rarity", "customized");
            workspace
                .write(&customized, "# Customized\n\noperator edits\n")
                .await
                .expect("operator write");

            let note = NoteFile {
                agent: "rarity".to_string(),
                topic: "customized".to_string(),
                path: notes_dir.join("rarity/customized.md"),
            };
            let raw = std::fs::read_to_string(&note.path).expect("note file");
            let parsed = parse_note(&note, &raw).expect("parsed");

            // The store itself is healthy here: the write lane would accept
            // the content. Only the injected lookup fails, exactly as a pool
            // exhaustion or query error does.
            let error = apply_parsed_note(
                &workspace,
                &customized,
                &parsed,
                Err(WorkspaceError::SearchFailed {
                    reason: "pool exhausted".to_string(),
                }),
                true,
                usize::MAX,
            )
            .await
            .expect_err("storage error must be reported");
            assert!(error.contains("lookup failed"), "{error}");

            let doc = workspace
                .read_primary(&customized)
                .await
                .expect("customized doc");
            assert!(doc.content.contains("operator edits"));
            assert!(!doc.content.contains("checkout body"));
            assert!(KnowledgeNoteMeta::from_metadata_json(&doc.metadata).is_none());

            // A confirmed miss (the real backend's DocumentNotFound) still
            // imports the note.
            let missing = note_path("rarity", "brand-new");
            let lookup = workspace.read_primary(&missing).await;
            assert!(matches!(
                lookup,
                Err(WorkspaceError::DocumentNotFound { .. })
            ));
            let brand_new = NoteFile {
                agent: "rarity".to_string(),
                topic: "brand-new".to_string(),
                path: notes_dir.join("rarity/brand-new.md"),
            };
            let raw = std::fs::read_to_string(&brand_new.path).expect("note file");
            let parsed = parse_note(&brand_new, &raw).expect("parsed");
            let outcome = apply_parsed_note(&workspace, &missing, &parsed, lookup, true, usize::MAX)
                .await
                .expect("missing note imports");
            assert!(matches!(outcome, SeedOutcome::Seeded(_)));

            let imported = workspace
                .read_primary(&missing)
                .await
                .expect("imported note");
            assert!(imported.content.contains("new body"));
            let meta = KnowledgeNoteMeta::from_metadata_json(&imported.metadata).expect("meta");
            assert_eq!(meta.topic, "brand-new");
        }

        #[tokio::test]
        async fn disabled_seed_is_a_noop() {
            let (db, _db_dir) = crate::testing::test_db().await;
            let workspace = Workspace::new_with_db("alice", db);
            let fixture = tempfile::tempdir().expect("fixture");
            write_note(fixture.path(), "rarity", "note.md", "# T\n\nB\n");
            let config = KnowledgeSeedConfig {
                enabled: false,
                source_home: fixture.path().to_path_buf(),
                state_dir: fixture.path().to_path_buf(),
            };
            let report = run_knowledge_seed_once(&workspace, &config).await;
            assert!(report.skipped);
            assert!(
                workspace
                    .read_primary("study-notes/rarity/note.md")
                    .await
                    .is_err()
            );
        }
    }
}
