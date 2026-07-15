//! Skills management API handlers.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::server::GatewayState;
use crate::channels::web::types::*;

pub async fn skills_list_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(_user): AuthenticatedUser,
) -> Result<Json<SkillListResponse>, (StatusCode, String)> {
    let registry = state.skill_registry.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Skills system not enabled".to_string(),
    ))?;

    let guard = registry.read().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Skill registry lock poisoned: {}", e),
        )
    })?;

    let skills: Vec<SkillInfo> = guard
        .skills()
        .iter()
        .map(|s| SkillInfo {
            name: s.manifest.name.clone(),
            description: s.manifest.description.clone(),
            version: s.manifest.version.clone(),
            trust: s.trust.to_string(),
            source: format!("{:?}", s.source),
            keywords: s.manifest.activation.keywords.clone(),
        })
        .collect();

    let count = skills.len();
    Ok(Json(SkillListResponse { skills, count }))
}

pub async fn skills_search_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(_user): AuthenticatedUser,
    Json(req): Json<SkillSearchRequest>,
) -> Result<Json<SkillSearchResponse>, (StatusCode, String)> {
    let registry = state.skill_registry.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Skills system not enabled".to_string(),
    ))?;

    let catalog = state.skill_catalog.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Skill catalog not available".to_string(),
    ))?;

    // Search ClawHub catalog
    let catalog_outcome = catalog.search(&req.query).await;
    let catalog_error = catalog_outcome.error.clone();

    // Enrich top results with detail data (stars, downloads, owner)
    let mut entries = catalog_outcome.results;
    catalog.enrich_search_results(&mut entries, 5).await;

    let catalog_json: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "slug": e.slug,
                "name": e.name,
                "description": e.description,
                "version": e.version,
                "score": e.score,
                "updatedAt": e.updated_at,
                "stars": e.stars,
                "downloads": e.downloads,
                "owner": e.owner,
            })
        })
        .collect();

    // Search local skills
    let query_lower = req.query.to_lowercase();
    let installed: Vec<SkillInfo> = {
        let guard = registry.read().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Skill registry lock poisoned: {}", e),
            )
        })?;
        guard
            .skills()
            .iter()
            .filter(|s| {
                s.manifest.name.to_lowercase().contains(&query_lower)
                    || s.manifest.description.to_lowercase().contains(&query_lower)
            })
            .map(|s| SkillInfo {
                name: s.manifest.name.clone(),
                description: s.manifest.description.clone(),
                version: s.manifest.version.clone(),
                trust: s.trust.to_string(),
                source: format!("{:?}", s.source),
                keywords: s.manifest.activation.keywords.clone(),
            })
            .collect()
    };

    Ok(Json(SkillSearchResponse {
        catalog: catalog_json,
        installed,
        registry_url: catalog.registry_url().to_string(),
        catalog_error,
    }))
}

pub async fn skills_install_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: axum::http::HeaderMap,
    Json(req): Json<SkillInstallRequest>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    // Require explicit confirmation header to prevent accidental installs.
    // Chat tools have requires_approval(); this is the equivalent for the web API.
    if headers
        .get("x-confirm-action")
        .and_then(|v| v.to_str().ok())
        != Some("true")
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Skill install requires X-Confirm-Action: true header".to_string(),
        ));
    }

    tracing::info!(user_id = %user.user_id, skill = %req.name, "skill install requested");

    let registry = state.skill_registry.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Skills system not enabled".to_string(),
    ))?;

    let content = if let Some(ref raw) = req.content {
        raw.clone()
    } else if let Some(ref url) = req.url {
        // Fetch from explicit URL (with SSRF protection)
        crate::tools::builtin::skill_tools::fetch_skill_content(url)
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    } else if let Some(ref catalog) = state.skill_catalog {
        // Prefer slug (e.g. "owner/skill-name") over display name for the
        // download URL, since the registry endpoint expects a slug.
        let download_key = req
            .slug
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&req.name);
        let url = crate::skills::catalog::skill_download_url(catalog.registry_url(), download_key);
        crate::tools::builtin::skill_tools::fetch_skill_content(&url)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
    } else {
        return Ok(Json(ActionResponse::fail(
            "Provide 'content' or 'url' to install a skill".to_string(),
        )));
    };

    // B-3: leak-scan the fetched skill body before installing it. Pulled skills
    // come from a public registry and must not bring credentials in. The
    // registry also scans server-side; this is the client-side gate.
    {
        let scan = lunarwing_safety::LeakDetector::new().scan(&content);
        if !scan.is_clean() {
            tracing::warn!(
                user_id = %user.user_id,
                skill = %req.name,
                "refusing to install skill: leak scan detected credentials"
            );
            return Ok(Json(ActionResponse::fail(
                "Skill body failed leak scan (credentials detected); refusing to install"
                    .to_string(),
            )));
        }
    }

    // Track registry provenance for the catalog-pull branch (B-3): written as
    // a `.registry.json` sidecar next to SKILL.md so the v1→v2 migration can
    // stamp it into V2SkillMetadata. None for inline-content / explicit-URL
    // installs (those aren't catalog pulls).
    let registry_provenance: Option<serde_json::Value> =
        if req.content.is_none() && req.url.is_none() {
            state.skill_catalog.as_ref().map(|cat| {
                let content_hash = {
                    use sha2::{Digest, Sha256};
                    let mut h = Sha256::new();
                    h.update(content.as_bytes());
                    format!("sha256:{:x}", h.finalize())
                };
                serde_json::json!({
                    "registry_url": cat.registry_url(),
                    "content_hash": content_hash,
                    "pulled_at": chrono::Utc::now(),
                })
            })
        } else {
            None
        };

    // Parse, check duplicates, and get install_dir under a brief read lock.
    let (user_dir, skill_name_from_parse) = {
        let guard = registry.read().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Skill registry lock poisoned: {}", e),
            )
        })?;

        let normalized = crate::skills::normalize_line_endings(&content);
        let parsed = crate::skills::parser::parse_skill_md(&normalized)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        let skill_name = parsed.manifest.name.clone();

        if guard.has(&skill_name) {
            return Ok(Json(ActionResponse::fail(format!(
                "Skill '{}' already exists",
                skill_name
            ))));
        }

        (guard.install_target_dir().to_path_buf(), skill_name)
    };

    // Perform async I/O (write to disk, load) with no lock held.
    let normalized = crate::skills::normalize_line_endings(&content);
    let (skill_name, loaded_skill) =
        crate::skills::registry::SkillRegistry::prepare_install_to_disk(
            &user_dir,
            &skill_name_from_parse,
            &normalized,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // B-3: write the registry provenance sidecar for catalog-pulled skills so
    // the v1→v2 migration can stamp it. Best-effort — a failed write just means
    // the skill installs without provenance (treated as locally-authored).
    if let Some(ref provenance) = registry_provenance {
        let sidecar = user_dir.join(&skill_name).join(".registry.json");
        if let Err(e) = std::fs::write(
            &sidecar,
            serde_json::to_string_pretty(provenance).unwrap_or_default(),
        ) {
            tracing::warn!("failed to write registry sidecar for {skill_name}: {e}");
        }
    }

    // Commit: brief write lock for in-memory addition
    let mut guard = registry.write().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Skill registry lock poisoned: {}", e),
        )
    })?;

    match guard.commit_install(&skill_name, loaded_skill) {
        Ok(()) => Ok(Json(ActionResponse::ok(format!(
            "Skill '{}' installed",
            skill_name
        )))),
        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
    }
}

pub async fn skills_remove_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    // Require explicit confirmation header to prevent accidental removals.
    if headers
        .get("x-confirm-action")
        .and_then(|v| v.to_str().ok())
        != Some("true")
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Skill removal requires X-Confirm-Action: true header".to_string(),
        ));
    }

    tracing::info!(user_id = %user.user_id, skill = %name, "skill remove requested");

    let registry = state.skill_registry.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Skills system not enabled".to_string(),
    ))?;

    // Validate removal under a brief read lock
    let skill_path = {
        let guard = registry.read().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Skill registry lock poisoned: {}", e),
            )
        })?;
        guard
            .validate_remove(&name)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    };

    // Delete files from disk (async I/O, no lock held)
    crate::skills::registry::SkillRegistry::delete_skill_files(&skill_path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Remove from in-memory registry under a brief write lock
    let mut guard = registry.write().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Skill registry lock poisoned: {}", e),
        )
    })?;

    match guard.commit_remove(&name) {
        Ok(()) => Ok(Json(ActionResponse::ok(format!(
            "Skill '{}' removed",
            name
        )))),
        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
    }
}

// ── B-1: self-improving skills — pending-patch approval API ──────────────
//
// These handlers surface the propose-then-approve loop to the operator. They
// call the engine bridge (which reads the global engine store), so they work
// only when the V2 engine is running. User scoping is enforced in the bridge.

/// GET /api/skills/patches — list pending skill-patch proposals.
pub async fn skill_patches_list_handler(
    State(_state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let proposals = crate::bridge::list_pending_skill_patches(&user.user_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let count = proposals.len();
    Ok(Json(serde_json::json!({
        "patches": proposals,
        "count": count,
    })))
}

/// POST /api/skills/patches/{doc_id}/approve — apply a pending patch.
pub async fn skill_patch_approve_handler(
    State(_state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(doc_id): Path<String>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    match crate::bridge::approve_skill_patch(&doc_id, &user.user_id).await {
        Ok(_) => Ok(Json(ActionResponse::ok(format!(
            "Skill patch approved and applied for {doc_id}"
        )))),
        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
    }
}

/// POST /api/skills/patches/{doc_id}/reject — discard a pending patch.
pub async fn skill_patch_reject_handler(
    State(_state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(doc_id): Path<String>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    match crate::bridge::reject_skill_patch(&doc_id, &user.user_id).await {
        Ok(_) => Ok(Json(ActionResponse::ok(format!(
            "Skill patch rejected for {doc_id}"
        )))),
        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
    }
}

// ── B-1/B-2/B-3: unified skill proposals approval API ────────────────────
//
// Generalizes the patch-only surface with a `kind` discriminator so the GUI
// panel can render and act on patch, prune, and update proposals through one
// endpoint. The patch routes above are kept as thin aliases for back-compat.

/// Body for approve/reject: `{ "kind": "patch" | "prune" | "update" }`.
#[derive(Debug, serde::Deserialize)]
pub struct ProposalActionBody {
    pub kind: Option<String>,
}

/// Parse the kind body, defaulting to `patch` (back-compat with B-1 callers
/// that hit the unified endpoint without specifying kind).
fn parse_kind(body: &ProposalActionBody) -> lunarwing_skills::v2::ProposalKind {
    use lunarwing_skills::v2::ProposalKind;
    match body
        .kind
        .as_deref()
        .map(str::trim)
        .unwrap_or("patch")
        .to_lowercase()
        .as_str()
    {
        "prune" => ProposalKind::Prune,
        "update" => ProposalKind::Update,
        _ => ProposalKind::Patch,
    }
}

/// GET /api/skills/proposals — list all pending skill proposals (patch/prune/update).
pub async fn skill_proposals_list_handler(
    State(_state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let proposals = crate::bridge::list_pending_skill_proposals(&user.user_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let count = proposals.len();
    Ok(Json(serde_json::json!({
        "proposals": proposals,
        "count": count,
    })))
}

/// POST /api/skills/proposals/{doc_id}/approve — apply a pending proposal.
/// Body: `{ "kind": "patch" | "prune" | "update" }` (defaults to `patch`).
pub async fn skill_proposal_approve_handler(
    State(_state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(doc_id): Path<String>,
    body: Option<Json<ProposalActionBody>>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let kind = parse_kind(
        &body
            .map(|b| b.0)
            .unwrap_or(ProposalActionBody { kind: None }),
    );
    match crate::bridge::approve_skill_proposal(&doc_id, kind, &user.user_id).await {
        Ok(_) => Ok(Json(ActionResponse::ok(format!(
            "Skill {kind:?} proposal approved for {doc_id}"
        )))),
        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
    }
}

/// POST /api/skills/proposals/{doc_id}/reject — discard a pending proposal.
/// Body: `{ "kind": "patch" | "prune" | "update" }` (defaults to `patch`).
pub async fn skill_proposal_reject_handler(
    State(_state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(doc_id): Path<String>,
    body: Option<Json<ProposalActionBody>>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    let kind = parse_kind(
        &body
            .map(|b| b.0)
            .unwrap_or(ProposalActionBody { kind: None }),
    );
    match crate::bridge::reject_skill_proposal(&doc_id, kind, &user.user_id).await {
        Ok(_) => Ok(Json(ActionResponse::ok(format!(
            "Skill {kind:?} proposal rejected for {doc_id}"
        )))),
        Err(e) => Ok(Json(ActionResponse::fail(e.to_string()))),
    }
}

// ── B-3: cross-agent skill sharing — publish API ──────────────────────────

/// Body for publish: `{ "slug", "version", "changelog"?, "tags"? }`.
#[derive(Debug, serde::Deserialize)]
pub struct PublishBody {
    pub slug: String,
    pub version: String,
    #[serde(default)]
    pub changelog: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// POST /api/skills/{doc_id}/publish — publish a skill to the registry.
/// Auth-gated, user-scoped, explicit (never automatic). The bridge enforces
/// publish eligibility (Trusted + proven) + leak-scans before sending.
pub async fn skill_publish_handler(
    State(_state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(doc_id): Path<String>,
    body: Option<Json<PublishBody>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let body = body.map(|b| b.0).unwrap_or(PublishBody {
        slug: String::new(),
        version: String::new(),
        changelog: String::new(),
        tags: vec![],
    });
    if body.slug.trim().is_empty() || body.version.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "slug and version are required".to_string(),
        ));
    }
    let result = crate::bridge::publish_skill(
        &doc_id,
        &user.user_id,
        &body.slug,
        &body.version,
        &body.changelog,
        body.tags,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::to_value(&result).unwrap_or_default()))
}
