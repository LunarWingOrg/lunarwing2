//! Public webhook trigger endpoint for routine webhook triggers.
//!
//! `POST /api/webhooks/{path}` — matches the path against routines with
//! `Trigger::Webhook { path, secret }`, validates the secret via constant-time
//! comparison, and fires the matching routine through the `RoutineEngine`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use subtle::ConstantTimeEq;

use crate::agent::routine::Trigger;
use crate::bridge::router::fire_engine_webhook_mission;
use crate::channels::web::server::GatewayState;

/// Validate the webhook secret for a routine.
///
/// Returns `Ok(())` if the routine has a configured secret and the provided
/// secret matches via constant-time comparison. Returns an appropriate HTTP
/// error if the secret is missing (403) or invalid (401).
fn validate_webhook_secret(
    trigger: &Trigger,
    provided_secret: &str,
) -> Result<(), (StatusCode, String)> {
    // Require webhook secret — routines without a secret cannot be triggered via webhook
    let expected_secret = match trigger {
        Trigger::Webhook {
            secret: Some(s), ..
        } => s,
        _ => {
            return Err((
                StatusCode::FORBIDDEN,
                "Webhook secret not configured for this routine. \
                 Set a secret with: lunarwing routine update <id> --webhook-secret <secret>"
                    .to_string(),
            ));
        }
    };

    if !bool::from(provided_secret.as_bytes().ct_eq(expected_secret.as_bytes())) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Invalid webhook secret".to_string(),
        ));
    }

    Ok(())
}

fn parse_mission_webhook_payload(body: &[u8]) -> Result<serde_json::Value, (StatusCode, String)> {
    serde_json::from_slice(body).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Webhook body must be valid JSON".to_string(),
        )
    })
}

/// Handle incoming webhook POST to `/api/webhooks/{path}`.
///
/// This endpoint is **public** (no gateway auth token required) but protected
/// by the per-routine webhook secret sent via the `X-Webhook-Secret` header.
pub async fn webhook_trigger_handler(
    State(state): State<Arc<GatewayState>>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Rate limit check
    if !state.webhook_rate_limiter.check() {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Rate limit exceeded. Try again shortly.".to_string(),
        ));
    }

    let store = state.store.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Database not available".to_string(),
    ))?;

    // Targeted query instead of loading all routines
    let routine = store
        .get_webhook_routine_by_path(&path, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((
            StatusCode::NOT_FOUND,
            "No routine matches this webhook path".to_string(),
        ))?;

    let provided_secret = headers
        .get("x-webhook-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    validate_webhook_secret(&routine.trigger, provided_secret)?;

    // Fire through the RoutineEngine so guardrails, run tracking,
    // notifications, and FullJob dispatch all work correctly.
    let engine = {
        let guard = state.routine_engine.read().await;
        guard.as_ref().cloned().ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "Routine engine not available".to_string(),
        ))?
    };

    let run_id = engine.fire_webhook(routine.id, &path).await.map_err(|e| {
        let status = match &e {
            crate::error::RoutineError::NotFound { .. } => StatusCode::NOT_FOUND,
            crate::error::RoutineError::Disabled { .. }
            | crate::error::RoutineError::Cooldown { .. }
            | crate::error::RoutineError::MaxConcurrent { .. } => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    })?;

    Ok(Json(serde_json::json!({
        "status": "triggered",
        "routine_id": routine.id,
        "routine_name": routine.name,
        "run_id": run_id,
    })))
}

/// Extract the `X-Webhook-Secret` header value for a mission webhook fire.
///
/// WI#359 (CO-1): an empty or whitespace-only value maps to `None` ("no
/// secret provided") so a legacy mission stored with an empty secret can
/// never be authenticated by presenting an empty header.
fn mission_webhook_secret(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-webhook-secret")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// Handle incoming webhook POST to `/api/missions/webhook/{path}`.
///
/// This endpoint routes payloads to Engine V2 `Webhook`-cadence missions.
/// It is **public** (no gateway auth) but protected by per-mission webhook
/// secrets (validated by the engine layer via constant-time comparison).
pub async fn mission_webhook_handler(
    State(state): State<Arc<GatewayState>>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !state.webhook_rate_limiter.check() {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Rate limit exceeded. Try again shortly.".to_string(),
        ));
    }

    let secret = mission_webhook_secret(&headers);

    let payload = parse_mission_webhook_payload(&body)?;

    let fired = fire_engine_webhook_mission(&path, secret.as_deref(), payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if fired == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            "No Engine V2 mission matches this webhook path".to_string(),
        ));
    }

    Ok(Json(serde_json::json!({
        "status": "triggered",
        "missions_fired": fired,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_mission_webhook_json_maps_to_bad_request() {
        let result = parse_mission_webhook_payload(b"not-json");
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    /// WI#359 (CO-1): an empty or whitespace-only `X-Webhook-Secret` header
    /// must map to `None` so it can never authenticate a legacy mission
    /// stored with an empty secret.
    #[test]
    fn empty_mission_webhook_secret_header_maps_to_none() {
        let mut headers = HeaderMap::new();
        assert_eq!(mission_webhook_secret(&headers), None);
        headers.insert("x-webhook-secret", "".parse().unwrap());
        assert_eq!(mission_webhook_secret(&headers), None);
        headers.insert("x-webhook-secret", "   ".parse().unwrap());
        assert_eq!(mission_webhook_secret(&headers), None);
        headers.insert("x-webhook-secret", "real-secret".parse().unwrap());
        assert_eq!(mission_webhook_secret(&headers).as_deref(), Some("real-secret"));
    }

    /// Routines with `secret: None` must be rejected with 403.
    #[test]
    fn test_validate_rejects_missing_secret() {
        let trigger = Trigger::Webhook {
            path: Some("my-hook".to_string()),
            secret: None,
        };
        let result = validate_webhook_secret(&trigger, "any-secret");
        let (status, msg) = result.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            msg.contains("not configured"),
            "Error should tell user to configure a secret, got: {msg}"
        );
    }

    /// Non-webhook triggers must be rejected with 403.
    #[test]
    fn test_validate_rejects_non_webhook_trigger() {
        let trigger = Trigger::Manual;
        let result = validate_webhook_secret(&trigger, "any-secret");
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// Correct secret passes validation.
    #[test]
    fn test_validate_accepts_correct_secret() {
        let trigger = Trigger::Webhook {
            path: Some("my-hook".to_string()),
            secret: Some("s3cret-token".to_string()),
        };
        assert!(validate_webhook_secret(&trigger, "s3cret-token").is_ok());
    }

    /// Wrong secret returns 401.
    #[test]
    fn test_validate_rejects_wrong_secret() {
        let trigger = Trigger::Webhook {
            path: Some("my-hook".to_string()),
            secret: Some("correct-secret".to_string()),
        };
        let result = validate_webhook_secret(&trigger, "wrong-secret");
        let (status, msg) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(msg.contains("Invalid"), "Expected 'Invalid' in: {msg}");
    }

    /// Empty provided secret returns 401 (not a false positive).
    #[test]
    fn test_validate_rejects_empty_provided_secret() {
        let trigger = Trigger::Webhook {
            path: Some("my-hook".to_string()),
            secret: Some("real-secret".to_string()),
        };
        let result = validate_webhook_secret(&trigger, "");
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// Constant-time comparison: secrets of different lengths are still rejected
    /// (not short-circuited in a way that leaks length info).
    #[test]
    fn test_validate_rejects_different_length_secret() {
        let trigger = Trigger::Webhook {
            path: None,
            secret: Some("short".to_string()),
        };
        let result = validate_webhook_secret(&trigger, "a-much-longer-secret-value");
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
