# SSH Key API Exposure — Log Leak, Unauthenticated Router, and Operations

**Status:** Both findings fixed (log leak: `09b0f507`; router fence: `8c323c43`; CI allowlist: `d89ceff8`)
**Severity:** HIGH (secret retention in logs) + MEDIUM (unauthenticated key management)
**Found by:** goal-26 systemd stress rerun (`aj/kimi-26-20260828-2023`, finding F2) and wrench's post-fix review (finding #2)

## TL;DR

Two related defects on the tenant SSH key-management surface:

1. **Log leak (HIGH)** — uploading an SSH key through the gateway API wrote the
   **raw private key and optional passphrase into logs** on any host that used
   the feature. Fixed; **any host that already performed a key upload must
   rotate those keys and clean its log sinks** — no code change undoes what
   journald or a log file already retained.
2. **Unauthenticated router (MEDIUM)** — the SSH API fragment on the shared
   WebhookServer carried no authentication, so anyone who could reach the
   webhook port could enumerate hosts, register arbitrary ones, overwrite
   stored keys (hot-loaded into the agent), or delete them. Fixed: all SSH API
   routes now require a bearer token, and the webhook server no longer
   defaults to a non-loopback bind.

## What happened

The `POST /hosts/{host}/key` handler (`ic/src/bridge/ssh_api.rs`) received the
key in a request DTO that derived `Debug` with plaintext fields, and its
tracing span did not skip that request. Tracing rendered the span — including
the full key — into every configured sink.

Affected sinks (all init systems — the leak is in shared daemon code, not the
init layer):

- **systemd**: the user journal (`journalctl --user`)
- **OpenRC**: the daemon's log file under the LunarWing base dir and OpenRC's
  service output capture
- **Both**: the web gateway's browser log stream (`ic/src/channels/web/log_layer.rs`)
  over SSE — its leak detector scrubs `api_key`-shaped values, not multi-line
  PEM blocks

Example of what a journal line contained (redacted here):

```
... INFO bridge::ssh_api: Uploaded key for host: example.com
    in bridge::ssh_api::upload_key[span-id]
    request=Json(KeyUploadRequest { key_data: "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA...", passphrase: Some("hunter2") })
    host=example.com
```

## Root cause (before)

```rust
// ic/src/bridge/ssh_api.rs
#[derive(Debug, Clone, Serialize, Deserialize)]          // <-- Debug renders secrets
pub struct KeyUploadRequest {
    pub key_data: String,                                // raw PEM, plaintext
    #[serde(default)]
    pub passphrase: Option<String>,                      // plaintext
}

#[instrument(skip(state))]                               // <-- request NOT skipped
async fn upload_key(
    State(state): State<Arc<SshApiState>>,
    Path(host): Path<String>,
    Json(request): Json<KeyUploadRequest>,
) -> Result<Json<ApiResponse<String>>, ApiError> { ... }
```

Two defects, either sufficient on its own:

1. `KeyUploadRequest` derived `Debug` over plaintext secret fields, so any
   formatted log/span of the request printed the key verbatim.
2. `#[instrument(skip(state))]` left `request` riding the tracing span; child
   events inherit and render span fields.

Encryption at rest (the secrets store encrypts key material) does not help
once the credential is already retained in journald or a central log sink.

## The fix (after)

```rust
// Manual Debug: nothing in this struct is safe to print.
impl std::fmt::Debug for KeyUploadRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyUploadRequest")
            .field("key_data", &"[REDACTED]")
            .field("passphrase", &"[REDACTED]")
            .finish()
    }
}

#[instrument(skip_all, fields(host = %host))]            // request gone; host stays
async fn upload_key(/* ... unchanged signature ... */) { ... }
```

Defense in depth: reverting either layer alone does not reopen the leak — the
redacted `Debug` still blocks rendering if the span regresses, and the span no
longer carries the request at all.

## Regression coverage

`ic/src/bridge/ssh_api.rs` tests (5/5 pass):

- `key_upload_request_debug_is_redacted` — `format!("{request:?}")` contains no
  key marker, key body, or passphrase.
- `upload_key_span_and_logs_keep_key_material_out` — calls the **real handler**
  (in-memory secrets store, real `SSHBridge`, agent `None`) under a captured
  tracing subscriber and asserts the key marker, key body, and passphrase are
  absent from captured output while the host field is present.
- `ssh_api_requires_bearer_token` — over a real bound listener: 401 without a
  token, 401 with a wrong token, 200 with the correct token.
- `ssh_api_unmounted_without_token` — no `SSH_API_TOKEN` configured → the
  routes are not served at all (fail-closed 404).

CI's gitleaks `secret-detection` job initially flagged the test fixtures
themselves; they are allowlisted by fingerprint in `.gitleaksignore`
(`ic/src/bridge/ssh_api.rs:private-key:374`, `:413`).

## Operations — required follow-up

1. **Rotate** the SSH keys uploaded through the gateway API on any host where
   the pre-fix code ever ran. Treat every such key as compromised.
2. **Purge retained logs**: `journalctl --user --vacuum-time` / targeted
   rotation, daemon log files under the LunarWing base dir on OpenRC hosts, and
   any central log sink the daemons shipped to.
3. Hosts that never used the key-upload endpoint are unaffected.

## Migration note for the router fence

Tenants running the new daemon before their `lunarwing.env` is regenerated
will have the SSH API **unmounted** (fail-closed) until `SSH_API_TOKEN` lands
in their env — mt-admin's `add-tenant`, `start-tenant`, and `upgrade-tenant`
flows regenerate it. After an upgrade, verify the tenant env contains
`SSH_API_TOKEN=` before relying on staged-key uploads; the mt-admin upload
path reads the token from the env file automatically.

## Related hardening — the router itself is now fenced

Review of the same surface (wrench, finding #2, MEDIUM) showed the SSH API
fragment on the shared WebhookServer carried **no authentication at all**:
anyone able to reach the webhook port could enumerate hosts, register
arbitrary ones, overwrite stored keys, or delete them. Commit `8c323c43`
fences that surface:

- `Authorization: Bearer <SSH_API_TOKEN>` is now required on every SSH API
  route (constant-time comparison). Fail-closed: without the token configured
  the routes are not served at all.
- The WebhookServer's fallback bind changed from `0.0.0.0:8080` to
  `127.0.0.1:8080`; a non-loopback bind now only happens when an operator sets
  `HTTP_HOST` explicitly (warned at startup as before).
- mt-admin generates and persists `SSH_API_TOKEN` in each tenant's
  `lunarwing.env` and sends it on the staged-key upload (and its readiness
  probe).

## References

- Log-leak fix: `09b0f507` ("fix(ssh): keep key upload secrets out of tracing spans and logs")
- Router fence: `8c323c43` ("fix(ssh): require bearer auth on SSH API and default webhook bind to loopback")
- CI allowlist: `d89ceff8`
- Finding source: `docs/ops/history/v2.0.9.0_GOAL25_STRESS_TEST_FINDINGS_SYSTEMD.md` (F2)
- Repo invariant: `secrecy` types for sensitive values; never log or expose secrets
