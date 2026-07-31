//! Localhost-only admin endpoints for the coordination pipeline (Phase 3).
//!
//! `docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`:
//! dedicated endpoints, bound to `127.0.0.1` only — intentionally NOT routed
//! through the authenticated web gateway. Started from `main.rs` when
//! `COORD_ENABLED=true`.
//!
//! All routes require `Authorization: Bearer <COORD_ADMIN_TOKEN>`; the token
//! is compared in constant time and the server refuses to start without one
//! (ISSUE-009). Loopback binding alone is not an authorization boundary on a
//! multi-user host. Agent path values are validated against a conservative
//! length/character policy before touching the store.
//!
//! - `GET  /coord/status`          — all tracked agents
//! - `GET  /coord/status/{agent}`  — single agent
//! - `POST /coord/clear/{agent}`   — reset violations
//! - `POST /coord/disable/{agent}` — manual disable
//! - `POST /coord/enable/{agent}`  — manual enable

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use subtle::ConstantTimeEq;

use crate::gate::violation_store::{ViolationStatus, ViolationStore};
#[cfg(test)]
use crate::hooks::MAX_COORD_AGENT_ID_LEN;
use crate::hooks::valid_coordination_agent_id;

/// Default port for the coordination admin server (`COORD_ADMIN_PORT`).
pub const DEFAULT_COORD_ADMIN_PORT: u16 = 18777;

/// Environment variable carrying the admin bearer token.
pub const COORD_ADMIN_TOKEN_ENV: &str = "COORD_ADMIN_TOKEN";

#[derive(Clone)]
struct CoordAdminState {
    store: Arc<ViolationStore>,
    token: SecretString,
}

#[derive(Serialize)]
struct AgentStatusView {
    agent: String,
    status: String,
}

#[derive(Serialize)]
struct AllStatusesView {
    agents: Vec<AgentStatusView>,
}

#[derive(Serialize)]
struct ActionView {
    agent: String,
    action: &'static str,
    status: String,
}

fn status_label(status: &ViolationStatus) -> String {
    match status {
        ViolationStatus::Active => "active".to_string(),
        ViolationStatus::Warning => "warning".to_string(),
        ViolationStatus::FinalWarning => "final_warning".to_string(),
        ViolationStatus::Disabled { until_secs } => {
            if *until_secs == u64::MAX {
                "disabled_manual".to_string()
            } else {
                format!("disabled_until:{until_secs}")
            }
        }
    }
}

/// Conservative agent-ID policy: non-empty, bounded length, and only
/// characters seen in JIDs and local agent names. Rejects control bytes,
/// whitespace, percent-encoding tricks, and store-flooding identifiers.
/// Bearer-token gate for every coordination admin route.
async fn require_auth(
    State(state): State<CoordAdminState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|provided| {
            bool::from(
                provided
                    .as_bytes()
                    .ct_eq(state.token.expose_secret().as_bytes()),
            )
        });
    if authorized {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn get_all_statuses(State(state): State<CoordAdminState>) -> Json<AllStatusesView> {
    let mut agents: Vec<AgentStatusView> = state
        .store
        .all_statuses()
        .into_iter()
        .map(|(agent, status)| AgentStatusView {
            agent,
            status: status_label(&status),
        })
        .collect();
    agents.sort_by(|a, b| a.agent.cmp(&b.agent));
    Json(AllStatusesView { agents })
}

async fn get_agent_status(
    State(state): State<CoordAdminState>,
    Path(agent): Path<String>,
) -> Result<Json<AgentStatusView>, StatusCode> {
    if !valid_coordination_agent_id(&agent) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(AgentStatusView {
        status: status_label(&state.store.status(&agent)),
        agent,
    }))
}

async fn clear_agent(
    State(state): State<CoordAdminState>,
    Path(agent): Path<String>,
) -> Result<Json<ActionView>, StatusCode> {
    if !valid_coordination_agent_id(&agent) {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.store.clear(&agent);
    tracing::info!(agent = %agent, "coordination admin: violations cleared");
    Ok(Json(ActionView {
        status: status_label(&state.store.status(&agent)),
        agent,
        action: "cleared",
    }))
}

async fn disable_agent(
    State(state): State<CoordAdminState>,
    Path(agent): Path<String>,
) -> Result<Json<ActionView>, StatusCode> {
    if !valid_coordination_agent_id(&agent) {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.store.manual_disable(&agent);
    tracing::info!(agent = %agent, "coordination admin: agent manually disabled");
    Ok(Json(ActionView {
        status: status_label(&state.store.status(&agent)),
        agent,
        action: "disabled",
    }))
}

async fn enable_agent(
    State(state): State<CoordAdminState>,
    Path(agent): Path<String>,
) -> Result<Json<ActionView>, StatusCode> {
    if !valid_coordination_agent_id(&agent) {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.store.manual_enable(&agent);
    tracing::info!(agent = %agent, "coordination admin: agent manually enabled");
    Ok(Json(ActionView {
        status: status_label(&state.store.status(&agent)),
        agent,
        action: "enabled",
    }))
}

/// Router for the coordination admin API (also used by tests).
pub fn coord_admin_router(store: Arc<ViolationStore>, token: SecretString) -> Router {
    let state = CoordAdminState { store, token };
    Router::new()
        .route("/coord/status", get(get_all_statuses))
        .route("/coord/status/{agent}", get(get_agent_status))
        .route("/coord/clear/{agent}", post(clear_agent))
        .route("/coord/disable/{agent}", post(disable_agent))
        .route("/coord/enable/{agent}", post(enable_agent))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .with_state(state)
}

/// Start the localhost-only coordination admin server.
///
/// Binds `127.0.0.1:{port}` exclusively — never an external interface.
/// Returns the bound address; the server runs on a spawned task for the
/// lifetime of the process.
pub async fn start_coord_admin_server(
    store: Arc<ViolationStore>,
    token: SecretString,
    port: u16,
) -> Result<SocketAddr, std::io::Error> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr().unwrap_or(addr);
    let app = coord_admin_router(store, token);
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!(error = %err, "Coordination admin server stopped unexpectedly");
        }
    });
    tracing::info!(addr = %bound, "Coordination admin server listening (localhost only)");
    Ok(bound)
}

/// Port from `COORD_ADMIN_PORT`, falling back to [`DEFAULT_COORD_ADMIN_PORT`].
pub fn coord_admin_port_from_env() -> u16 {
    std::env::var("COORD_ADMIN_PORT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_COORD_ADMIN_PORT)
}

/// Admin bearer token from [`COORD_ADMIN_TOKEN_ENV`].
///
/// Returns `None` when unset or empty; the caller must then skip starting the
/// admin server (fail closed) rather than expose unauthenticated mutations.
pub fn coord_admin_token_from_env() -> Option<SecretString> {
    std::env::var(COORD_ADMIN_TOKEN_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(SecretString::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::violation_store::ViolationStoreConfig;

    const TEST_TOKEN: &str = "test-coord-admin-token";

    fn test_store() -> Arc<ViolationStore> {
        let dir = std::env::temp_dir().join(format!(
            "coord-admin-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        Arc::new(ViolationStore::new(
            dir.join("violations.json"),
            ViolationStoreConfig {
                max_violations: 2,
                disable_duration_secs: 3600,
                decay_secs: 600,
            },
        ))
    }

    fn authed(client: &reqwest::Client, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let _ = client;
        req.bearer_auth(TEST_TOKEN)
    }

    #[test]
    fn test_status_labels() {
        assert_eq!(status_label(&ViolationStatus::Active), "active");
        assert_eq!(status_label(&ViolationStatus::Warning), "warning");
        assert_eq!(
            status_label(&ViolationStatus::FinalWarning),
            "final_warning"
        );
        assert_eq!(
            status_label(&ViolationStatus::Disabled {
                until_secs: u64::MAX
            }),
            "disabled_manual"
        );
        assert_eq!(
            status_label(&ViolationStatus::Disabled { until_secs: 42 }),
            "disabled_until:42"
        );
    }

    #[test]
    fn test_valid_agent_id() {
        assert!(valid_coordination_agent_id("agent-1"));
        assert!(valid_coordination_agent_id(
            "luna@conference.example/resource"
        ));
        assert!(valid_coordination_agent_id("a"));
        assert!(!valid_coordination_agent_id(""));
        assert!(!valid_coordination_agent_id(
            &"x".repeat(MAX_COORD_AGENT_ID_LEN + 1)
        ));
        assert!(!valid_coordination_agent_id("bad id"));
        assert!(!valid_coordination_agent_id("bad%20id"));
        assert!(!valid_coordination_agent_id("bad\nid"));
        assert!(!valid_coordination_agent_id("bad?id"));
    }

    #[tokio::test]
    async fn test_admin_server_requires_auth() {
        let store = test_store();
        let addr = match start_coord_admin_server(
            Arc::clone(&store),
            SecretString::from(TEST_TOKEN.to_string()),
            0,
        )
        .await
        {
            Ok(addr) => addr,
            Err(err) => panic!("failed to start admin server: {err}"),
        };
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // Missing credentials: every route rejects.
        for path in [
            "/coord/status",
            "/coord/status/agent-x",
            "/coord/clear/agent-x",
            "/coord/disable/agent-x",
            "/coord/enable/agent-x",
        ] {
            let resp = match client.get(format!("{base}{path}")).send().await {
                Ok(r) => r,
                Err(err) => panic!("GET {path} failed: {err}"),
            };
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "GET {path}");
        }
        let resp = match client
            .post(format!("{base}/coord/disable/agent-x"))
            .send()
            .await
        {
            Ok(r) => r,
            Err(err) => panic!("POST without auth failed: {err}"),
        };
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(!store.is_disabled("agent-x"));

        // Wrong token rejects too.
        let resp = match client
            .get(format!("{base}/coord/status"))
            .bearer_auth("wrong-token")
            .send()
            .await
        {
            Ok(r) => r,
            Err(err) => panic!("GET with wrong token failed: {err}"),
        };
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Authorized calls work.
        let resp = match authed(&client, client.get(format!("{base}/coord/status")))
            .send()
            .await
        {
            Ok(r) => r,
            Err(err) => panic!("authorized GET failed: {err}"),
        };
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_admin_server_rejects_bad_agent_ids() {
        let store = test_store();
        let addr = match start_coord_admin_server(
            Arc::clone(&store),
            SecretString::from(TEST_TOKEN.to_string()),
            0,
        )
        .await
        {
            Ok(addr) => addr,
            Err(err) => panic!("failed to start admin server: {err}"),
        };
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // Overlong identifier.
        let long = "x".repeat(MAX_COORD_AGENT_ID_LEN + 1);
        let resp = match authed(&client, client.post(format!("{base}/coord/disable/{long}")))
            .send()
            .await
        {
            Ok(r) => r,
            Err(err) => panic!("POST overlong id failed: {err}"),
        };
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // Illegal character (space, percent-encoded).
        let resp = match authed(
            &client,
            client.post(format!("{base}/coord/disable/bad%20id")),
        )
        .send()
        .await
        {
            Ok(r) => r,
            Err(err) => panic!("POST encoded id failed: {err}"),
        };
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_admin_server_status_and_actions() {
        let store = test_store();
        store.record_violation("agent-x", "r", "hook", "chan");
        let addr = match start_coord_admin_server(
            Arc::clone(&store),
            SecretString::from(TEST_TOKEN.to_string()),
            0,
        )
        .await
        {
            Ok(addr) => addr,
            Err(err) => panic!("failed to start admin server: {err}"),
        };
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        let resp = match authed(&client, client.get(format!("{base}/coord/status")))
            .send()
            .await
        {
            Ok(r) => r,
            Err(err) => panic!("GET /coord/status failed: {err}"),
        };
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(err) => panic!("invalid JSON from /coord/status: {err}"),
        };
        assert_eq!(body["agents"][0]["agent"], "agent-x");
        assert_eq!(body["agents"][0]["status"], "warning");

        let resp = match authed(&client, client.get(format!("{base}/coord/status/agent-x")))
            .send()
            .await
        {
            Ok(r) => r,
            Err(err) => panic!("GET /coord/status/agent-x failed: {err}"),
        };
        assert_eq!(resp.status(), StatusCode::OK);

        // Second strike disables (max_violations = 2).
        store.record_violation("agent-x", "r2", "hook", "chan");
        let resp = match authed(&client, client.get(format!("{base}/coord/status/agent-x")))
            .send()
            .await
        {
            Ok(r) => r,
            Err(err) => panic!("GET after disable failed: {err}"),
        };
        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(err) => panic!("invalid JSON after disable: {err}"),
        };
        assert!(
            body["status"]
                .as_str()
                .unwrap_or_default()
                .starts_with("disabled")
        );

        // Clear re-enables.
        let resp = match authed(&client, client.post(format!("{base}/coord/clear/agent-x")))
            .send()
            .await
        {
            Ok(r) => r,
            Err(err) => panic!("POST /coord/clear failed: {err}"),
        };
        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(err) => panic!("invalid JSON from clear: {err}"),
        };
        assert_eq!(body["status"], "active");

        // Manual disable/enable round-trip.
        let _ = authed(
            &client,
            client.post(format!("{base}/coord/disable/agent-y")),
        )
        .send()
        .await;
        assert!(store.is_disabled("agent-y"));
        let _ = authed(&client, client.post(format!("{base}/coord/enable/agent-y")))
            .send()
            .await;
        assert!(!store.is_disabled("agent-y"));
    }
}
