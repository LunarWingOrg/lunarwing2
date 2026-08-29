//! SSH Bridge API — HTTP endpoints for SSH host and key management.

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{Next, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use tracing::{info, instrument, warn};

use crate::bridge::ssh::{
    HostKeyMode, SSHBridge, SSHCredentials, SSHHostConfig, SSHKeyType, SshBridgeError,
};
use crate::bridge::ssh_agent::SshAgentServer;
use crate::bridge::ssh_secrets::SshSecretsManager;
use crate::secrets::SecretError;

/// Optional bearer token guarding the SSH API routes.
///
/// Mounted on the shared WebhookServer, which is reachable from any interface
/// when `HTTP_HOST` is non-loopback; the SSH fragment is the only fragment
/// there that must not be reachable without authorization, so it carries its
/// own bearer fence. When the token is unset the API is simply not served
/// (fail-closed).
const SSH_API_TOKEN_ENV: &str = "SSH_API_TOKEN";

pub struct SshApiState {
    pub bridge: Arc<RwLock<SSHBridge>>,
    pub secrets: Arc<SshSecretsManager>,
    pub agent: Arc<RwLock<Option<Arc<SshAgentServer>>>>,
    pub auth_token: Option<SecretString>,
}

/// Reject any request whose `Authorization: Bearer` header does not match the
/// configured token with a constant-time comparison.
async fn ssh_api_auth(
    State(state): State<Arc<SshApiState>>,
    request: Request,
    next: Next,
) -> Response {
    let provided = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|value| value.to_string());

    let matches = match (&state.auth_token, &provided) {
        (Some(expected), Some(actual)) => {
            bool::from(expected.expose_secret().as_bytes().ct_eq(actual.as_bytes()))
        }
        _ => false,
    };

    if matches {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}

pub fn create_router(state: Arc<SshApiState>) -> Router {
    let routes = Router::new()
        .route("/hosts", get(list_hosts).post(add_host))
        .route("/hosts/{host}", get(get_host).delete(remove_host))
        .route("/hosts/{host}/key", post(upload_key).delete(delete_key))
        .route("/hosts/{host}/key/status", get(key_status))
        .route("/agent/status", get(agent_status))
        .route("/agent/keys", get(agent_keys));

    match &state.auth_token {
        Some(token) if !token.expose_secret().is_empty() => routes
            .route_layer(from_fn_with_state(state.clone(), ssh_api_auth))
            .with_state(state),
        None | Some(_) => {
            warn!(
                "SSH API routes disabled: {} is not set or empty; SSH host/key management will 404",
                SSH_API_TOKEN_ENV
            );
            Router::new()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostRequest {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub key_type: SSHKeyType,
    #[serde(default)]
    pub host_key_mode: HostKeyMode,
    #[serde(default)]
    pub known_host_key: Option<String>,
}

fn default_port() -> u16 {
    22
}

impl From<HostRequest> for SSHHostConfig {
    fn from(req: HostRequest) -> Self {
        SSHHostConfig {
            host: req.host,
            port: req.port,
            user: req.user,
            key_type: req.key_type,
            host_key_mode: req.host_key_mode,
            known_host_key: req.known_host_key,
            connect_timeout_secs: 10,
            operation_timeout_secs: 30,
            keepalive_interval_secs: 60,
            keepalive_max_misses: 3,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct KeyUploadRequest {
    pub key_data: String,
    #[serde(default)]
    pub passphrase: Option<String>,
}

// Manual Debug: the derived impl would render the raw private key and
// passphrase into any span or log that formats the request. Nothing in
// this struct is safe to print.
impl std::fmt::Debug for KeyUploadRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyUploadRequest")
            .field("key_data", &"[REDACTED]")
            .field("passphrase", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostResponse {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_type: String,
    pub host_key_mode: String,
    pub has_key: bool,
}

impl From<SSHHostConfig> for HostResponse {
    fn from(config: SSHHostConfig) -> Self {
        Self {
            host: config.host,
            port: config.port,
            user: config.user,
            key_type: config.key_type.to_string(),
            host_key_mode: config.host_key_mode.to_string(),
            has_key: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatusResponse {
    pub running: bool,
    pub socket_path: Option<String>,
    pub keys_loaded: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }
    pub fn error(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}

#[instrument(skip(state))]
async fn list_hosts(
    State(state): State<Arc<SshApiState>>,
) -> Result<Json<ApiResponse<Vec<HostResponse>>>, ApiError> {
    let bridge = state.bridge.read().await;
    let hosts = bridge.list_hosts().await;
    let mut responses = Vec::new();
    for config in hosts {
        let has_key = state.secrets.key_exists(&config.host).await?;
        responses.push(HostResponse {
            host: config.host,
            port: config.port,
            user: config.user,
            key_type: config.key_type.to_string(),
            host_key_mode: config.host_key_mode.to_string(),
            has_key,
        });
    }
    Ok(Json(ApiResponse::success(responses)))
}

#[instrument(skip(state))]
async fn get_host(
    State(state): State<Arc<SshApiState>>,
    Path(host): Path<String>,
) -> Result<Json<ApiResponse<HostResponse>>, ApiError> {
    let bridge = state.bridge.read().await;
    let config = bridge.get_host_config(&host).await?;
    let has_key = state.secrets.key_exists(&host).await?;
    Ok(Json(ApiResponse::success(HostResponse {
        host: config.host,
        port: config.port,
        user: config.user,
        key_type: config.key_type.to_string(),
        host_key_mode: config.host_key_mode.to_string(),
        has_key,
    })))
}

#[instrument(skip(state))]
async fn add_host(
    State(state): State<Arc<SshApiState>>,
    Json(request): Json<HostRequest>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let config = SSHHostConfig::from(request);
    let host = config.host.clone();
    let bridge = state.bridge.write().await;
    bridge.add_host(config).await?;
    info!("Added host: {}", host);
    Ok(Json(ApiResponse::success(format!("Host {} added", host))))
}

#[instrument(skip(state))]
async fn remove_host(
    State(state): State<Arc<SshApiState>>,
    Path(host): Path<String>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let bridge = state.bridge.write().await;
    bridge.remove_host(&host).await?;
    info!("Removed host: {}", host);
    Ok(Json(ApiResponse::success(format!("Host {} removed", host))))
}

#[instrument(skip_all, fields(host = %host))]
async fn upload_key(
    State(state): State<Arc<SshApiState>>,
    Path(host): Path<String>,
    Json(request): Json<KeyUploadRequest>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let key_data = request.key_data.as_bytes().to_vec();
    let passphrase = request.passphrase;

    state
        .secrets
        .store_key(&host, &key_data, passphrase.as_deref())
        .await?;

    {
        let agent_guard = state.agent.read().await;
        if let Some(agent) = agent_guard.as_ref() {
            let creds = SSHCredentials {
                key_data: key_data.clone().into(),
                passphrase: passphrase.map(|s| SecretString::new(s.into_boxed_str())),
            };
            agent.add_key(host.clone(), creds).await?;
        }
    }

    info!("Uploaded key for host: {}", host);
    Ok(Json(ApiResponse::success(format!(
        "Key uploaded for {}",
        host
    ))))
}

#[instrument(skip(state))]
async fn delete_key(
    State(state): State<Arc<SshApiState>>,
    Path(host): Path<String>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    state.secrets.delete_key(&host).await?;
    {
        let agent_guard = state.agent.read().await;
        if let Some(agent) = agent_guard.as_ref() {
            agent.remove_key(&host).await?;
        }
    }
    info!("Deleted key for host: {}", host);
    Ok(Json(ApiResponse::success(format!(
        "Key deleted for {}",
        host
    ))))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyStatusResponse {
    pub host: String,
    pub exists: bool,
}

#[instrument(skip(state))]
async fn key_status(
    State(state): State<Arc<SshApiState>>,
    Path(host): Path<String>,
) -> Result<Json<ApiResponse<KeyStatusResponse>>, ApiError> {
    let exists = state.secrets.key_exists(&host).await?;
    Ok(Json(ApiResponse::success(KeyStatusResponse {
        host,
        exists,
    })))
}

#[instrument(skip(state))]
async fn agent_status(
    State(state): State<Arc<SshApiState>>,
) -> Result<Json<ApiResponse<AgentStatusResponse>>, ApiError> {
    let agent_guard = state.agent.read().await;
    let response = if let Some(agent) = agent_guard.as_ref() {
        let keys = agent.list_keys().await;
        AgentStatusResponse {
            running: true,
            socket_path: Some(agent.socket_path().to_string_lossy().to_string()),
            keys_loaded: keys.len(),
        }
    } else {
        AgentStatusResponse {
            running: false,
            socket_path: None,
            keys_loaded: 0,
        }
    };
    Ok(Json(ApiResponse::success(response)))
}

#[instrument(skip(state))]
async fn agent_keys(
    State(state): State<Arc<SshApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, ApiError> {
    let agent_guard = state.agent.read().await;
    let keys = if let Some(agent) = agent_guard.as_ref() {
        agent.list_keys().await
    } else {
        Vec::new()
    };
    Ok(Json(ApiResponse::success(keys)))
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ApiError {}
impl From<SshBridgeError> for ApiError {
    fn from(err: SshBridgeError) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: err.to_string(),
        }
    }
}
impl From<SecretError> for ApiError {
    fn from(err: SecretError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "success": false, "error": self.message });
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::ssh::NullAuditLogger;
    use crate::secrets::{InMemorySecretsStore, SecretsCrypto};
    use secrecy::SecretString as MasterKey;
    use std::collections::HashMap;
    use uuid::Uuid;

    #[test]
    fn test_host_request_conversion() {
        let request = HostRequest {
            host: "example.com".into(),
            port: 22,
            user: "admin".into(),
            key_type: SSHKeyType::Ed25519,
            host_key_mode: HostKeyMode::Strict,
            known_host_key: None,
        };
        let config: SSHHostConfig = request.into();
        assert_eq!(config.host, "example.com");
    }

    #[test]
    fn key_upload_request_debug_is_redacted() {
        // Built by concatenation so gitleaks' private-key rule never matches
        // the literal marker in source (no .gitleaksignore line churn).
        let key_marker = format!("-----BEGIN OPENSSH PRIVATE {}-----", "KEY");
        let request = KeyUploadRequest {
            key_data: format!("{key_marker}\nsecret\n"),
            passphrase: Some("p4ssphr4se-marker".into()),
        };
        let rendered = format!("{request:?}");
        assert!(!rendered.contains(&key_marker));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("p4ssphr4se-marker"));
        assert!(rendered.contains("[REDACTED]"));
    }

    async fn make_test_state() -> SshApiState {
        make_test_state_with_token(None).await
    }

    async fn make_test_state_with_token(token: Option<&str>) -> SshApiState {
        let store: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(InMemorySecretsStore::new(Arc::new(
                SecretsCrypto::new(MasterKey::from(
                    "test-master-key-that-is-at-least-32-bytes-long!",
                ))
                .unwrap(),
            )));
        let secrets = Arc::new(SshSecretsManager::new(store.clone(), "test-tenant"));
        let bridge = SSHBridge::new(
            Uuid::new_v4(),
            "test-tenant".into(),
            HashMap::new(),
            store,
            Arc::new(NullAuditLogger),
        )
        .await;
        SshApiState {
            bridge: Arc::new(RwLock::new(bridge.unwrap())),
            secrets,
            agent: Arc::new(RwLock::new(None)),
            auth_token: token.map(|t| SecretString::new(t.to_string().into_boxed_str())),
        }
    }

    async fn serve_router(app: axum::Router) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        addr
    }

    #[tokio::test]
    async fn ssh_api_requires_bearer_token() {
        let state = Arc::new(make_test_state_with_token(Some("test-api-token")).await);
        let addr = serve_router(create_router(state)).await;

        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        let no_auth = client
            .get(format!("{base}/agent/keys"))
            .send()
            .await
            .unwrap();
        assert_eq!(no_auth.status(), 401, "missing bearer must be rejected");

        let bad_auth = client
            .get(format!("{base}/agent/keys"))
            .header("Authorization", "Bearer wrong-token")
            .send()
            .await
            .unwrap();
        assert_eq!(bad_auth.status(), 401, "wrong bearer must be rejected");

        let good_auth = client
            .get(format!("{base}/agent/keys"))
            .header("Authorization", "Bearer test-api-token")
            .send()
            .await
            .unwrap();
        assert_eq!(good_auth.status(), 200, "correct bearer must pass");
    }

    #[tokio::test]
    async fn ssh_api_unmounted_without_token() {
        let state = Arc::new(make_test_state_with_token(None).await);
        let app = create_router(state);
        let addr = serve_router(app).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/agent/keys"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            404,
            "no SSH_API_TOKEN configured -> SSH API must fail closed (not served)"
        );
    }

    #[tokio::test]
    async fn ssh_api_unmounted_with_empty_token() {
        let state = Arc::new(make_test_state_with_token(Some("")).await);
        let addr = serve_router(create_router(state)).await;

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/agent/keys"))
            .header("Authorization", "Bearer ")
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            404,
            "empty SSH_API_TOKEN must leave SSH API unmounted"
        );
    }

    #[tokio::test]
    #[tracing_test::traced_test]
    async fn upload_key_span_and_logs_keep_key_material_out() {
        let state = make_test_state().await;
        // Concatenation keeps the literal PEM marker out of source so gitleaks
        // never flags this file (no .gitleaksignore line churn).
        let key_marker = format!("-----BEGIN OPENSSH PRIVATE {}-----", "KEY");
        let request = KeyUploadRequest {
            key_data: format!("{key_marker}\nleak-marker\n"),
            passphrase: Some("p4ssphr4se-marker".into()),
        };

        let response = upload_key(
            State(Arc::new(state)),
            Path("example.com".to_string()),
            Json(request),
        )
        .await
        .expect("upload should succeed");
        assert!(response.0.success);

        assert!(
            !logs_contain("BEGIN OPENSSH PRIVATE KEY"),
            "private key material leaked into tracing output"
        );
        assert!(
            !logs_contain("leak-marker"),
            "private key body leaked into tracing output"
        );
        assert!(
            !logs_contain("p4ssphr4se-marker"),
            "passphrase leaked into tracing output"
        );
        assert!(logs_contain("example.com"), "host field should be recorded");
    }
}
