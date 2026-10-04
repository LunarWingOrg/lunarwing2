//! Memory/workspace API handlers.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::Deserialize;

use crate::channels::web::auth::{AuthenticatedUser, UserIdentity};
use crate::channels::web::server::GatewayState;
use crate::channels::web::types::*;
use crate::workspace::Workspace;

/// Resolve the workspace for the authenticated user.
///
/// Prefers `workspace_pool` (multi-user mode) when available, falling back
/// to the single-user `state.workspace`.
pub(crate) async fn resolve_workspace(
    state: &GatewayState,
    user: &UserIdentity,
) -> Result<Arc<Workspace>, (StatusCode, String)> {
    if let Some(ref pool) = state.workspace_pool {
        return Ok(pool.get_or_create(user).await);
    }
    state.workspace.as_ref().cloned().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Workspace not available".to_string(),
    ))
}

#[derive(Deserialize)]
pub struct TreeQuery {
    #[allow(dead_code)]
    pub depth: Option<usize>,
}

pub async fn memory_tree_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(_query): Query<TreeQuery>,
) -> Result<Json<MemoryTreeResponse>, (StatusCode, String)> {
    let workspace = resolve_workspace(&state, &user).await?;

    // Build tree from list_all (flat list of all paths)
    let all_paths = workspace
        .list_all()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Collect unique directories and files
    let mut entries: Vec<TreeEntry> = Vec::new();
    let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();

    for path in &all_paths {
        // Add parent directories
        let parts: Vec<&str> = path.split('/').collect();
        for i in 0..parts.len().saturating_sub(1) {
            let dir_path = parts[..=i].join("/");
            if seen_dirs.insert(dir_path.clone()) {
                entries.push(TreeEntry {
                    path: dir_path,
                    is_dir: true,
                });
            }
        }
        // Add the file itself
        entries.push(TreeEntry {
            path: path.clone(),
            is_dir: false,
        });
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Json(MemoryTreeResponse { entries }))
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub path: Option<String>,
}

pub async fn memory_list_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<MemoryListResponse>, (StatusCode, String)> {
    let workspace = resolve_workspace(&state, &user).await?;

    let path = query.path.as_deref().unwrap_or("");
    let entries = workspace
        .list(path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let list_entries: Vec<ListEntry> = entries
        .iter()
        .map(|e| ListEntry {
            name: e.path.rsplit('/').next().unwrap_or(&e.path).to_string(),
            path: e.path.clone(),
            is_dir: e.is_directory,
            updated_at: e.updated_at.map(|dt| dt.to_rfc3339()),
        })
        .collect();

    Ok(Json(MemoryListResponse {
        path: path.to_string(),
        entries: list_entries,
    }))
}

#[derive(Deserialize)]
pub struct ReadQuery {
    pub path: String,
}

pub async fn memory_read_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<ReadQuery>,
) -> Result<Json<MemoryReadResponse>, (StatusCode, String)> {
    let workspace = resolve_workspace(&state, &user).await?;

    let doc = workspace
        .read(&query.path)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    Ok(Json(MemoryReadResponse {
        path: query.path,
        content: doc.content,
        updated_at: Some(doc.updated_at.to_rfc3339()),
    }))
}

pub async fn memory_write_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(req): Json<MemoryWriteRequest>,
) -> Result<Json<MemoryWriteResponse>, (StatusCode, String)> {
    let workspace = resolve_workspace(&state, &user).await?;

    // Route through layer-aware methods when a layer is specified.
    //
    // Note: unlike MemoryWriteTool, this endpoint does NOT block writes to
    // identity files (IDENTITY.md, SOUL.md, etc.). The HTTP API is an
    // authenticated admin interface; the supervisor uses it to seed identity
    // files at startup. Identity-file protection is enforced at the tool
    // layer (LLM-facing) where the write originates from an untrusted agent.
    if let Some(ref layer_name) = req.layer {
        let result = if req.append {
            workspace
                .append_to_layer(layer_name, &req.path, &req.content, req.force)
                .await
        } else {
            workspace
                .write_to_layer(layer_name, &req.path, &req.content, req.force)
                .await
        }
        .map_err(|e| (write_error_status(&e), e.to_string()))?;
        return Ok(Json(MemoryWriteResponse {
            path: req.path,
            status: "written",
            redirected: Some(result.redirected),
            actual_layer: Some(result.actual_layer),
        }));
    }

    // Non-layer path: honor the append field
    if req.append {
        workspace
            .append(&req.path, &req.content)
            .await
            .map_err(|e| (write_error_status(&e), e.to_string()))?;
    } else {
        workspace
            .write(&req.path, &req.content)
            .await
            .map_err(|e| (write_error_status(&e), e.to_string()))?;
    }

    Ok(Json(MemoryWriteResponse {
        path: req.path,
        status: "written",
        redirected: None,
        actual_layer: None,
    }))
}

/// HTTP status for a failed workspace write. Content refused by the
/// prompt-injection gate is a client input problem (422, matching the prompt
/// editor); a daily-log quarantine and an append refused only because of
/// content already stored in the file are state conflicts (409: the request
/// body was fine, the stored file is not). None is a server fault, so none
/// maps to 500 (WI#290).
fn write_error_status(error: &crate::error::WorkspaceError) -> StatusCode {
    use crate::error::WorkspaceError;
    match error {
        WorkspaceError::LayerNotFound { .. } => StatusCode::BAD_REQUEST,
        WorkspaceError::LayerReadOnly { .. } => StatusCode::FORBIDDEN,
        WorkspaceError::PrivacyRedirectFailed | WorkspaceError::InjectionRejected { .. } => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        WorkspaceError::DailyLogQuarantined { .. }
        | WorkspaceError::ExistingContentRejected { .. } => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn search_result_to_hit(result: &crate::workspace::SearchResult) -> SearchHit {
    SearchHit {
        path: result.document_path.clone(),
        content: result.content.clone(),
        score: result.score as f64,
    }
}

pub async fn memory_search_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(req): Json<MemorySearchRequest>,
) -> Result<Json<MemorySearchResponse>, (StatusCode, String)> {
    let workspace = resolve_workspace(&state, &user).await?;

    let limit = req.limit.unwrap_or(10);
    let results = workspace
        .search(&req.query, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let total_matches = results.len();
    let hits = results.iter().map(search_result_to_hit).collect();

    Ok(Json(MemorySearchResponse {
        results: hits,
        total_matches,
        truncated: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn search_hit_uses_readable_document_path() {
        let result = crate::workspace::SearchResult {
            document_id: Uuid::new_v4(),
            document_path: "memory/ledger.md".to_string(),
            chunk_id: Uuid::new_v4(),
            content: "ledger entry".to_string(),
            score: 0.75,
            fts_rank: Some(1),
            vector_rank: None,
        };

        let hit = search_result_to_hit(&result);

        assert_eq!(hit.path, "memory/ledger.md");
        assert_eq!(hit.content, "ledger entry");
        assert_eq!(hit.score, 0.75);
    }

    /// WI#290: content refused by the injection gate (422), a daily-log
    /// quarantine and an append refused because of stored content (409) are
    /// reported as client-side statuses, not a 500.
    #[test]
    fn write_error_status_classifies_content_rejections() {
        use crate::error::WorkspaceError;
        let rejected = WorkspaceError::InjectionRejected {
            path: "MEMORY.md".to_string(),
            reason: "Attempt to override previous instructions (line 1)".to_string(),
        };
        assert_eq!(
            write_error_status(&rejected),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let quarantined = WorkspaceError::DailyLogQuarantined {
            path: "daily/2026-10-02.md".to_string(),
            quarantine_path: "daily/2026-10-02.poisoned-1.md".to_string(),
            reason: "Attempt to override previous instructions (line 2)".to_string(),
        };
        assert_eq!(write_error_status(&quarantined), StatusCode::CONFLICT);
        let stored_poison = WorkspaceError::ExistingContentRejected {
            path: "MEMORY.md".to_string(),
            reason: "Attempt to inject user message (line 1)".to_string(),
        };
        assert_eq!(write_error_status(&stored_poison), StatusCode::CONFLICT);
        assert_eq!(
            write_error_status(&WorkspaceError::LayerNotFound {
                name: "missing".to_string()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            write_error_status(&WorkspaceError::IoError {
                reason: "disk".to_string()
            }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
