//! Study-Notes knowledge layer types.
//!
//! Study notes are durable, verified codebase-study findings shared between
//! agents. They are stored as ordinary workspace documents under
//! `study-notes/<agent>/<topic>.md` with a `study-note` provenance tag in the
//! document metadata, so they flow through the normal workspace pipeline
//! (chunking, PostgreSQL FTS, multi-scope reads, versioning).
//!
//! Scopes:
//! - **private** — the caller's own user scope (per-tenant knowledge)
//! - **shared** — the well-known [`KNOWLEDGE_SHARED_SCOPE`] user scope
//!   ("herd knowledge") readable by every agent

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Workspace directory prefix that holds all study notes.
pub const KNOWLEDGE_DIR: &str = "study-notes";

/// Metadata provenance tag marking a workspace document as a study note.
pub const KNOWLEDGE_PROVENANCE: &str = "study-note";

/// Well-known user scope holding shared ("herd") knowledge.
pub const KNOWLEDGE_SHARED_SCOPE: &str = "shared";

/// Maximum length of a topic or agent slug (UTF-8 bytes).
pub const MAX_SLUG_BYTES: usize = 100;
/// Maximum title length (UTF-8 bytes).
pub const MAX_TITLE_BYTES: usize = 200;
/// Maximum note content length (UTF-8 bytes).
pub const MAX_CONTENT_BYTES: usize = 50_000;
/// Maximum number of tags per note.
pub const MAX_TAGS: usize = 16;
/// Maximum tag length (UTF-8 bytes).
pub const MAX_TAG_BYTES: usize = 40;
/// Maximum source-commit length (UTF-8 bytes).
pub const MAX_COMMIT_BYTES: usize = 200;
/// Maximum status-reason length (UTF-8 bytes), matching the canonical
/// lw2studymcp metadata model.
pub const MAX_STATUS_REASON_BYTES: usize = 1_000;
/// Maximum per-note staleness threshold in days, matching the canonical
/// lw2studymcp metadata model.
pub const MAX_STALE_AFTER_DAYS: u32 = 36_500;
/// Rows fetched per page by full-store knowledge metadata scans.
pub const KNOWLEDGE_SCAN_PAGE: usize = 500;
/// Hard ceiling on rows returned by a full-store knowledge metadata scan.
/// Bounds the sweep and the knowledge tools on pathologically large stores
/// while still covering two orders of magnitude more than a single bounded
/// list ever could.
pub const KNOWLEDGE_SCAN_MAX_NOTES: usize = 100_000;

/// Lifecycle status of a study note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnowledgeStatus {
    /// Verified against current source by the submitting agent.
    Active,
    /// No longer holds against current source.
    Stale,
    /// Replaced by a newer note.
    Superseded,
    /// Recorded but not yet verified against current source.
    NeedsVerification,
}

impl KnowledgeStatus {
    /// Stable string form used in metadata JSON and tool parameters.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Superseded => "superseded",
            Self::NeedsVerification => "needs-verification",
        }
    }

    /// Parse a status string, accepting the exact `as_str` forms only.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "stale" => Some(Self::Stale),
            "superseded" => Some(Self::Superseded),
            "needs-verification" => Some(Self::NeedsVerification),
            _ => None,
        }
    }

    /// Whether this status already counts as needing verification.
    pub fn is_unverified(self) -> bool {
        matches!(self, Self::Stale | Self::NeedsVerification)
    }
}

impl std::fmt::Display for KnowledgeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a workspace path belongs to the study-notes knowledge store.
pub fn is_knowledge_path(path: &str) -> bool {
    path.strip_prefix(KNOWLEDGE_DIR)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

/// Whether a lowercase slug is valid (topic, agent, tag names).
pub fn is_valid_slug(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

/// Workspace path for one agent's version of a topic.
pub fn note_path(agent: &str, topic: &str) -> String {
    format!("{KNOWLEDGE_DIR}/{agent}/{topic}.md")
}

/// Provenance and lifecycle metadata stored on a study-note document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeNoteMeta {
    /// Canonical lowercase topic slug.
    pub topic: String,
    /// Stable slug of the agent that studied this version.
    pub agent: String,
    /// Human-readable note title.
    pub title: String,
    /// Lifecycle status.
    pub status: KnowledgeStatus,
    /// Evidence-backed confidence (0.0-1.0), if known.
    pub confidence: Option<f32>,
    /// When the findings were verified against source.
    pub studied_at: Option<DateTime<Utc>>,
    /// Repository commit the findings were verified against.
    pub source_commit: Option<String>,
    /// Lowercase tag slugs.
    pub tags: Vec<String>,
    /// Topic/version superseding this note, when status is superseded.
    pub superseded_by: Option<String>,
    /// Per-note staleness threshold in days (1-36500). When set, stale
    /// detection uses it as the default age limit whenever the caller passes
    /// no explicit `max_age_days`, matching the canonical lw2studymcp store.
    #[serde(default)]
    pub stale_after_days: Option<u32>,
    /// Human-readable reason for the current lifecycle status.
    #[serde(default)]
    pub status_reason: Option<String>,
    /// User scope that actually submitted the note. Recorded from the
    /// caller's job context so shared-scope notes keep submitter attribution.
    #[serde(default)]
    pub submitted_by: Option<String>,
    /// Job or conversation session the note was submitted from, when known.
    #[serde(default)]
    pub session_id: Option<String>,
}

impl KnowledgeNoteMeta {
    /// Build the document metadata JSON, including the provenance tag.
    pub fn to_metadata_json(&self) -> serde_json::Value {
        serde_json::json!({
            "provenance": KNOWLEDGE_PROVENANCE,
            "topic": self.topic,
            "agent": self.agent,
            "title": self.title,
            "status": self.status.as_str(),
            "confidence": self.confidence,
            "studied_at": self.studied_at.map(|ts| ts.to_rfc3339()),
            "source_commit": self.source_commit,
            "tags": self.tags,
            "superseded_by": self.superseded_by,
            "stale_after_days": self.stale_after_days,
            "status_reason": self.status_reason,
            "submitted_by": self.submitted_by,
            "session_id": self.session_id,
        })
    }

    /// Parse metadata JSON back into structured form.
    ///
    /// Returns `None` when the document is not a study note (missing or
    /// mismatched provenance tag) or required fields are absent/invalid.
    pub fn from_metadata_json(metadata: &serde_json::Value) -> Option<Self> {
        if metadata.get("provenance")?.as_str()? != KNOWLEDGE_PROVENANCE {
            return None;
        }
        Some(Self {
            topic: metadata.get("topic")?.as_str()?.to_string(),
            agent: metadata.get("agent")?.as_str()?.to_string(),
            title: metadata
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            status: KnowledgeStatus::parse(metadata.get("status")?.as_str()?)?,
            confidence: metadata
                .get("confidence")
                .and_then(|v| v.as_f64())
                .map(|v| v as f32),
            studied_at: metadata
                .get("studied_at")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|ts| ts.with_timezone(&Utc)),
            source_commit: metadata
                .get("source_commit")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            tags: metadata
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            superseded_by: metadata
                .get("superseded_by")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            stale_after_days: metadata
                .get("stale_after_days")
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|days| (1..=MAX_STALE_AFTER_DAYS).contains(days)),
            status_reason: metadata
                .get("status_reason")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            submitted_by: metadata
                .get("submitted_by")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            session_id: metadata
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
    }

    /// Whether this note needs verification given age/commit criteria.
    ///
    /// A note is stale when its status is `stale`/`needs-verification`, when
    /// it was studied more than the effective age limit ago (falling back to
    /// `updated_at` when no studied timestamp is recorded), or when its
    /// source commit differs from `repository_commit`. The effective age
    /// limit is the caller's `max_age_days` when given, otherwise the note's
    /// own `stale_after_days` threshold (canonical lw2studymcp semantics).
    pub fn needs_verification(
        &self,
        updated_at: DateTime<Utc>,
        max_age_days: Option<i64>,
        repository_commit: Option<&str>,
    ) -> bool {
        if self.status.is_unverified() {
            return true;
        }
        let age_limit_days = max_age_days.or_else(|| self.stale_after_days.map(i64::from));
        if let Some(days) = age_limit_days {
            let studied = self.studied_at.unwrap_or(updated_at);
            if Utc::now() - studied > chrono::Duration::days(days) {
                return true;
            }
        }
        if let Some(commit) = repository_commit
            && self.source_commit.as_deref() != Some(commit)
        {
            return true;
        }
        false
    }
}

/// One knowledge-store document returned by metadata scans (no content).
#[derive(Debug, Clone)]
pub struct KnowledgeNoteRow {
    /// User scope that owns the document.
    pub scope: String,
    /// Workspace path (`study-notes/<agent>/<topic>.md`).
    pub path: String,
    /// Last document update time.
    pub updated_at: DateTime<Utc>,
    /// Parsed study-note metadata.
    pub meta: KnowledgeNoteMeta,
}

/// A ranked full-text search hit against study-note chunks.
#[derive(Debug, Clone)]
pub struct KnowledgeSearchHit {
    /// User scope that owns the matching document.
    pub scope: String,
    /// Workspace path of the matching document.
    pub path: String,
    /// Best matching chunk text.
    pub snippet: String,
    /// PostgreSQL `ts_rank_cd` rank of the snippet.
    pub rank: f32,
    /// Parsed study-note metadata.
    pub meta: KnowledgeNoteMeta,
}

/// Optional filters for knowledge full-text search.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeFilter {
    /// Restrict to one canonical topic.
    pub topic: Option<String>,
    /// Restrict to one submitting agent.
    pub agent: Option<String>,
    /// Restrict to one lifecycle status.
    pub status: Option<KnowledgeStatus>,
    /// Require every listed tag (canonical lw2studymcp semantics).
    pub tags: Vec<String>,
    /// Require at least this confidence (0.0-1.0). Notes without a recorded
    /// confidence are excluded while the filter is set.
    pub min_confidence: Option<f32>,
}

/// Aggregate knowledge-store statistics across the searched scopes.
#[derive(Debug, Clone, Default, Serialize)]
pub struct KnowledgeStats {
    /// Scopes included in the scan.
    pub scopes: Vec<String>,
    /// Total study notes.
    pub total_notes: u64,
    /// Distinct canonical topics.
    pub topics: u64,
    /// Notes per lifecycle status.
    pub active: u64,
    /// Stale notes.
    pub stale: u64,
    /// Superseded notes.
    pub superseded: u64,
    /// Notes awaiting verification.
    pub needs_verification: u64,
    /// Notes ingested from coordination rooms (untrusted wire claims).
    pub coordination: u64,
    /// Notes per scope.
    pub per_scope: Vec<KnowledgeScopeCount>,
}

/// Note count for one scope.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeScopeCount {
    /// User scope.
    pub scope: String,
    /// Number of study notes in that scope.
    pub notes: u64,
}

impl KnowledgeStats {
    /// Compute statistics from a metadata scan.
    pub fn from_rows(scopes: &[String], rows: &[KnowledgeNoteRow]) -> Self {
        let mut stats = Self {
            scopes: scopes.to_vec(),
            ..Self::default()
        };
        let mut topics = std::collections::HashSet::new();
        let mut per_scope: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
        for row in rows {
            stats.total_notes += 1;
            topics.insert(row.meta.topic.as_str());
            match row.meta.status {
                KnowledgeStatus::Active => stats.active += 1,
                KnowledgeStatus::Stale => stats.stale += 1,
                KnowledgeStatus::Superseded => stats.superseded += 1,
                KnowledgeStatus::NeedsVerification => stats.needs_verification += 1,
            }
            if row.meta.tags.iter().any(|tag| tag == "coordination") {
                stats.coordination += 1;
            }
            *per_scope.entry(row.scope.as_str()).or_default() += 1;
        }
        stats.topics = topics.len() as u64;
        let mut per_scope: Vec<KnowledgeScopeCount> = per_scope
            .into_iter()
            .map(|(scope, notes)| KnowledgeScopeCount {
                scope: scope.to_string(),
                notes,
            })
            .collect();
        per_scope.sort_by(|a, b| a.scope.cmp(&b.scope));
        stats.per_scope = per_scope;
        stats
    }
}

/// Per-topic aggregate returned by `knowledge_list_topics`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnowledgeTopicSummary {
    /// Canonical topic slug.
    pub topic: String,
    /// Number of per-agent versions across the scanned scopes.
    pub versions: u64,
    /// Distinct scopes holding a version of this topic.
    pub scopes: Vec<String>,
    /// Distinct submitting agents.
    pub agents: Vec<String>,
    /// Distinct lifecycle statuses across versions.
    pub statuses: Vec<String>,
    /// Distinct tags across versions.
    pub tags: Vec<String>,
}

fn push_unique(target: &mut Vec<String>, value: &str) {
    if !target.iter().any(|existing| existing == value) {
        target.push(value.to_string());
    }
}

/// Aggregate scanned note rows into per-topic summaries sorted by topic slug.
///
/// `filter` is a substring match on the topic slug; since slugs are always
/// lowercase, lowercasing the needle reproduces the canonical store's
/// case-insensitive `LIKE` semantics.
pub fn summarize_topics(
    rows: &[KnowledgeNoteRow],
    filter: Option<&str>,
) -> Vec<KnowledgeTopicSummary> {
    let needle = filter
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let mut by_topic: std::collections::BTreeMap<&str, KnowledgeTopicSummary> =
        std::collections::BTreeMap::new();
    for row in rows {
        let topic = row.meta.topic.as_str();
        if needle
            .as_ref()
            .is_some_and(|needle| !topic.contains(needle))
        {
            continue;
        }
        let summary = by_topic
            .entry(topic)
            .or_insert_with(|| KnowledgeTopicSummary {
                topic: topic.to_string(),
                versions: 0,
                scopes: Vec::new(),
                agents: Vec::new(),
                statuses: Vec::new(),
                tags: Vec::new(),
            });
        summary.versions += 1;
        push_unique(&mut summary.scopes, &row.scope);
        push_unique(&mut summary.agents, &row.meta.agent);
        push_unique(&mut summary.statuses, row.meta.status.as_str());
        for tag in &row.meta.tags {
            push_unique(&mut summary.tags, tag);
        }
    }
    by_topic.into_values().collect()
}

/// Render the canonical Markdown body for a study note.
pub fn render_note_markdown(title: &str, content: &str) -> String {
    format!("# {title}\n\n{}\n", content.trim_end())
}

/// Agent-slug namespace prefix reserved for wire-ingested coordination notes.
///
/// Wire notes live under `study-notes/{WIRE_AGENT_PREFIX}-<sender>-<hash>/`,
/// so they can never overwrite locally authored agent versions.
pub const WIRE_AGENT_PREFIX: &str = "coord";

/// Machine-checkable sentinel marking a coordination wire-ingested note body.
///
/// The cross-tenant knowledge-share pipeline
/// (`crate::hooks::coordination_knowledge`) commits note content before its
/// provenance metadata; a crash or shutdown between the two commits strands a
/// body with no metadata. The sentinel is part of the body from the first
/// content commit, so stranded writes are detectable by exact match instead
/// of a title heuristic. It is an HTML comment: invisible in rendered
/// Markdown and inert to readers.
pub const WIRE_NOTE_SENTINEL: &str = "<!-- lunarwing:coordination-wire-note -->";

/// Render the canonical Markdown body for a coordination wire note.
///
/// Identical to [`render_note_markdown`] with the [`WIRE_NOTE_SENTINEL`]
/// marker line prepended, so every wire note — including one stranded before
/// its metadata commit — carries the hard marker.
pub fn render_wire_note_markdown(title: &str, content: &str) -> String {
    format!(
        "{WIRE_NOTE_SENTINEL}\n\n{}",
        render_note_markdown(title, content)
    )
}

/// Detect a note stranded between the content and metadata commits of a
/// coordination ingest: study-note provenance is absent, but the body carries
/// the [`WIRE_NOTE_SENTINEL`] hard marker. Notes stranded before the sentinel
/// existed are still recognized by the legacy `KNOW:` title prefix, which
/// only that pipeline writes.
pub fn is_incomplete_wire_write(metadata: &serde_json::Value, content: &str) -> bool {
    KnowledgeNoteMeta::from_metadata_json(metadata).is_none()
        && (content.starts_with(WIRE_NOTE_SENTINEL) || content.starts_with("# KNOW: "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta() -> KnowledgeNoteMeta {
        KnowledgeNoteMeta {
            topic: "memory-search".to_string(),
            agent: "kimi-1".to_string(),
            title: "Hybrid memory search".to_string(),
            status: KnowledgeStatus::Active,
            confidence: Some(0.9),
            studied_at: DateTime::parse_from_rfc3339("2026-08-06T10:00:00Z")
                .ok()
                .map(|ts| ts.with_timezone(&Utc)),
            source_commit: Some("abc123".to_string()),
            tags: vec!["memory".to_string(), "search".to_string()],
            superseded_by: None,
            stale_after_days: None,
            status_reason: None,
            submitted_by: Some("alice".to_string()),
            session_id: Some("session-1".to_string()),
        }
    }

    #[test]
    fn status_roundtrip() {
        for status in [
            KnowledgeStatus::Active,
            KnowledgeStatus::Stale,
            KnowledgeStatus::Superseded,
            KnowledgeStatus::NeedsVerification,
        ] {
            assert_eq!(KnowledgeStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(KnowledgeStatus::parse("bogus"), None);
    }

    #[test]
    fn knowledge_path_detection() {
        assert!(is_knowledge_path("study-notes/kimi-1/memory-search.md"));
        assert!(!is_knowledge_path("study-notes"));
        assert!(!is_knowledge_path("MEMORY.md"));
        assert!(!is_knowledge_path("projects/study-notes/x.md"));
    }

    #[test]
    fn slug_validation() {
        assert!(is_valid_slug("memory-search", MAX_SLUG_BYTES));
        assert!(is_valid_slug("a1", MAX_SLUG_BYTES));
        assert!(!is_valid_slug("", MAX_SLUG_BYTES));
        assert!(!is_valid_slug("Memory", MAX_SLUG_BYTES));
        assert!(!is_valid_slug("-lead", MAX_SLUG_BYTES));
        assert!(!is_valid_slug("trail-", MAX_SLUG_BYTES));
        assert!(!is_valid_slug("has space", MAX_SLUG_BYTES));
        assert!(!is_valid_slug("slash/x", MAX_SLUG_BYTES));
        assert!(!is_valid_slug("x".repeat(101).as_str(), MAX_SLUG_BYTES));
    }

    #[test]
    fn metadata_json_roundtrip() {
        let meta = sample_meta();
        let json = meta.to_metadata_json();
        assert_eq!(json["provenance"], KNOWLEDGE_PROVENANCE);
        let parsed = KnowledgeNoteMeta::from_metadata_json(&json);
        assert_eq!(parsed, Some(meta));
    }

    #[test]
    fn lifecycle_metadata_fields_roundtrip() {
        let meta = KnowledgeNoteMeta {
            stale_after_days: Some(45),
            status_reason: Some("verified against abc123".to_string()),
            ..sample_meta()
        };
        let json = meta.to_metadata_json();
        assert_eq!(json["stale_after_days"], 45);
        assert_eq!(json["status_reason"], "verified against abc123");
        assert_eq!(
            KnowledgeNoteMeta::from_metadata_json(&json),
            Some(meta.clone())
        );
        // Out-of-range thresholds are dropped on parse, never trusted.
        let mut bad = meta.to_metadata_json();
        bad["stale_after_days"] = serde_json::json!(0);
        let parsed = KnowledgeNoteMeta::from_metadata_json(&bad).expect("parses");
        assert_eq!(parsed.stale_after_days, None);
        bad["stale_after_days"] = serde_json::json!(MAX_STALE_AFTER_DAYS + 1);
        let parsed = KnowledgeNoteMeta::from_metadata_json(&bad).expect("parses");
        assert_eq!(parsed.stale_after_days, None);
    }

    #[test]
    fn needs_verification_honors_per_note_stale_after_days() {
        let now = Utc::now();
        let meta = KnowledgeNoteMeta {
            stale_after_days: Some(30),
            studied_at: Some(now - chrono::Duration::days(45)),
            ..sample_meta()
        };
        // The note's own threshold applies when the caller passes no limit.
        assert!(meta.needs_verification(now, None, None));
        // A more generous caller limit overrides the note threshold.
        assert!(!meta.needs_verification(now, Some(60), None));
        // A stricter caller limit also wins.
        assert!(meta.needs_verification(now, Some(10), None));
        // Notes without the field keep the no-default-age-check behavior.
        let plain = KnowledgeNoteMeta {
            studied_at: Some(now - chrono::Duration::days(45)),
            ..sample_meta()
        };
        assert!(!plain.needs_verification(now, None, None));
    }

    #[test]
    fn summarize_topics_aggregates_versions() {
        let now = Utc::now();
        let rows = vec![
            KnowledgeNoteRow {
                scope: "alice".to_string(),
                path: note_path("kimi-1", "memory-search"),
                updated_at: now,
                meta: sample_meta(),
            },
            KnowledgeNoteRow {
                scope: "shared".to_string(),
                path: note_path("kimi-2", "memory-search"),
                updated_at: now,
                meta: KnowledgeNoteMeta {
                    agent: "kimi-2".to_string(),
                    status: KnowledgeStatus::NeedsVerification,
                    tags: vec!["vectors".to_string()],
                    ..sample_meta()
                },
            },
            KnowledgeNoteRow {
                scope: "shared".to_string(),
                path: note_path("kimi-1", "approvals"),
                updated_at: now,
                meta: KnowledgeNoteMeta {
                    topic: "approvals".to_string(),
                    status: KnowledgeStatus::Stale,
                    tags: vec![],
                    ..sample_meta()
                },
            },
        ];
        let summaries = summarize_topics(&rows, None);
        assert_eq!(summaries.len(), 2);
        // Sorted by topic slug: approvals before memory-search.
        assert_eq!(summaries[0].topic, "approvals");
        assert_eq!(summaries[0].versions, 1);
        assert_eq!(summaries[0].statuses, vec!["stale".to_string()]);
        let memory = &summaries[1];
        assert_eq!(memory.topic, "memory-search");
        assert_eq!(memory.versions, 2);
        assert_eq!(
            memory.scopes,
            vec!["alice".to_string(), "shared".to_string()]
        );
        assert_eq!(
            memory.agents,
            vec!["kimi-1".to_string(), "kimi-2".to_string()]
        );
        assert_eq!(
            memory.statuses,
            vec!["active".to_string(), "needs-verification".to_string()]
        );
        assert_eq!(
            memory.tags,
            vec![
                "memory".to_string(),
                "search".to_string(),
                "vectors".to_string()
            ]
        );
        // Substring filter narrows the set (case-insensitive needle).
        let filtered = summarize_topics(&rows, Some("MEMORY"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].topic, "memory-search");
        assert!(summarize_topics(&rows, Some("missing")).is_empty());
        // Blank filters behave like no filter.
        assert_eq!(summarize_topics(&rows, Some("  ")).len(), 2);
    }

    #[test]
    fn provenance_fields_default_for_legacy_notes() {
        // Notes written before Phase 2 have no submitted_by/session_id keys;
        // they must still parse with None provenance extras.
        let json = serde_json::json!({
            "provenance": KNOWLEDGE_PROVENANCE,
            "topic": "memory-search",
            "agent": "kimi-1",
            "title": "Hybrid memory search",
            "status": "active",
            "confidence": 0.9,
            "studied_at": "2026-08-06T10:00:00Z",
            "source_commit": "abc123",
            "tags": ["memory"],
            "superseded_by": null,
        });
        let parsed = KnowledgeNoteMeta::from_metadata_json(&json).expect("legacy note parses");
        assert_eq!(parsed.submitted_by, None);
        assert_eq!(parsed.session_id, None);
    }

    #[test]
    fn from_metadata_rejects_non_study_note() {
        assert_eq!(
            KnowledgeNoteMeta::from_metadata_json(&serde_json::json!({})),
            None
        );
        assert_eq!(
            KnowledgeNoteMeta::from_metadata_json(&serde_json::json!({
                "provenance": "user-note",
                "topic": "x",
                "agent": "y",
                "status": "active"
            })),
            None
        );
    }

    #[test]
    fn needs_verification_rules() {
        let mut meta = sample_meta();
        let now = Utc::now();
        meta.studied_at = Some(now);
        // Active, fresh, matching commit.
        assert!(!meta.needs_verification(now, Some(30), Some("abc123")));
        // Status alone triggers.
        meta.status = KnowledgeStatus::NeedsVerification;
        assert!(meta.needs_verification(now, None, None));
        meta.status = KnowledgeStatus::Active;
        // Commit mismatch triggers.
        assert!(meta.needs_verification(now, None, Some("other")));
        // Missing commit triggers when a commit is provided.
        meta.source_commit = None;
        assert!(meta.needs_verification(now, None, Some("abc123")));
        // Age triggers (studied_at far in the past).
        meta.studied_at = Some(now - chrono::Duration::days(90));
        assert!(meta.needs_verification(now, Some(30), None));
        assert!(!meta.needs_verification(now, Some(120), None));
        // No studied_at falls back to updated_at.
        meta.studied_at = None;
        assert!(meta.needs_verification(now - chrono::Duration::days(90), Some(30), None));
    }

    #[test]
    fn stats_aggregation() {
        let rows = vec![
            KnowledgeNoteRow {
                scope: "alice".to_string(),
                path: note_path("kimi-1", "a"),
                updated_at: Utc::now(),
                meta: sample_meta(),
            },
            KnowledgeNoteRow {
                scope: "shared".to_string(),
                path: note_path("kimi-2", "a"),
                updated_at: Utc::now(),
                meta: KnowledgeNoteMeta {
                    agent: "kimi-2".to_string(),
                    status: KnowledgeStatus::Stale,
                    ..sample_meta()
                },
            },
            KnowledgeNoteRow {
                scope: "shared".to_string(),
                path: note_path("kimi-1", "b"),
                updated_at: Utc::now(),
                meta: KnowledgeNoteMeta {
                    topic: "b".to_string(),
                    status: KnowledgeStatus::NeedsVerification,
                    tags: vec!["coordination".to_string()],
                    ..sample_meta()
                },
            },
        ];
        let stats = KnowledgeStats::from_rows(&["alice".to_string(), "shared".to_string()], &rows);
        assert_eq!(stats.total_notes, 3);
        assert_eq!(stats.topics, 2);
        assert_eq!(stats.active, 1);
        assert_eq!(stats.stale, 1);
        assert_eq!(stats.needs_verification, 1);
        assert_eq!(stats.superseded, 0);
        assert_eq!(stats.coordination, 1);
        assert_eq!(stats.per_scope.len(), 2);
        assert_eq!(stats.per_scope[0].scope, "alice");
        assert_eq!(stats.per_scope[0].notes, 1);
        assert_eq!(stats.per_scope[1].scope, "shared");
        assert_eq!(stats.per_scope[1].notes, 2);
    }

    #[test]
    fn render_markdown_has_title_and_body() {
        let md = render_note_markdown("My Title", "body text\n\n");
        assert_eq!(md, "# My Title\n\nbody text\n");
    }

    #[test]
    fn wire_note_markdown_carries_the_sentinel() {
        let md = render_wire_note_markdown("KNOW: finding", "finding body\n");
        assert!(md.starts_with(WIRE_NOTE_SENTINEL));
        assert!(md.contains("# KNOW: finding\n\nfinding body\n"));
    }

    #[test]
    fn incomplete_wire_write_detection() {
        let no_metadata = serde_json::json!({});
        // Hard marker: sentinel body without provenance metadata.
        let stranded = render_wire_note_markdown("KNOW: x", "stranded body");
        assert!(is_incomplete_wire_write(&no_metadata, &stranded));
        // Legacy marker: pre-sentinel notes used the KNOW title prefix only.
        let legacy = render_note_markdown("KNOW: x", "stranded body");
        assert!(is_incomplete_wire_write(&no_metadata, &legacy));
        // A complete wire note (metadata committed) is not stranded.
        let meta = sample_meta().to_metadata_json();
        assert!(!is_incomplete_wire_write(&meta, &stranded));
        // A native note body never matches, with or without metadata.
        let native = render_note_markdown("Hybrid memory search", "body");
        assert!(!is_incomplete_wire_write(&no_metadata, &native));
        assert!(!is_incomplete_wire_write(&meta, &native));
    }
}
