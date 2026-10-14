//! Per-job bearer token authentication for worker-to-orchestrator communication.
//!
//! Security properties:
//! - Tokens are cryptographically random (32 bytes, hex-encoded)
//! - Tokens are scoped to a specific job_id
//! - Tokens are ephemeral (in-memory only, never persisted)
//! - A token for Job A cannot access endpoints for Job B
//! - Credential grants are per-job: only secrets explicitly granted are accessible

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use uuid::Uuid;

/// A credential grant that maps a secret (stored in SecretsStore) to an
/// environment variable name the container worker expects.
///
/// For example: `{ secret_name: "github_token", env_var: "GITHUB_TOKEN" }`
/// means "decrypt the secret named `github_token` and provide it as the
/// env var `GITHUB_TOKEN` to the container".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialGrant {
    pub secret_name: String,
    pub env_var: String,
}

/// Owner-scoped secret grant for one MCP server process environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCredentialGrant {
    pub server_name: String,
    pub secret_name: String,
    pub env_var: String,
}

/// Restart metadata for sandbox job capabilities.
///
/// New records use this object envelope. Deserialization also accepts the
/// legacy bare credential array, which grants no worker-local MCP servers.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SandboxJobGrants {
    pub credentials: Vec<CredentialGrant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SandboxJobGrantsWire {
    Current(SandboxJobGrantsFields),
    Legacy(Vec<CredentialGrant>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxJobGrantsFields {
    #[serde(default)]
    credentials: Vec<CredentialGrant>,
    #[serde(default)]
    mcp_servers: Vec<String>,
}

impl<'de> Deserialize<'de> for SandboxJobGrants {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match SandboxJobGrantsWire::deserialize(deserializer)? {
            SandboxJobGrantsWire::Current(fields) => Ok(Self {
                credentials: fields.credentials,
                mcp_servers: fields.mcp_servers,
            }),
            SandboxJobGrantsWire::Legacy(credentials) => Ok(Self {
                credentials,
                mcp_servers: Vec::new(),
            }),
        }
    }
}

/// Mounted harness secret values for one job: `(file_name, value)` pairs.
type HarnessSecrets = Vec<(String, SecretString)>;

/// In-memory store for per-job authentication tokens and credential grants.
#[derive(Clone)]
pub struct TokenStore {
    /// Maps job_id -> bearer token. Never logged or persisted.
    tokens: Arc<RwLock<HashMap<Uuid, String>>>,
    /// Maps job_id -> granted credentials. Revoked alongside the token.
    credential_grants: Arc<RwLock<HashMap<Uuid, Vec<CredentialGrant>>>>,
    /// MCP grants stay isolated from generic shell/child-process credentials.
    mcp_credential_grants: Arc<RwLock<HashMap<Uuid, Vec<McpCredentialGrant>>>>,
    /// Harness-mode secret values (WI#326).
    ///
    /// Resolved host-side at dispatch and held for the job's lifetime: the
    /// worker pulls them over its authenticated channel (CV-2), and the
    /// daemon reuses them for value-level event redaction (CV-7) and the
    /// write-back mounted-secret scan (CW-10). Deliberately NOT cleared by
    /// `revoke` — promotion runs after the token dies; every terminal path
    /// calls [`TokenStore::clear_harness_secrets`] explicitly.
    harness_secrets: Arc<RwLock<HashMap<Uuid, HarnessSecrets>>>,
    /// Jobs whose harness secrets were already served once (DG-7). The
    /// worker pulls exactly once at startup; any later pull is a replay and
    /// gets 410 Gone, so a token leaked mid-job cannot re-fetch values.
    harness_secrets_served: Arc<RwLock<std::collections::HashSet<Uuid>>>,
    /// Per-job redactor cache (EB-4): building a redactor costs one variant
    /// set per secret, so it is built once per job and reused by every
    /// event/complete scrub. Cleared alongside the secrets on terminal paths.
    harness_redactors:
        Arc<RwLock<HashMap<Uuid, Arc<crate::orchestrator::harness::SecretValueRedactor>>>>,
}

impl TokenStore {
    pub fn new() -> Self {
        Self {
            tokens: Arc::new(RwLock::new(HashMap::new())),
            credential_grants: Arc::new(RwLock::new(HashMap::new())),
            mcp_credential_grants: Arc::new(RwLock::new(HashMap::new())),
            harness_secrets: Arc::new(RwLock::new(HashMap::new())),
            harness_secrets_served: Arc::new(RwLock::new(std::collections::HashSet::new())),
            harness_redactors: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Generate and store a new token for a job.
    pub async fn create_token(&self, job_id: Uuid) -> String {
        let token = generate_token();
        self.tokens.write().await.insert(job_id, token.clone());
        token
    }

    /// Validate a token for a specific job (constant-time comparison).
    pub async fn validate(&self, job_id: Uuid, token: &str) -> bool {
        self.tokens
            .read()
            .await
            .get(&job_id)
            .map(|stored| stored.as_bytes().ct_eq(token.as_bytes()).into())
            .unwrap_or(false)
    }

    /// Remove a token and its credential grants (on container cleanup).
    pub async fn revoke(&self, job_id: Uuid) {
        self.tokens.write().await.remove(&job_id);
        self.credential_grants.write().await.remove(&job_id);
        self.mcp_credential_grants.write().await.remove(&job_id);
    }

    /// Get the number of active tokens (for diagnostics).
    pub async fn active_count(&self) -> usize {
        self.tokens.read().await.len()
    }

    /// Store credential grants for a job. Call right after `create_token()`.
    pub async fn store_grants(&self, job_id: Uuid, grants: Vec<CredentialGrant>) {
        if !grants.is_empty() {
            self.credential_grants.write().await.insert(job_id, grants);
        }
    }

    /// Retrieve credential grants for a job.
    pub async fn get_grants(&self, job_id: Uuid) -> Option<Vec<CredentialGrant>> {
        self.credential_grants.read().await.get(&job_id).cloned()
    }

    pub async fn store_mcp_grants(&self, job_id: Uuid, grants: Vec<McpCredentialGrant>) {
        let mut stored = self.mcp_credential_grants.write().await;
        if grants.is_empty() {
            stored.remove(&job_id);
        } else {
            stored.insert(job_id, grants);
        }
    }

    pub async fn get_mcp_grants(&self, job_id: Uuid) -> Option<Vec<McpCredentialGrant>> {
        self.mcp_credential_grants
            .read()
            .await
            .get(&job_id)
            .cloned()
    }

    /// Store harness-mode secret values for a job (WI#326). Call right after
    /// `create_token()`, before the container is created.
    pub async fn store_harness_secrets(&self, job_id: Uuid, secrets: HarnessSecrets) {
        let mut stored = self.harness_secrets.write().await;
        if secrets.is_empty() {
            stored.remove(&job_id);
        } else {
            stored.insert(job_id, secrets);
        }
        // A re-store invalidates any redactor built from the older values.
        self.harness_redactors.write().await.remove(&job_id);
    }

    /// Retrieve the mounted file names and values for a job (worker pull).
    pub async fn harness_secrets(&self, job_id: Uuid) -> Option<HarnessSecrets> {
        self.harness_secrets.read().await.get(&job_id).cloned()
    }

    /// The job's redactor over its mounted secret values (CV-7/CW-10), built
    /// once and cached per job (EB-4) — rebuilding on every event would
    /// re-allocate the full variant set per request. Returns `None` when the
    /// job has no harness secrets.
    pub async fn harness_secret_redactor(
        &self,
        job_id: Uuid,
    ) -> Option<Arc<crate::orchestrator::harness::SecretValueRedactor>> {
        if let Some(redactor) = self.harness_redactors.read().await.get(&job_id) {
            return Some(Arc::clone(redactor));
        }
        let values: Vec<SecretString> = self
            .harness_secrets
            .read()
            .await
            .get(&job_id)?
            .iter()
            .map(|(_, value)| value.clone())
            .collect();
        let redactor = crate::orchestrator::harness::SecretValueRedactor::new(&values);
        if !redactor.is_armed() {
            return None;
        }
        self.cache_redactor_if_held(job_id, Arc::new(redactor))
            .await
    }

    /// Insert a freshly built redactor only when the job's secrets are still
    /// held (EE-4): a `clear_harness_secrets` that raced the build must not
    /// be followed by a re-insert — nothing would ever remove that entry.
    /// Lock order secrets → redactors matches the clear path, so the
    /// re-check and the insert are atomic against a concurrent clear.
    ///
    /// The re-check covers PRESENCE, not generation: a build racing a
    /// re-store of different values would cache the older generation. That
    /// is unreachable today because the only production store runs once at
    /// dispatch and job ids are fresh UUIDs (a web restart creates a new
    /// id), so a build can never race a re-store for the same id.
    async fn cache_redactor_if_held(
        &self,
        job_id: Uuid,
        redactor: Arc<crate::orchestrator::harness::SecretValueRedactor>,
    ) -> Option<Arc<crate::orchestrator::harness::SecretValueRedactor>> {
        let secrets = self.harness_secrets.read().await;
        if !secrets.contains_key(&job_id) {
            return None;
        }
        let mut redactors = self.harness_redactors.write().await;
        if let Some(existing) = redactors.get(&job_id) {
            // A concurrent build won the insert; both describe the same
            // held values, so either Arc is correct.
            return Some(Arc::clone(existing));
        }
        redactors.insert(job_id, Arc::clone(&redactor));
        Some(redactor)
    }

    /// Release a job's mounted secret values. Called on every terminal path
    /// (finalize after promotion, stop/cancel, crash watcher, cleanup).
    pub async fn clear_harness_secrets(&self, job_id: Uuid) {
        self.harness_secrets.write().await.remove(&job_id);
        self.harness_secrets_served.write().await.remove(&job_id);
        self.harness_redactors.write().await.remove(&job_id);
    }

    /// Mark the job's harness secrets as served (DG-7 one-shot delivery).
    /// Returns false when they were already served — the caller must refuse
    /// the replay with 410 Gone.
    pub async fn mark_harness_secrets_served(&self, job_id: Uuid) -> bool {
        self.harness_secrets_served.write().await.insert(job_id)
    }
}

impl Default for TokenStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate a cryptographically random token (32 bytes, hex-encoded = 64 chars).
fn generate_token() -> String {
    use rand::RngCore;
    use rand::rngs::OsRng;
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    // Hex-encode without pulling in a crate: fixed-size array, no allocation concern.
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Axum middleware that validates worker bearer tokens.
///
/// Extracts the job_id from the path (`/worker/{job_id}/...`) and validates
/// the `Authorization: Bearer <token>` header against the token store.
///
/// Wire up with `axum::middleware::from_fn_with_state(token_store, worker_auth_middleware)`.
pub async fn worker_auth_middleware(
    State(token_store): State<TokenStore>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = request.uri().path().to_string();
    let job_id = extract_job_id_from_path(&path).ok_or(StatusCode::BAD_REQUEST)?;

    let token = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if !token_store.validate(job_id, token).await {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(request).await)
}

/// Extract job UUID from a path like `/worker/{uuid}/...`
fn extract_job_id_from_path(path: &str) -> Option<Uuid> {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() >= 2 && parts[0] == "worker" {
        Uuid::parse_str(parts[1]).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_token_create_and_validate() {
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();

        let token = store.create_token(job_id).await;
        assert_eq!(token.len(), 64); // 32 bytes hex = 64 chars

        assert!(store.validate(job_id, &token).await);
        assert!(!store.validate(job_id, "wrong-token").await);
        assert!(!store.validate(Uuid::new_v4(), &token).await);
    }

    #[tokio::test]
    async fn test_token_revoke() {
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();

        let token = store.create_token(job_id).await;
        assert!(store.validate(job_id, &token).await);

        store.revoke(job_id).await;
        assert!(!store.validate(job_id, &token).await);
    }

    #[test]
    fn test_extract_job_id() {
        let id = Uuid::new_v4();
        let path = format!("/worker/{}/llm/complete", id);
        assert_eq!(extract_job_id_from_path(&path), Some(id));

        assert_eq!(extract_job_id_from_path("/other/path"), None);
        assert_eq!(extract_job_id_from_path("/worker/not-a-uuid/foo"), None);
    }

    #[test]
    fn test_token_is_random() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
    }

    #[tokio::test]
    async fn test_store_and_get_grants() {
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();

        // No grants initially
        assert!(store.get_grants(job_id).await.is_none());

        let grants = vec![
            CredentialGrant {
                secret_name: "github_token".to_string(),
                env_var: "GITHUB_TOKEN".to_string(),
            },
            CredentialGrant {
                secret_name: "npm_token".to_string(),
                env_var: "NPM_TOKEN".to_string(),
            },
        ];

        store.store_grants(job_id, grants).await;

        let retrieved = store.get_grants(job_id).await.unwrap();
        assert_eq!(retrieved.len(), 2);
        assert_eq!(retrieved[0].secret_name, "github_token");
        assert_eq!(retrieved[0].env_var, "GITHUB_TOKEN");
        assert_eq!(retrieved[1].secret_name, "npm_token");
    }

    #[test]
    fn sandbox_job_grants_decode_legacy_array_without_mcp_access() {
        let grants: SandboxJobGrants =
            serde_json::from_str(r#"[{"secret_name":"github_token","env_var":"GITHUB_TOKEN"}]"#)
                .unwrap();

        assert_eq!(grants.credentials.len(), 1);
        assert!(grants.mcp_servers.is_empty());
    }

    #[test]
    fn sandbox_job_grants_round_trip_explicit_mcp_selection() {
        let grants = SandboxJobGrants {
            credentials: Vec::new(),
            mcp_servers: vec!["project-tools".to_string()],
        };
        let json = serde_json::to_string(&grants).unwrap();
        let decoded: SandboxJobGrants = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.mcp_servers, ["project-tools"]);
    }

    #[test]
    fn sandbox_job_grants_reject_unknown_fields() {
        assert!(serde_json::from_str::<SandboxJobGrants>(r#"{"mcp_server":["typo"]}"#).is_err());
    }

    #[tokio::test]
    async fn test_revoke_clears_grants() {
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();

        let _token = store.create_token(job_id).await;
        store
            .store_grants(
                job_id,
                vec![CredentialGrant {
                    secret_name: "my_secret".to_string(),
                    env_var: "MY_SECRET".to_string(),
                }],
            )
            .await;

        assert!(store.get_grants(job_id).await.is_some());

        store.revoke(job_id).await;

        assert!(!store.validate(job_id, "anything").await);
        assert!(store.get_grants(job_id).await.is_none());
    }

    #[tokio::test]
    async fn test_empty_grants_not_stored() {
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();

        store.store_grants(job_id, vec![]).await;

        // Empty vec should not create an entry
        assert!(store.get_grants(job_id).await.is_none());
    }

    #[tokio::test]
    async fn test_grants_isolated_per_job() {
        let store = TokenStore::new();
        let job_a = Uuid::new_v4();
        let job_b = Uuid::new_v4();

        store
            .store_grants(
                job_a,
                vec![CredentialGrant {
                    secret_name: "secret_a".to_string(),
                    env_var: "SECRET_A".to_string(),
                }],
            )
            .await;

        store
            .store_grants(
                job_b,
                vec![CredentialGrant {
                    secret_name: "secret_b".to_string(),
                    env_var: "SECRET_B".to_string(),
                }],
            )
            .await;

        let grants_a = store.get_grants(job_a).await.unwrap();
        assert_eq!(grants_a.len(), 1);
        assert_eq!(grants_a[0].secret_name, "secret_a");

        let grants_b = store.get_grants(job_b).await.unwrap();
        assert_eq!(grants_b.len(), 1);
        assert_eq!(grants_b[0].secret_name, "secret_b");
    }

    #[tokio::test]
    async fn mcp_grants_are_isolated_and_revoked() {
        let store = TokenStore::new();
        let job_a = Uuid::new_v4();
        let job_b = Uuid::new_v4();
        store
            .store_mcp_grants(
                job_a,
                vec![McpCredentialGrant {
                    server_name: "alpha".into(),
                    secret_name: "token_a".into(),
                    env_var: "TOKEN".into(),
                }],
            )
            .await;
        assert!(store.get_mcp_grants(job_b).await.is_none());
        assert_eq!(
            store.get_mcp_grants(job_a).await.unwrap()[0].server_name,
            "alpha"
        );
        store.revoke(job_a).await;
        assert!(store.get_mcp_grants(job_a).await.is_none());
    }

    #[tokio::test]
    async fn harness_secrets_survive_revoke_until_explicit_clear() {
        // WI#326: promotion runs after token revocation, so values outlive
        // `revoke` but are dropped by `clear_harness_secrets`.
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();
        let _token = store.create_token(job_id).await;
        store
            .store_harness_secrets(
                job_id,
                vec![(
                    "API_KEY".to_string(),
                    SecretString::from("value-that-is-long-enough".to_string()),
                )],
            )
            .await;

        store.revoke(job_id).await;
        assert!(
            store.harness_secrets(job_id).await.is_some(),
            "revoke must not clear harness secrets (promotion still needs them)"
        );
        let redactor = store.harness_secret_redactor(job_id).await;
        assert!(redactor.is_some_and(|r| r.is_armed()));

        store.clear_harness_secrets(job_id).await;
        assert!(store.harness_secrets(job_id).await.is_none());
        assert!(store.harness_secret_redactor(job_id).await.is_none());
    }

    #[tokio::test]
    async fn harness_redactor_is_cached_per_job_and_cleared_with_secrets() {
        // EB-4: the redactor is built once per job, not per request.
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();
        store
            .store_harness_secrets(
                job_id,
                vec![(
                    "API_KEY".to_string(),
                    SecretString::from("value-that-is-long-enough".to_string()),
                )],
            )
            .await;

        let first = store.harness_secret_redactor(job_id).await.unwrap();
        let second = store.harness_secret_redactor(job_id).await.unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "repeat calls must reuse the cached redactor"
        );

        // A re-store invalidates the cache (new values, new redactor).
        store
            .store_harness_secrets(
                job_id,
                vec![(
                    "API_KEY".to_string(),
                    SecretString::from("another-value-that-is-long".to_string()),
                )],
            )
            .await;
        let third = store.harness_secret_redactor(job_id).await.unwrap();
        assert!(!Arc::ptr_eq(&first, &third));

        store.clear_harness_secrets(job_id).await;
        assert!(store.harness_secret_redactor(job_id).await.is_none());
    }

    #[tokio::test]
    async fn harness_redactor_insert_is_skipped_when_secrets_were_cleared() {
        // EE-4: a redactor built while `clear_harness_secrets` ran must NOT
        // be inserted — nothing would ever remove that entry, leaving a full
        // secret-derived variant set alive for an ended job.
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();
        let redactor = Arc::new(crate::orchestrator::harness::SecretValueRedactor::new(
            [SecretString::from("value-that-is-long-enough".to_string())].as_slice(),
        ));
        // No secrets held for the job (as after a racing clear): refused.
        assert!(
            store
                .cache_redactor_if_held(job_id, Arc::clone(&redactor))
                .await
                .is_none()
        );
        assert!(store.harness_redactors.read().await.get(&job_id).is_none());

        // Secrets held: the insert lands and is reused.
        store
            .store_harness_secrets(
                job_id,
                vec![(
                    "API_KEY".to_string(),
                    SecretString::from("value-that-is-long-enough".to_string()),
                )],
            )
            .await;
        assert!(
            store
                .cache_redactor_if_held(job_id, Arc::clone(&redactor))
                .await
                .is_some()
        );
        assert!(store.harness_redactors.read().await.get(&job_id).is_some());

        // Quiescent-state sweep: once everything settles, the cache never
        // outlives the held secrets. This is an invariant check, NOT a race
        // pin: `#[tokio::test]` runs current-thread and `clear_harness_secrets`
        // holds no guard across an await, so the spawned builder always runs
        // atomically between the clear's statements — the loop cannot fail on
        // a pre-EE-4 direct insert. The race closure rests on the lock-order
        // argument in `cache_redactor_if_held` and the two direct calls above.
        for _ in 0..64 {
            let store = Arc::new(TokenStore::new());
            let job_id = Uuid::new_v4();
            store
                .store_harness_secrets(
                    job_id,
                    vec![(
                        "API_KEY".to_string(),
                        SecretString::from("value-that-is-long-enough".to_string()),
                    )],
                )
                .await;
            let builder = tokio::spawn({
                let store = Arc::clone(&store);
                async move { store.harness_secret_redactor(job_id).await }
            });
            store.clear_harness_secrets(job_id).await;
            let _ = builder.await;
            assert!(
                store.harness_redactors.read().await.get(&job_id).is_none(),
                "a cleared job must never retain a cached redactor"
            );
        }
    }
}
