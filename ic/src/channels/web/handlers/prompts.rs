//! Operator editing of the authenticated workspace's prompt sources (WI#220).
//!
//! This is a workspace preview, not a dump of a running Engine V2 thread.
//! Identity/configuration reads deliberately never fall through to read scopes.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::handlers::memory::resolve_workspace;
use crate::channels::web::server::GatewayState;
use crate::error::WorkspaceError;
use crate::workspace::paths;

const EDITABLE_SOURCES: [&str; 7] = [
    paths::AGENTS,
    paths::SOUL,
    paths::USER,
    paths::IDENTITY,
    paths::TOOLS,
    paths::MEMORY,
    paths::BOOTSTRAP,
];
const MAX_SOURCE_BYTES: usize = 256 * 1024;

type ApiError = (StatusCode, String);

/// Mounted inside the gateway's authenticated router, also used by route tests.
pub(crate) fn routes() -> Router<Arc<GatewayState>> {
    Router::new()
        .route("/api/prompts", get(list_sources))
        .route("/api/prompts/preview", get(preview))
        .route("/api/prompts/write", post(write_source))
}

#[derive(Serialize)]
struct PromptSource {
    path: String,
    content: String,
    exists: bool,
    updated_at: Option<String>,
}

#[derive(Serialize)]
struct PromptSources {
    scope: String,
    files: Vec<PromptSource>,
}

fn private_json(value: impl Serialize) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}

fn workspace_error(error: WorkspaceError) -> ApiError {
    if matches!(error, WorkspaceError::InjectionRejected { .. }) {
        return (StatusCode::UNPROCESSABLE_ENTITY, error.to_string());
    }
    tracing::error!(%error, "Prompt workspace operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Prompt workspace operation failed".to_string(),
    )
}

async fn list_sources(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Response, ApiError> {
    let workspace = resolve_workspace(&state, &user).await?;
    let mut files = Vec::with_capacity(EDITABLE_SOURCES.len());
    for path in EDITABLE_SOURCES {
        let (content, exists, updated_at) = match workspace.read_primary(path).await {
            Ok(doc) => (doc.content, true, Some(doc.updated_at.to_rfc3339())),
            Err(WorkspaceError::DocumentNotFound { .. }) => (String::new(), false, None),
            Err(error) => return Err(workspace_error(error)),
        };
        files.push(PromptSource {
            path: path.to_string(),
            content,
            exists,
            updated_at,
        });
    }
    Ok(private_json(PromptSources {
        scope: workspace.user_id().to_string(),
        files,
    }))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewQuery {
    #[serde(default)]
    is_group_chat: bool,
    timezone: Option<String>,
}

#[derive(Serialize)]
struct PromptPreview {
    scope: String,
    kind: &'static str,
    timezone: String,
    is_group_chat: bool,
    prompt: String,
}

async fn preview(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<PreviewQuery>,
) -> Result<Response, ApiError> {
    let tz = query
        .timezone
        .as_deref()
        .unwrap_or("UTC")
        .parse::<chrono_tz::Tz>()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid IANA timezone".to_string()))?;
    let workspace = resolve_workspace(&state, &user).await?;
    // The gateway pool also exists in single-owner deployments. Carry over
    // that owner's initialized onboarding state, without applying it to a
    // different authenticated user's workspace.
    if state.workspace.as_ref().is_some_and(|primary| {
        primary.user_id() == workspace.user_id() && primary.is_bootstrap_completed()
    }) {
        workspace.mark_bootstrap_completed();
    }
    let prompt = workspace
        .system_prompt_for_context_tz(query.is_group_chat, tz)
        .await
        .map_err(workspace_error)?;
    Ok(private_json(PromptPreview {
        scope: workspace.user_id().to_string(),
        kind: "workspace",
        timezone: tz.to_string(),
        is_group_chat: query.is_group_chat,
        prompt,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteSourceRequest {
    path: String,
    content: String,
}

async fn write_source(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(req): Json<WriteSourceRequest>,
) -> Result<Response, ApiError> {
    if !EDITABLE_SOURCES.contains(&req.path.as_str()) {
        return Err((StatusCode::BAD_REQUEST, "Unknown prompt source".to_string()));
    }
    if req.content.len() > MAX_SOURCE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "Prompt source exceeds 256 KiB".to_string(),
        ));
    }
    let workspace = resolve_workspace(&state, &user).await?;
    // Keep the normal prompt-injection scan and search re-indexing. There is
    // no layer/force/user parameter: writes always target this primary scope.
    let doc = workspace
        .write(&req.path, &req.content)
        .await
        .map_err(workspace_error)?;
    Ok(private_json(PromptSource {
        path: req.path,
        content: doc.content,
        exists: true,
        updated_at: Some(doc.updated_at.to_rfc3339()),
    }))
}
