//! Localhost-only admin endpoints for the coordination pipeline (Phase 3).
//!
//! `docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`:
//! dedicated endpoints, bound to `127.0.0.1` only — intentionally NOT routed
//! through the authenticated web gateway. Started from `main.rs` when
//! `COORD_ENABLED=true`.
//!
//! - `GET  /coord/status`          — all tracked agents
//! - `GET  /coord/status/{agent}`  — single agent
//! - `POST /coord/clear/{agent}`   — reset violations
//! - `POST /coord/disable/{agent}` — manual disable
//! - `POST /coord/enable/{agent}`  — manual enable

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::gate::violation_store::{ViolationStatus, ViolationStore};

/// Default port for the coordination admin server (`COORD_ADMIN_PORT`).
pub const DEFAULT_COORD_ADMIN_PORT: u16 = 18777;

#[derive(Clone)]
struct CoordAdminState {
    store: Arc<ViolationStore>,
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
) -> Json<AgentStatusView> {
    Json(AgentStatusView {
        status: status_label(&state.store.status(&agent)),
        agent,
    })
}

async fn clear_agent(
    State(state): State<CoordAdminState>,
    Path(agent): Path<String>,
) -> Json<ActionView> {
    state.store.clear(&agent);
    tracing::info!(agent = %agent, "coordination admin: violations cleared");
    Json(ActionView {
        status: status_label(&state.store.status(&agent)),
        agent,
        action: "cleared",
    })
}

async fn disable_agent(
    State(state): State<CoordAdminState>,
    Path(agent): Path<String>,
) -> Json<ActionView> {
    state.store.manual_disable(&agent);
    tracing::info!(agent = %agent, "coordination admin: agent manually disabled");
    Json(ActionView {
        status: status_label(&state.store.status(&agent)),
        agent,
        action: "disabled",
    })
}

async fn enable_agent(
    State(state): State<CoordAdminState>,
    Path(agent): Path<String>,
) -> Json<ActionView> {
    state.store.manual_enable(&agent);
    tracing::info!(agent = %agent, "coordination admin: agent manually enabled");
    Json(ActionView {
        status: status_label(&state.store.status(&agent)),
        agent,
        action: "enabled",
    })
}

/// Router for the coordination admin API (also used by tests).
pub fn coord_admin_router(store: Arc<ViolationStore>) -> Router {
    Router::new()
        .route("/coord/status", get(get_all_statuses))
        .route("/coord/status/{agent}", get(get_agent_status))
        .route("/coord/clear/{agent}", post(clear_agent))
        .route("/coord/disable/{agent}", post(disable_agent))
        .route("/coord/enable/{agent}", post(enable_agent))
        .with_state(CoordAdminState { store })
}

/// Start the localhost-only coordination admin server.
///
/// Binds `127.0.0.1:{port}` exclusively — never an external interface.
/// Returns the bound address; the server runs on a spawned task for the
/// lifetime of the process.
pub async fn start_coord_admin_server(
    store: Arc<ViolationStore>,
    port: u16,
) -> Result<SocketAddr, std::io::Error> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr().unwrap_or(addr);
    let app = coord_admin_router(store);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::violation_store::ViolationStoreConfig;
    use axum::http::StatusCode;

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

    #[tokio::test]
    async fn test_admin_server_status_and_actions() {
        let store = test_store();
        store.record_violation("agent-x", "r", "hook", "chan");
        let addr = match start_coord_admin_server(Arc::clone(&store), 0).await {
            Ok(addr) => addr,
            Err(err) => panic!("failed to start admin server: {err}"),
        };
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        let resp = match client.get(format!("{base}/coord/status")).send().await {
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

        let resp = match client
            .get(format!("{base}/coord/status/agent-x"))
            .send()
            .await
        {
            Ok(r) => r,
            Err(err) => panic!("GET /coord/status/agent-x failed: {err}"),
        };
        assert_eq!(resp.status(), StatusCode::OK);

        // Second strike disables (max_violations = 2).
        store.record_violation("agent-x", "r2", "hook", "chan");
        let resp = match client
            .get(format!("{base}/coord/status/agent-x"))
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
        let resp = match client
            .post(format!("{base}/coord/clear/agent-x"))
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
        let _ = client
            .post(format!("{base}/coord/disable/agent-y"))
            .send()
            .await;
        assert!(store.is_disabled("agent-y"));
        let _ = client
            .post(format!("{base}/coord/enable/agent-y"))
            .send()
            .await;
        assert!(!store.is_disabled("agent-y"));
    }
}
