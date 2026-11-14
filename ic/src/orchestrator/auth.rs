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
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// Granted credential values served to a job (WI#666).
    ///
    /// `/worker/{job}/credentials` and `/worker/{job}/mcp/credentials` decrypt
    /// secrets and hand them to the worker for child-process env injection.
    /// A child can `printenv` any of them into tool output, so the daemon
    /// backstop must scrub the same values it armed the harness secrets with.
    /// Held for the job's lifetime like `harness_secrets` (promotion scans and
    /// late events run after the token dies) and released only by
    /// [`TokenStore::clear_harness_secrets`] on terminal paths. Recorded only
    /// while the job's token is live, so a request that outlives the job
    /// cannot leave values behind.
    credential_values: Arc<RwLock<HashMap<Uuid, ServedCredentialValues>>>,
    /// Source of `ServedCredentialValues::generation`. Store-wide, so a
    /// generation is never used twice: an entry re-created after a clear
    /// cannot match a redactor built from the entry before it.
    credential_generations: Arc<AtomicU64>,
}

/// Credential values served to one job (WI#666).
///
/// `generation` takes a new store-wide number every time a value is added
/// (0 stands for "no entry"). Unlike harness secrets, these are stored at
/// request time, any number of times per job, so a redactor build can race a
/// pull: a redactor is cached only while the generation it was built from is
/// still current.
#[derive(Default)]
struct ServedCredentialValues {
    values: Vec<SecretString>,
    generation: u64,
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
            credential_values: Arc::new(RwLock::new(HashMap::new())),
            credential_generations: Arc::new(AtomicU64::new(0)),
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

    /// The job's redactor over its mounted secret values and served
    /// credential values (CV-7/CW-10, WI#666), built once and cached per job
    /// (EB-4) — rebuilding on every event would re-allocate the full variant
    /// set per request. Returns `None` when the job has neither.
    pub async fn harness_secret_redactor(
        &self,
        job_id: Uuid,
    ) -> Option<Arc<crate::orchestrator::harness::SecretValueRedactor>> {
        if let Some(redactor) = self.harness_redactors.read().await.get(&job_id) {
            return Some(Arc::clone(redactor));
        }
        let harness_values: Option<Vec<SecretString>> = self
            .harness_secrets
            .read()
            .await
            .get(&job_id)
            .map(|secrets| secrets.iter().map(|(_, value)| value.clone()).collect());
        let (credential_values, credential_generation) = self
            .credential_values
            .read()
            .await
            .get(&job_id)
            .map(|served| (served.values.clone(), served.generation))
            .unwrap_or_default();
        let redactor = build_redactor(&harness_values.unwrap_or_default(), &credential_values);
        if !redactor.is_armed() {
            return None;
        }
        self.cache_redactor_if_held(job_id, Arc::new(redactor), credential_generation)
            .await
    }

    /// Record credential values served to a job (WI#666).
    ///
    /// Called right after the secrets store decrypts a grant, so the
    /// daemon-side event/promotion scrubber covers values injected into the
    /// worker's child-process env. Repeated pulls extend the set (deduped); a
    /// pull that adds a value invalidates the cached redactor, a pull of
    /// known values keeps it. Like `harness_secrets`, these are not cleared
    /// by [`TokenStore::revoke`] — terminal paths call
    /// [`TokenStore::clear_harness_secrets`].
    ///
    /// Nothing is recorded for a job whose token is gone. The handlers decrypt
    /// between their auth check and this call, so the job can end in between.
    /// Every path that ends a job finishes with revoke, then clear
    /// (`cleanup_job` may clear earlier, while the token is still live, but
    /// the ending path's own clear follows it). The token guard is held
    /// across the insert here, so values are either stored before that
    /// revoke, and its clear releases them, or not stored at all. Without the
    /// check, values stored after the last clear would stay for the daemon's
    /// lifetime (the DG-3 class).
    pub async fn store_credential_values(&self, job_id: Uuid, values: Vec<SecretString>) {
        use secrecy::ExposeSecret;

        if values.is_empty() {
            return;
        }
        // Lock order tokens → credential values → redactors; no other path
        // takes `tokens` while it holds one of the later locks.
        let tokens = self.tokens.read().await;
        if !tokens.contains_key(&job_id) {
            return;
        }
        // Every guard is taken before anything changes, so the push, the new
        // generation and the cache drop below have no await between them: a
        // handler future dropped mid-call (its client gone) cannot leave a
        // cached redactor behind a generation that has moved. Holding the
        // values guard through the drop also means `cache_redactor_if_held`
        // (which holds the read guard through its insert) can never cache
        // against a generation that is about to move.
        let mut stored = self.credential_values.write().await;
        let mut redactors = self.harness_redactors.write().await;
        let entry = stored.entry(job_id).or_default();
        let mut added = false;
        for value in values {
            if !entry
                .values
                .iter()
                .any(|existing| existing.expose_secret() == value.expose_secret())
            {
                entry.values.push(value);
                added = true;
            }
        }
        if !added {
            return;
        }
        entry.generation = self.credential_generations.fetch_add(1, Ordering::Relaxed) + 1;
        redactors.remove(&job_id);
    }

    /// Insert a freshly built redactor only when the job's secrets are still
    /// held (EE-4): a `clear_harness_secrets` that raced the build must not
    /// be followed by a re-insert — nothing would ever remove that entry.
    /// Lock order secrets → redactors matches the clear path, so the
    /// re-check and the insert are atomic against a concurrent clear.
    ///
    /// For harness secrets the re-check covers PRESENCE, not generation: a
    /// build racing a re-store of different values would cache the older
    /// generation. That is unreachable today because their only production
    /// store runs once at dispatch and job ids are fresh UUIDs (a web
    /// restart creates a new id), so a build can never race a re-store for
    /// the same id.
    ///
    /// Served credential values have no such guarantee: both credential
    /// routes store at request time, and nothing but the order of calls in
    /// today's runtimes keeps a pull from racing an event. So for them the
    /// re-check covers the generation too. `credential_generation` is the
    /// generation the caller read with the values it built from; a redactor
    /// from an older generation is handed back to that one caller (its event
    /// raced the pull, and the worker cannot have echoed values it had not
    /// yet received) but never cached, so the next call builds from the
    /// current set.
    async fn cache_redactor_if_held(
        &self,
        job_id: Uuid,
        redactor: Arc<crate::orchestrator::harness::SecretValueRedactor>,
        credential_generation: u64,
    ) -> Option<Arc<crate::orchestrator::harness::SecretValueRedactor>> {
        // Both guards stay held through the insert: dropping them here would
        // let a concurrent terminal clear slip between the re-check and the
        // insert, leaving a redactor nothing ever removes. Lock order
        // secrets → credential values → redactors matches every writer.
        let secrets = self.harness_secrets.read().await;
        let holds_secrets = secrets.contains_key(&job_id);
        // WI#666: grants alone arm a redactor too, so a job with served
        // credential values but no harness secrets must still cache it.
        let credentials = self.credential_values.read().await;
        let served = credentials.get(&job_id);
        if !holds_secrets && served.is_none() {
            return None;
        }
        if served.map_or(0, |served| served.generation) != credential_generation {
            return Some(redactor);
        }
        let mut redactors = self.harness_redactors.write().await;
        if let Some(existing) = redactors.get(&job_id) {
            // A concurrent build won the insert. It was checked against the
            // same credential generation under the same guard, and a store
            // that moves the generation drops the entry before it releases
            // its write guard, so either Arc describes the held values.
            return Some(Arc::clone(existing));
        }
        redactors.insert(job_id, Arc::clone(&redactor));
        Some(redactor)
    }

    /// Release a job's mounted secret values and served credential values.
    /// Called on every terminal path (finalize after promotion, stop/cancel,
    /// crash watcher, cleanup).
    pub async fn clear_harness_secrets(&self, job_id: Uuid) {
        self.harness_secrets.write().await.remove(&job_id);
        self.harness_secrets_served.write().await.remove(&job_id);
        self.credential_values.write().await.remove(&job_id);
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

/// Build a job redactor over harness secret values plus credential values
/// served to the worker (WI#666). Harness values arm the initial variant set;
/// credential values are batched in with a single extra matcher rebuild.
fn build_redactor(
    harness_values: &[SecretString],
    credential_values: &[SecretString],
) -> crate::orchestrator::harness::SecretValueRedactor {
    use secrecy::ExposeSecret;

    let mut redactor = crate::orchestrator::harness::SecretValueRedactor::new(harness_values);
    redactor.add_values(credential_values.iter().map(|value| value.expose_secret()));
    redactor
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

    /// The generation of the credential values held for a job, if any.
    async fn served_generation(store: &TokenStore, job_id: Uuid) -> Option<u64> {
        store
            .credential_values
            .read()
            .await
            .get(&job_id)
            .map(|served| served.generation)
    }

    #[tokio::test]
    async fn credential_values_arm_the_daemon_redactor() {
        // WI#666: a job with granted credentials but no harness secrets must
        // still get a redactor from the served values, since those land in
        // the child process env.
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();
        let _token = store.create_token(job_id).await;
        let value = "granted-credential-fixture-value";
        assert!(store.harness_secret_redactor(job_id).await.is_none());

        store
            .store_credential_values(job_id, vec![SecretString::from(value.to_string())])
            .await;

        let redactor = store
            .harness_secret_redactor(job_id)
            .await
            .expect("credential values must arm the redactor"); // safety: test
        let scrubbed = redactor.scrub(&format!("printenv leaked {value}"));
        assert!(!scrubbed.contains(value));
        assert!(scrubbed.contains("[redacted-secret]"));

        // Like harness secrets, the values outlive the token: the write-back
        // scan runs after `revoke`.
        store.revoke(job_id).await;
        assert!(
            store
                .harness_secret_redactor(job_id)
                .await
                .is_some_and(|redactor| redactor.contains_secret(value))
        );

        // Terminal cleanup releases the values and the cached redactor.
        store.clear_harness_secrets(job_id).await;
        assert!(store.harness_secret_redactor(job_id).await.is_none());
        assert!(store.credential_values.read().await.get(&job_id).is_none());
    }

    #[tokio::test]
    async fn credential_values_are_not_recorded_for_an_ended_job() {
        // A credential request decrypts between its auth check and the
        // store. When the job ends in between (revoke, then clear), the late
        // store must record nothing: no later path would release it.
        let store = TokenStore::new();
        let value = "granted-credential-late-store-value";

        let stopped = Uuid::new_v4();
        let _token = store.create_token(stopped).await;
        store.revoke(stopped).await;
        store.clear_harness_secrets(stopped).await;
        // A job that never had a token is refused the same way.
        let unknown = Uuid::new_v4();

        for job_id in [stopped, unknown] {
            store
                .store_credential_values(job_id, vec![SecretString::from(value.to_string())])
                .await;
            assert!(
                store.credential_values.read().await.get(&job_id).is_none(),
                "values stored after the terminal clear would never be released"
            );
            assert!(store.harness_secret_redactor(job_id).await.is_none());
            assert!(store.harness_redactors.read().await.get(&job_id).is_none());
        }
    }

    #[tokio::test]
    async fn redactor_built_before_a_credential_pull_is_not_cached() {
        // The credential routes store at request time, so an event's redactor
        // build can race a pull. A build from the older set must not become
        // the job's cached redactor: every later event would be scrubbed
        // without the values the pull added.
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();
        let _token = store.create_token(job_id).await;
        let first = "granted-credential-first-value";
        let second = "granted-credential-second-value";
        store
            .store_credential_values(job_id, vec![SecretString::from(first.to_string())])
            .await;

        // The build read the set before the second pull...
        let older_generation = served_generation(&store, job_id)
            .await
            .expect("first pull is recorded"); // safety: test
        let older = Arc::new(build_redactor(
            &[],
            &[SecretString::from(first.to_string())],
        ));
        // ...and reaches the cache after it.
        store
            .store_credential_values(job_id, vec![SecretString::from(second.to_string())])
            .await;
        let returned = store
            .cache_redactor_if_held(job_id, Arc::clone(&older), older_generation)
            .await
            .expect("the job still holds values"); // safety: test
        assert!(Arc::ptr_eq(&returned, &older));
        assert!(
            store.harness_redactors.read().await.get(&job_id).is_none(),
            "a redactor from an older credential set must not be cached"
        );

        // The next call builds from the current set and caches that.
        let current = store
            .harness_secret_redactor(job_id)
            .await
            .expect("credential values arm the redactor"); // safety: test
        for value in [first, second] {
            assert!(current.contains_secret(value));
        }
        let again = store
            .harness_secret_redactor(job_id)
            .await
            .expect("cached redactor"); // safety: test
        assert!(Arc::ptr_eq(&current, &again));
    }

    #[tokio::test]
    async fn credential_generation_is_not_reused_after_an_early_clear() {
        // `cleanup_job` can clear while the token is still live (the job
        // tool's wait path sees `Stopped` before the ending path revokes). A
        // pull that lands after that clear re-creates the entry. A redactor
        // built from the entry before the clear must not match it.
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();
        let _token = store.create_token(job_id).await;
        let first = "granted-credential-first-value";
        let second = "granted-credential-second-value";
        store
            .store_credential_values(job_id, vec![SecretString::from(first.to_string())])
            .await;
        let before_clear = served_generation(&store, job_id)
            .await
            .expect("first pull is recorded"); // safety: test
        let older = Arc::new(build_redactor(
            &[],
            &[SecretString::from(first.to_string())],
        ));

        store.clear_harness_secrets(job_id).await;
        store
            .store_credential_values(job_id, vec![SecretString::from(second.to_string())])
            .await;
        let after_clear = served_generation(&store, job_id)
            .await
            .expect("the late pull is recorded"); // safety: test
        assert_ne!(before_clear, after_clear, "a generation must not be reused");

        let returned = store
            .cache_redactor_if_held(job_id, Arc::clone(&older), before_clear)
            .await
            .expect("the job holds values again"); // safety: test
        assert!(Arc::ptr_eq(&returned, &older));
        assert!(
            store.harness_redactors.read().await.get(&job_id).is_none(),
            "a redactor from before the clear must not be cached"
        );
    }

    #[tokio::test]
    async fn credential_values_extend_a_harness_armed_redactor_and_dedupe() {
        let store = TokenStore::new();
        let job_id = Uuid::new_v4();
        let _token = store.create_token(job_id).await;
        let harness_value = "harness-mounted-fixture-value";
        let credential_value = "granted-credential-fixture-value";
        store
            .store_harness_secrets(
                job_id,
                vec![(
                    "API_KEY".to_string(),
                    SecretString::from(harness_value.to_string()),
                )],
            )
            .await;

        // Arm, then serve credentials: the cached redactor must be rebuilt.
        assert!(store.harness_secret_redactor(job_id).await.is_some());
        for _ in 0..2 {
            store
                .store_credential_values(
                    job_id,
                    vec![SecretString::from(credential_value.to_string())],
                )
                .await;
        }

        let redactor = store
            .harness_secret_redactor(job_id)
            .await
            .expect("harness values keep a redactor armed"); // safety: test
        for value in [harness_value, credential_value] {
            assert!(
                !redactor.scrub(value).contains(value),
                "both channels must be scrubbed"
            );
        }
        // Repeated pulls extend without duplicating the stored value.
        {
            let stored = store.credential_values.read().await;
            assert_eq!(
                stored.get(&job_id).map(|served| served.values.len()),
                Some(1)
            );
        }
        // A pull that adds nothing keeps the cached redactor (EB-4).
        store
            .store_credential_values(
                job_id,
                vec![SecretString::from(credential_value.to_string())],
            )
            .await;
        let after_repeat = store
            .harness_secret_redactor(job_id)
            .await
            .expect("still armed"); // safety: test
        assert!(Arc::ptr_eq(&redactor, &after_repeat));
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
                .cache_redactor_if_held(job_id, Arc::clone(&redactor), 0)
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
                .cache_redactor_if_held(job_id, Arc::clone(&redactor), 0)
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
