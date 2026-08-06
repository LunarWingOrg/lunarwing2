//! Study-Notes knowledge tools.
//!
//! Native implementation of the shared study-notes knowledge layer. Notes are
//! workspace documents under `study-notes/<agent>/<topic>.md` with a
//! `study-note` provenance tag, backed by PostgreSQL FTS (see
//! `workspace::knowledge`). Every tool reads across the caller's private scope
//! plus the shared herd scope unless narrowed with the `scope` parameter.

use std::sync::Arc;

use async_trait::async_trait;

use crate::context::JobContext;
use crate::tools::builtin::memory::WorkspaceResolver;
use crate::tools::tool::{Tool, ToolError, ToolOutput, require_str};
use crate::workspace::{
    KNOWLEDGE_SCAN_CAP, KNOWLEDGE_SHARED_SCOPE, KnowledgeFilter, KnowledgeNoteMeta,
    KnowledgeNoteRow, KnowledgeStats, KnowledgeStatus, MAX_COMMIT_BYTES, MAX_CONTENT_BYTES,
    MAX_SLUG_BYTES, MAX_TAG_BYTES, MAX_TAGS, MAX_TITLE_BYTES, is_valid_slug, note_path,
    render_note_markdown,
};

const MAX_KNOWLEDGE_QUERY_BYTES: usize = 1_024;
const MAX_KNOWLEDGE_OUTPUT_BYTES: usize = 8_000;
const MAX_SNIPPET_BYTES: usize = 800;
const MAX_VERSION_CONTENT_BYTES: usize = 3_000;
const DEFAULT_SEARCH_LIMIT: u64 = 10;
const MAX_SEARCH_LIMIT: u64 = 25;
const DEFAULT_STALE_LIMIT: u64 = 25;
const MAX_STALE_LIMIT: u64 = 50;

// ── Parameter helpers ──────────────────────────────────────────────

fn optional_string(params: &serde_json::Value, key: &str) -> Result<Option<String>, ToolError> {
    match params.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ToolError::InvalidParameters(format!(
            "{key} must be a string"
        ))),
    }
}

fn required_slug(params: &serde_json::Value, key: &str) -> Result<String, ToolError> {
    let value = require_str(params, key)?;
    if !is_valid_slug(value, MAX_SLUG_BYTES) {
        return Err(ToolError::InvalidParameters(format!(
            "{key} must be a lowercase slug (a-z, 0-9, dashes; max {MAX_SLUG_BYTES} bytes)"
        )));
    }
    Ok(value.to_string())
}

fn optional_slug(params: &serde_json::Value, key: &str) -> Result<Option<String>, ToolError> {
    match optional_string(params, key)? {
        None => Ok(None),
        Some(value) if is_valid_slug(&value, MAX_SLUG_BYTES) => Ok(Some(value)),
        Some(_) => Err(ToolError::InvalidParameters(format!(
            "{key} must be a lowercase slug (a-z, 0-9, dashes; max {MAX_SLUG_BYTES} bytes)"
        ))),
    }
}

fn optional_status(params: &serde_json::Value) -> Result<Option<KnowledgeStatus>, ToolError> {
    match optional_string(params, "status")? {
        None => Ok(None),
        Some(value) => KnowledgeStatus::parse(&value).map(Some).ok_or_else(|| {
            ToolError::InvalidParameters(
                "status must be one of active, stale, superseded, needs-verification".to_string(),
            )
        }),
    }
}

fn optional_tags(params: &serde_json::Value) -> Result<Vec<String>, ToolError> {
    let Some(value) = params.get("tags") else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or_else(|| {
        ToolError::InvalidParameters("tags must be an array of strings".to_string())
    })?;
    if array.len() > MAX_TAGS {
        return Err(ToolError::InvalidParameters(format!(
            "tags is limited to {MAX_TAGS} entries"
        )));
    }
    let mut tags = Vec::with_capacity(array.len());
    for item in array {
        let tag = item.as_str().ok_or_else(|| {
            ToolError::InvalidParameters("tags must be an array of strings".to_string())
        })?;
        if !is_valid_slug(tag, MAX_TAG_BYTES) {
            return Err(ToolError::InvalidParameters(format!(
                "tag '{tag}' must be a lowercase slug (max {MAX_TAG_BYTES} bytes)"
            )));
        }
        tags.push(tag.to_string());
    }
    Ok(tags)
}

fn search_limit(params: &serde_json::Value) -> usize {
    params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT) as usize
}

fn stale_limit(params: &serde_json::Value) -> usize {
    params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_STALE_LIMIT)
        .clamp(1, MAX_STALE_LIMIT) as usize
}

/// Resolve the read scope list: private + shared herd by default.
fn read_scopes(user_id: &str, scope: Option<&str>) -> Result<Vec<String>, ToolError> {
    match scope.unwrap_or("all") {
        "all" => {
            if user_id == KNOWLEDGE_SHARED_SCOPE {
                Ok(vec![user_id.to_string()])
            } else {
                Ok(vec![
                    user_id.to_string(),
                    KNOWLEDGE_SHARED_SCOPE.to_string(),
                ])
            }
        }
        "private" => Ok(vec![user_id.to_string()]),
        "shared" => Ok(vec![KNOWLEDGE_SHARED_SCOPE.to_string()]),
        other => Err(ToolError::InvalidParameters(format!(
            "scope must be one of all, private, shared (got '{other}')"
        ))),
    }
}

/// Resolve the write scope: private (caller) or shared herd.
fn write_scope(user_id: &str, scope: Option<&str>) -> Result<String, ToolError> {
    match scope.unwrap_or("private") {
        "private" => Ok(user_id.to_string()),
        "shared" => Ok(KNOWLEDGE_SHARED_SCOPE.to_string()),
        other => Err(ToolError::InvalidParameters(format!(
            "scope must be one of private, shared (got '{other}')"
        ))),
    }
}

/// Truncate on a UTF-8 boundary with an explicit marker.
fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = 0;
    for (index, character) in value.char_indices() {
        let next = index + character.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    format!("{}...[truncated]", &value[..end])
}

fn pretty_json_len(value: &serde_json::Value) -> usize {
    serde_json::to_vec_pretty(value).map_or(usize::MAX, |encoded| encoded.len())
}

fn meta_json(meta: &KnowledgeNoteMeta) -> serde_json::Value {
    serde_json::json!({
        "topic": meta.topic,
        "agent": meta.agent,
        "title": meta.title,
        "status": meta.status.as_str(),
        "confidence": meta.confidence,
        "studied_at": meta.studied_at.map(|ts| ts.to_rfc3339()),
        "source_commit": meta.source_commit,
        "tags": meta.tags,
        "superseded_by": meta.superseded_by,
    })
}

fn note_row_json(row: &KnowledgeNoteRow) -> serde_json::Value {
    let mut value = meta_json(&row.meta);
    value["scope"] = serde_json::Value::String(row.scope.clone());
    value["path"] = serde_json::Value::String(row.path.clone());
    value["updated_at"] = serde_json::Value::String(row.updated_at.to_rfc3339());
    value
}

/// Bound an array-valued output field to the overall output budget by
/// dropping trailing entries.
fn bound_results(
    mut output: serde_json::Value,
    field: &str,
) -> (serde_json::Value, bool) {
    let mut truncated = false;
    while pretty_json_len(&output) > MAX_KNOWLEDGE_OUTPUT_BYTES {
        let Some(results) = output.get_mut(field).and_then(|v| v.as_array_mut()) else {
            break;
        };
        if results.pop().is_none() {
            break;
        }
        truncated = true;
    }
    (output, truncated)
}

// ── knowledge_search ───────────────────────────────────────────────

/// Ranked PostgreSQL FTS over shared study notes.
pub struct KnowledgeSearchTool {
    resolver: Arc<dyn WorkspaceResolver>,
}

impl KnowledgeSearchTool {
    /// Create a new knowledge search tool with a workspace resolver.
    pub fn new(resolver: Arc<dyn WorkspaceResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl Tool for KnowledgeSearchTool {
    fn name(&self) -> &str {
        "knowledge_search"
    }

    fn description(&self) -> &str {
        "Search shared study notes (verified codebase-study findings recorded by agents) \
         with PostgreSQL full-text search. Use as the first research layer before broad \
         codebase exploration. Covers your private scope and the shared herd scope unless \
         narrowed with 'scope'. Returns the best-ranked snippet per note with provenance \
         (agent, topic, status, confidence, studied timestamp, tags)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Full-text search query: concrete subsystem names, types, functions, paths, or errors. Max 1,024 UTF-8 bytes."
                },
                "topic": {
                    "type": "string",
                    "description": "Optional canonical topic slug to restrict results."
                },
                "agent": {
                    "type": "string",
                    "description": "Optional submitting-agent slug to restrict results."
                },
                "status": {
                    "type": "string",
                    "enum": ["active", "stale", "superseded", "needs-verification"],
                    "description": "Optional lifecycle status filter."
                },
                "tags": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional tag slugs; notes carrying at least one match."
                },
                "scope": {
                    "type": "string",
                    "enum": ["all", "private", "shared"],
                    "description": "Which knowledge scopes to search (default: all = private + shared herd)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum notes to return (default: 10, max: 25)",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 25
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let query = require_str(&params, "query")?;
        if query.len() > MAX_KNOWLEDGE_QUERY_BYTES {
            return Err(ToolError::InvalidParameters(format!(
                "query exceeds {MAX_KNOWLEDGE_QUERY_BYTES} UTF-8 bytes"
            )));
        }
        let filter = KnowledgeFilter {
            topic: optional_slug(&params, "topic")?,
            agent: optional_slug(&params, "agent")?,
            status: optional_status(&params)?,
            tags: optional_tags(&params)?,
        };
        let limit = search_limit(&params);
        let scopes = read_scopes(&ctx.user_id, optional_string(&params, "scope")?.as_deref())?;

        let workspace = self.resolver.resolve(&ctx.user_id).await;
        let hits = workspace
            .knowledge_search(&scopes, query, &filter, limit)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Knowledge search failed: {e}")))?;

        let results: Vec<serde_json::Value> = hits
            .iter()
            .map(|hit| {
                let mut value = meta_json(&hit.meta);
                value["scope"] = serde_json::Value::String(hit.scope.clone());
                value["path"] = serde_json::Value::String(hit.path.clone());
                value["snippet"] =
                    serde_json::Value::String(truncate_utf8(&hit.snippet, MAX_SNIPPET_BYTES));
                value["rank"] = serde_json::json!(hit.rank);
                value
            })
            .collect();
        let output = serde_json::json!({
            "query": query,
            "scopes": scopes,
            "result_count": results.len(),
            "results": results,
        });
        let (mut output, truncated) = bound_results(output, "results");
        output["truncated"] = serde_json::Value::Bool(truncated);
        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        false // Internal knowledge store, trusted content
    }
}

// ── knowledge_get_topic ────────────────────────────────────────────

/// Full versions and provenance for one canonical topic.
pub struct KnowledgeGetTopicTool {
    resolver: Arc<dyn WorkspaceResolver>,
}

impl KnowledgeGetTopicTool {
    /// Create a new get-topic tool with a workspace resolver.
    pub fn new(resolver: Arc<dyn WorkspaceResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl Tool for KnowledgeGetTopicTool {
    fn name(&self) -> &str {
        "knowledge_get_topic"
    }

    fn description(&self) -> &str {
        "Fetch every per-agent version of one canonical study-note topic with full content \
         and provenance. Compare all returned versions: different agents may record \
         complementary or conflicting findings. Current source remains the authority; \
         verify stale or conflicting claims before relying on them."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "description": "Canonical topic slug (lowercase, dashes)."
                },
                "scope": {
                    "type": "string",
                    "enum": ["all", "private", "shared"],
                    "description": "Which knowledge scopes to read (default: all)."
                }
            },
            "required": ["topic"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let topic = required_slug(&params, "topic")?;
        let scopes = read_scopes(&ctx.user_id, optional_string(&params, "scope")?.as_deref())?;

        let workspace = self.resolver.resolve(&ctx.user_id).await;
        let documents = workspace
            .knowledge_topic_versions(&scopes, &topic)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Knowledge topic failed: {e}")))?;

        let versions: Vec<serde_json::Value> = documents
            .iter()
            .filter_map(|doc| {
                let meta = KnowledgeNoteMeta::from_metadata_json(&doc.metadata)?;
                let mut value = meta_json(&meta);
                value["scope"] = serde_json::Value::String(doc.user_id.clone());
                value["path"] = serde_json::Value::String(doc.path.clone());
                value["updated_at"] = serde_json::Value::String(doc.updated_at.to_rfc3339());
                value["content"] = serde_json::Value::String(truncate_utf8(
                    &doc.content,
                    MAX_VERSION_CONTENT_BYTES,
                ));
                Some(value)
            })
            .collect();
        let output = serde_json::json!({
            "topic": topic,
            "scopes": scopes,
            "version_count": versions.len(),
            "versions": versions,
        });
        let (mut output, truncated) = bound_results(output, "versions");
        output["truncated"] = serde_json::Value::Bool(truncated);
        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

// ── knowledge_submit_note ──────────────────────────────────────────

/// Validated submission parameters.
struct SubmittedNote {
    meta: KnowledgeNoteMeta,
    content: String,
    replace: bool,
}

fn validate_submission(params: &serde_json::Value) -> Result<SubmittedNote, ToolError> {
    let title = require_str(params, "title")?;
    if title.is_empty() || title.len() > MAX_TITLE_BYTES || title.contains('\n') {
        return Err(ToolError::InvalidParameters(format!(
            "title must be a single line of at most {MAX_TITLE_BYTES} UTF-8 bytes"
        )));
    }
    let content = require_str(params, "content")?;
    if content.trim().is_empty() || content.len() > MAX_CONTENT_BYTES {
        return Err(ToolError::InvalidParameters(format!(
            "content must be non-empty and at most {MAX_CONTENT_BYTES} UTF-8 bytes"
        )));
    }
    let confidence = match params.get("confidence") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => {
            let confidence = value.as_f64().ok_or_else(|| {
                ToolError::InvalidParameters("confidence must be a number".to_string())
            })?;
            if !(0.0..=1.0).contains(&confidence) {
                return Err(ToolError::InvalidParameters(
                    "confidence must be between 0.0 and 1.0".to_string(),
                ));
            }
            Some(confidence as f32)
        }
    };
    let studied_at = match optional_string(params, "studied_at")? {
        None => None,
        Some(value) => Some(
            chrono::DateTime::parse_from_rfc3339(&value)
                .map_err(|_| {
                    ToolError::InvalidParameters(
                        "studied_at must be an RFC 3339 timestamp".to_string(),
                    )
                })?
                .with_timezone(&chrono::Utc),
        ),
    };
    let source_commit = optional_string(params, "source_commit")?;
    if let Some(ref commit) = source_commit {
        if commit.len() > MAX_COMMIT_BYTES {
            return Err(ToolError::InvalidParameters(format!(
                "source_commit exceeds {MAX_COMMIT_BYTES} UTF-8 bytes"
            )));
        }
    }
    Ok(SubmittedNote {
        meta: KnowledgeNoteMeta {
            topic: required_slug(params, "topic")?,
            agent: required_slug(params, "agent")?,
            title: title.to_string(),
            status: optional_status(params)?.unwrap_or(KnowledgeStatus::Active),
            confidence,
            studied_at,
            source_commit,
            tags: optional_tags(params)?,
            superseded_by: None,
        },
        content: content.to_string(),
        replace: params
            .get("replace")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

/// Submit a verified study note to the knowledge store.
pub struct KnowledgeSubmitNoteTool {
    resolver: Arc<dyn WorkspaceResolver>,
}

impl KnowledgeSubmitNoteTool {
    /// Create a new submit tool with a workspace resolver.
    pub fn new(resolver: Arc<dyn WorkspaceResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl Tool for KnowledgeSubmitNoteTool {
    fn name(&self) -> &str {
        "knowledge_submit_note"
    }

    fn description(&self) -> &str {
        "Record a durable, source-verified codebase-study finding as a study note in the \
         shared knowledge store (PostgreSQL FTS, survives restarts). Submit only after \
         verifying against current source; use status 'needs-verification' for unresolved \
         findings. Notes are per-agent versions of a canonical topic: resubmit with \
         replace=true to update YOUR OWN agent's version, never another agent's. Defaults \
         to your private scope; use scope='shared' for herd knowledge."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "Your stable operator-assigned agent slug (lowercase, dashes)."
                },
                "topic": {
                    "type": "string",
                    "description": "Canonical topic slug; becomes the shared topic identifier."
                },
                "title": {
                    "type": "string",
                    "description": "One-line human-readable title (max 200 bytes)."
                },
                "content": {
                    "type": "string",
                    "description": "Markdown body citing concrete evidence (path:line references, commands, commits). Max 50,000 bytes."
                },
                "status": {
                    "type": "string",
                    "enum": ["active", "stale", "superseded", "needs-verification"],
                    "description": "Lifecycle status; 'active' only for findings verified against current source in this session (default: active)."
                },
                "confidence": {
                    "type": "number",
                    "description": "Evidence-backed confidence 0.0-1.0; omit when unknown.",
                    "minimum": 0.0,
                    "maximum": 1.0
                },
                "studied_at": {
                    "type": "string",
                    "description": "RFC 3339 verification timestamp."
                },
                "source_commit": {
                    "type": "string",
                    "description": "Repository commit the findings were verified against."
                },
                "tags": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Lowercase tag slugs such as security, architecture, testing (max 16)."
                },
                "scope": {
                    "type": "string",
                    "enum": ["private", "shared"],
                    "description": "Where to store the note: your private tenant scope (default) or the shared herd scope."
                },
                "replace": {
                    "type": "boolean",
                    "description": "Set true to replace your own agent's existing version of this topic."
                }
            },
            "required": ["agent", "topic", "title", "content"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let submission = validate_submission(&params)?;
        let target_scope = write_scope(
            &ctx.user_id,
            optional_string(&params, "scope")?.as_deref(),
        )?;

        let workspace = self.resolver.resolve(&target_scope).await;
        let path = note_path(&submission.meta.agent, &submission.meta.topic);
        let exists = workspace.read_primary(&path).await.is_ok();
        if exists && !submission.replace {
            return Err(ToolError::InvalidParameters(format!(
                "note '{path}' already exists; resubmit with replace=true to update your own agent's version"
            )));
        }

        let markdown = render_note_markdown(&submission.meta.title, &submission.content);
        workspace
            .write(&path, &markdown)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Knowledge write failed: {e}")))?;
        workspace
            .set_document_metadata(&path, &submission.meta.to_metadata_json())
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Knowledge metadata failed: {e}")))?;

        let output = serde_json::json!({
            "submitted": true,
            "replaced": exists,
            "scope": target_scope,
            "path": path,
            "topic": submission.meta.topic,
            "agent": submission.meta.agent,
            "status": submission.meta.status.as_str(),
        });
        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

// ── knowledge_list_stale ───────────────────────────────────────────

/// List notes needing verification by status, age, or source commit.
pub struct KnowledgeListStaleTool {
    resolver: Arc<dyn WorkspaceResolver>,
}

impl KnowledgeListStaleTool {
    /// Create a new stale-listing tool with a workspace resolver.
    pub fn new(resolver: Arc<dyn WorkspaceResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl Tool for KnowledgeListStaleTool {
    fn name(&self) -> &str {
        "knowledge_list_stale"
    }

    fn description(&self) -> &str {
        "List study notes needing verification: status stale/needs-verification, studied \
         older than max_age_days, or recorded against a different source commit. Pass the \
         current repository commit when freshness materially affects your task."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "max_age_days": {
                    "type": "integer",
                    "description": "Flag notes studied more than this many days ago.",
                    "minimum": 1
                },
                "repository_commit": {
                    "type": "string",
                    "description": "Current repository commit; notes verified against a different commit are flagged."
                },
                "scope": {
                    "type": "string",
                    "enum": ["all", "private", "shared"],
                    "description": "Which knowledge scopes to scan (default: all)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum notes to return (default: 25, max: 50)",
                    "default": 25,
                    "minimum": 1,
                    "maximum": 50
                }
            }
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let max_age_days = params
            .get("max_age_days")
            .and_then(|v| v.as_u64())
            .map(|days| i64::try_from(days).unwrap_or(i64::MAX));
        let repository_commit = optional_string(&params, "repository_commit")?;
        let limit = stale_limit(&params);
        let scopes = read_scopes(&ctx.user_id, optional_string(&params, "scope")?.as_deref())?;

        let workspace = self.resolver.resolve(&ctx.user_id).await;
        let rows = workspace
            .knowledge_list_notes(&scopes, KNOWLEDGE_SCAN_CAP)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Knowledge scan failed: {e}")))?;

        let stale: Vec<serde_json::Value> = rows
            .iter()
            .filter(|row| {
                row.meta.needs_verification(
                    row.updated_at,
                    max_age_days,
                    repository_commit.as_deref(),
                )
            })
            .take(limit)
            .map(note_row_json)
            .collect();
        let output = serde_json::json!({
            "criteria": {
                "max_age_days": max_age_days,
                "repository_commit": repository_commit,
            },
            "scopes": scopes,
            "scanned_notes": rows.len(),
            "stale_count": stale.len(),
            "notes": stale,
        });
        let (mut output, truncated) = bound_results(output, "notes");
        output["truncated"] = serde_json::Value::Bool(truncated);
        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

// ── knowledge_stats ────────────────────────────────────────────────

/// Knowledge-store availability and lifecycle counts.
pub struct KnowledgeStatsTool {
    resolver: Arc<dyn WorkspaceResolver>,
}

impl KnowledgeStatsTool {
    /// Create a new stats tool with a workspace resolver.
    pub fn new(resolver: Arc<dyn WorkspaceResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl Tool for KnowledgeStatsTool {
    fn name(&self) -> &str {
        "knowledge_stats"
    }

    fn description(&self) -> &str {
        "Report knowledge-store statistics across your private scope and the shared herd \
         scope: total notes, distinct topics, lifecycle status counts, and per-scope counts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["all", "private", "shared"],
                    "description": "Which knowledge scopes to count (default: all)."
                }
            }
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let scopes = read_scopes(&ctx.user_id, optional_string(&params, "scope")?.as_deref())?;

        let workspace = self.resolver.resolve(&ctx.user_id).await;
        let rows = workspace
            .knowledge_list_notes(&scopes, KNOWLEDGE_SCAN_CAP)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Knowledge scan failed: {e}")))?;

        let stats = KnowledgeStats::from_rows(&scopes, &rows);
        let mut output = serde_json::to_value(&stats)
            .map_err(|e| ToolError::ExecutionFailed(format!("Knowledge stats failed: {e}")))?;
        output["scan_capped"] = serde_json::Value::Bool(rows.len() >= KNOWLEDGE_SCAN_CAP);
        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_scope_combinations() {
        assert_eq!(
            read_scopes("alice", None).unwrap(),
            vec!["alice".to_string(), KNOWLEDGE_SHARED_SCOPE.to_string()]
        );
        assert_eq!(
            read_scopes("alice", Some("private")).unwrap(),
            vec!["alice".to_string()]
        );
        assert_eq!(
            read_scopes("alice", Some("shared")).unwrap(),
            vec![KNOWLEDGE_SHARED_SCOPE.to_string()]
        );
        // The herd scope itself must not double-read.
        assert_eq!(
            read_scopes(KNOWLEDGE_SHARED_SCOPE, None).unwrap(),
            vec![KNOWLEDGE_SHARED_SCOPE.to_string()]
        );
        assert!(read_scopes("alice", Some("bogus")).is_err());
    }

    #[test]
    fn write_scope_combinations() {
        assert_eq!(write_scope("alice", None).unwrap(), "alice");
        assert_eq!(
            write_scope("alice", Some("shared")).unwrap(),
            KNOWLEDGE_SHARED_SCOPE
        );
        assert!(write_scope("alice", Some("all")).is_err());
    }

    #[test]
    fn slug_params_validate() {
        let params = serde_json::json!({"topic": "memory-search", "bad": "UPPER"});
        assert_eq!(required_slug(&params, "topic").unwrap(), "memory-search");
        assert!(required_slug(&params, "bad").is_err());
        assert!(required_slug(&params, "missing").is_err());
    }

    #[test]
    fn status_and_tags_parse() {
        let params = serde_json::json!({"status": "needs-verification", "tags": ["security"]});
        assert_eq!(
            optional_status(&params).unwrap(),
            Some(KnowledgeStatus::NeedsVerification)
        );
        assert_eq!(optional_tags(&params).unwrap(), vec!["security"]);
        let bad_status = serde_json::json!({"status": "ACTIVE"});
        assert!(optional_status(&bad_status).is_err());
        let bad_tags = serde_json::json!({"tags": ["Has Space"]});
        assert!(optional_tags(&bad_tags).is_err());
    }

    #[test]
    fn truncate_respects_boundaries() {
        assert_eq!(truncate_utf8("hello", 10), "hello");
        let truncated = truncate_utf8("hello world", 5);
        assert!(truncated.starts_with("hello"));
        assert!(truncated.ends_with("[truncated]"));
        // Multibyte characters are never split.
        let multi = truncate_utf8("a€€€€b", 5);
        assert!(multi.starts_with("a€"));
    }

    #[test]
    fn bound_results_drops_to_budget() {
        let results: Vec<serde_json::Value> = (0..100)
            .map(|i| serde_json::json!({"idx": i, "pad": "x".repeat(500)}))
            .collect();
        let output = serde_json::json!({"results": results});
        let (bounded, truncated) = bound_results(output, "results");
        assert!(truncated);
        assert!(pretty_json_len(&bounded) <= MAX_KNOWLEDGE_OUTPUT_BYTES);
    }

    #[test]
    fn submission_validation() {
        let good = serde_json::json!({
            "agent": "kimi-1", "topic": "memory-search",
            "title": "Hybrid search", "content": "Findings...",
            "confidence": 0.9, "studied_at": "2026-08-06T10:00:00Z",
            "source_commit": "abc123", "tags": ["memory"]
        });
        let parsed = validate_submission(&good).unwrap();
        assert_eq!(parsed.meta.status, KnowledgeStatus::Active);
        assert_eq!(parsed.meta.confidence, Some(0.9));
        assert!(!parsed.replace);

        for bad in [
            serde_json::json!({"agent": "BAD", "topic": "t", "title": "x", "content": "y"}),
            serde_json::json!({"agent": "a", "topic": "t", "title": "", "content": "y"}),
            serde_json::json!({"agent": "a", "topic": "t", "title": "x", "content": " "}),
            serde_json::json!({
                "agent": "a", "topic": "t", "title": "x", "content": "y",
                "confidence": 1.5
            }),
            serde_json::json!({
                "agent": "a", "topic": "t", "title": "x", "content": "y",
                "studied_at": "not-a-date"
            }),
        ] {
            assert!(validate_submission(&bad).is_err(), "should reject: {bad}");
        }
    }
}
