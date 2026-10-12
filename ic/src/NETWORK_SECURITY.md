# LunarWing Network Security Reference

This document catalogs every network-facing surface in LunarWing, its authentication mechanism, bind address, security controls, and known findings. Use this as the authoritative reference during code reviews that touch network-facing code.

**Last updated:** 2026-02-18

---

## Threat Model

LunarWing operates across four trust boundaries:

| Boundary | Trust Level | Examples |
|----------|------------|---------|
| **Local user** | Fully trusted | TUI, web gateway (loopback), CLI commands |
| **Browser client** | Authenticated | Web UI connected via bearer token; subject to CORS, Origin validation, CSRF protections |
| **Docker containers** | Untrusted (sandboxed) | Worker containers executing user jobs; isolated via per-job tokens, allowlisted egress, dropped capabilities |
| **External services** | Untrusted | Webhook senders (XMPP bridge); authenticated via shared secret |

**Key assumptions:**

- The local machine is single-user. The web gateway and OAuth listener bind to loopback and do not defend against other local users.
- Docker containers are adversarial. A compromised container should not be able to access other jobs, exfiltrate secrets, or reach the host network beyond the orchestrator API.
- Webhook senders must prove knowledge of the shared secret. The secret is never transmitted in the clear by LunarWing itself.
- MCP server URLs are operator-configured and treated as trusted destinations (see [MCP Client](#mcp-client)).

---

## Network Surface Inventory

| Listener | Default Port | Default Bind | Auth Mechanism | Config Env Var | Source |
|----------|-------------|-------------|----------------|----------------|--------|
| Web Gateway | 3000 | `127.0.0.1` | Bearer token (constant-time) | `GATEWAY_HOST`, `GATEWAY_PORT`, `GATEWAY_AUTH_TOKEN` | `server.rs` — `start_server()` |
| HTTP Webhook Server | 8080 | `0.0.0.0` | Shared secret (body field) | `HTTP_HOST`, `HTTP_PORT`, `HTTP_WEBHOOK_SECRET` | `webhook_server.rs` — `start()` |
| Orchestrator Internal API | 50051 | `127.0.0.1` (macOS/Win) / `0.0.0.0` (Linux) | Per-job bearer token (constant-time) | `ORCHESTRATOR_PORT` | `api.rs` — `OrchestratorApi::start()` |
| OAuth Callback Listener | 9876 | `127.0.0.1` | None (ephemeral, 5-min timeout) | N/A (hardcoded) | `oauth_defaults.rs` — `bind_callback_listener()` |
| Sandbox HTTP Proxy | OS-assigned (ephemeral) | `127.0.0.1` | None (loopback only) | N/A (auto-assigned) | `proxy/http.rs` — `SandboxProxy::start()` |

---

## 1. Web Gateway

**Source:** `src/channels/web/server.rs`, `src/channels/web/auth.rs`

### Bind Address

Configurable via `GATEWAY_HOST` (default `127.0.0.1`) and `GATEWAY_PORT` (default `3000`). The gateway is designed as a local-first, single-user service.

**Reference:** `src/config.rs` — `gateway_host` default (`"127.0.0.1"`), `gateway_port` default (`3000`)

### Authentication

Bearer token middleware applied to all `/api/*` routes via `route_layer`. Token checked in two locations:

1. `Authorization: Bearer <token>` header (primary)
2. `?token=<token>` query parameter (fallback for SSE `EventSource` which cannot set headers)

Both paths use **constant-time comparison** via `subtle::ConstantTimeEq` (`ct_eq`).

**Reference:** `src/channels/web/auth.rs` — `auth_middleware()`, header check and query-param fallback both use `ct_eq`

If `GATEWAY_AUTH_TOKEN` is not set, a random hex token is generated at startup.

### Owner-only cron worker recovery (WI#325 / MR !1531)

`GET/POST /api/workers/{worker}/cron` lives on the authenticated gateway
router. The handler compares `AuthenticatedUser.user_id` with
`GatewayState.owner_id` before any worker/manager lookup, then invokes the
manager's independent mission-owner gate. POST accepts only a typed
`pause|resume|status` action and rejects unknown JSON fields; callers cannot
choose their identity. Query-string bearer tokens are not accepted on this
route. Existing CORS/body limits apply, and waits are bounded to 10 seconds.
Enqueue and status-reply are separate phases within that budget: if the
command was accepted by the supervisor queue but not confirmed in time, the
route answers `202 Accepted` (`queued: true`) rather than a misleading 504 —
the mutation may already be applied or still queued, so re-query status
before retrying (pause/resume are idempotent; a repeated resume resets the
circuit breaker again and writes a duplicate audit row). 504 means the
command never entered the queue. Lifecycle audit rows record the origin
(`owner_api` here versus `owner_tool` for the `cron_mission` LLM tool).
The route works independently of Engine V1/V2 and creates no tool/worker
credential bypass. See `handlers/cron_workers.rs` and the cron-mode design doc.

### Unauthenticated Routes

| Route | Purpose | Response |
|-------|---------|----------|
| `/api/health` | Health check endpoint | `{"status":"healthy","channel":"gateway"}` — no version, uptime, or fingerprinting data |
| `/` | Static HTML (embedded) | Single-page app shell |
| `/style.css` | Static CSS (embedded) | Stylesheet |
| `/app.js` | Static JS (embedded) | Client-side app |

### CORS Policy

Restricted to a two-origin allowlist (not browser same-origin policy, but a CORS allowlist that achieves equivalent protection):

- `http://<bind_ip>:<bind_port>`
- `http://localhost:<bind_port>`

Allowed methods: `GET`, `POST`, `PUT`, `DELETE`. Allowed headers: `Content-Type`, `Authorization`. Credentials allowed.

**Reference:** `src/channels/web/server.rs` — `CorsLayer::new()` block

### WebSocket Origin Validation

The `/api/chat/ws` endpoint has two layers of protection:

1. **Bearer token auth** — the route is inside the `protected` router with `route_layer`, so `auth_middleware` runs before the handler. The token is passed via the `Authorization: Bearer` header on the HTTP upgrade request (not via query parameter).

2. **Origin header validation** (inside the handler) as a defense-in-depth guard against cross-site WebSocket hijacking (CSWSH):
   - Origin header is **required** — missing Origin returns 403 (browsers always send it for WS upgrades; absence implies a non-browser client)
   - Origin host is extracted by stripping scheme and port, then compared **exactly** against `localhost`, `127.0.0.1`, and `[::1]`
   - Partial matches like `localhost.evil.com` are rejected because the check extracts the host portion before the first `:` or `/`

**Reference:** `src/channels/web/server.rs` — `chat_ws_handler()` (origin validation block)

### Rate Limiting

Chat endpoint (`/api/chat/send`) enforces a sliding-window rate limit: **30 requests per 60 seconds** (global, not per-IP — single-user gateway).

**Reference:** `src/channels/web/server.rs` — `RateLimiter` struct, `chat_rate_limiter` field

### Body Limits

- Global: **1 MB** max request body (`DefaultBodyLimit::max(1024 * 1024)`)
- **Reference:** `src/channels/web/server.rs` — `.layer(DefaultBodyLimit::max(...))`

### Project File Serving

The `/projects/{project_id}/*` routes serve files from project directories. These are **behind auth middleware** to prevent unauthorized file access.

**Reference:** `src/channels/web/server.rs` — project file routes in `protected` router

### Security Headers

The gateway sets the following security headers on all responses (via `SetResponseHeaderLayer::if_not_present`, so handlers can override):

- `X-Content-Type-Options: nosniff` — prevents MIME-sniffing
- `X-Frame-Options: DENY` — prevents clickjacking via iframes

**Reference:** `src/channels/web/server.rs` — `SetResponseHeaderLayer` calls

### Graceful Shutdown

Shutdown is triggered via a `oneshot::Sender` stored in `GatewayState::shutdown_tx`. The server uses `axum::serve(...).with_graceful_shutdown(...)` to drain in-flight requests before closing the listener.

**Reference:** `src/channels/web/server.rs` — `shutdown_tx` / `shutdown_rx` setup

---

## 2. HTTP Webhook Server

**Source:** `src/channels/webhook_server.rs`, `src/channels/http.rs`

### Bind Address

Configurable via `HTTP_HOST` (default `0.0.0.0`) and `HTTP_PORT` (default `8080`).

**WARNING:** The default bind address is `0.0.0.0`, meaning the webhook server listens on **all interfaces** by default. This is intentional (webhooks must be reachable from external services like XMPP), but operators should be aware of the exposure.

**Reference:** `src/config.rs` — `http_host` default (`"0.0.0.0"`), `http_port` default (`8080`)

### Authentication

Webhook secret is passed **in the JSON request body** (`secret` field), not as a header. The secret is compared using **constant-time** `subtle::ConstantTimeEq` (`ct_eq`).

The secret is required to start the channel — if `HTTP_WEBHOOK_SECRET` is not set, `start()` returns an error.

**CSRF note:** Because the secret is in the JSON body (not a cookie or header that browsers auto-attach), a cross-origin form POST cannot forge a valid request. Browsers would send `application/x-www-form-urlencoded`, which the `Json<T>` extractor rejects with HTTP 415. Even if `Content-Type` were spoofed via CORS preflight, the attacker would need the secret value, which is never stored in the browser.

**Reference:** `src/channels/http.rs` — `webhook_handler()` (secret validation with `ct_eq`), `start()` (required-secret check)

### Content-Type Validation

The webhook endpoint uses axum's `Json<WebhookRequest>` extractor, which enforces `Content-Type: application/json`. Requests with missing or incorrect Content-Type are rejected with **HTTP 415 Unsupported Media Type** before the handler body executes. Malformed JSON bodies are rejected with **HTTP 422 Unprocessable Entity**.

**Reference:** `src/channels/http.rs` — `webhook_handler()` function signature (`Json(req): Json<WebhookRequest>`)

### Rate Limiting

**60 requests per minute**, enforced via a mutex-protected sliding window.

**Reference:** `src/channels/http.rs` — `MAX_REQUESTS_PER_MINUTE` constant, rate-limit check in `webhook_handler()`

### Body Limits

- JSON body: **64 KB** max (`MAX_BODY_BYTES`)
- Message content: **32 KB** max (`MAX_CONTENT_BYTES`)
- Pending synchronous responses: **100 max** (`MAX_PENDING_RESPONSES`)
- Synchronous response timeout: **60 seconds**

**Reference:** `src/channels/http.rs` — constants block (`MAX_BODY_BYTES`, `MAX_CONTENT_BYTES`, `MAX_PENDING_RESPONSES`, `MAX_REQUESTS_PER_MINUTE`)

### Routes

| Route | Auth | Purpose | Response |
|-------|------|---------|----------|
| `/health` | None | Health check | `{"status":"healthy","channel":"http"}` — no fingerprinting data |
| `/webhook` | Webhook secret | Receive messages | Webhook response |

### Graceful Shutdown

Shutdown is triggered via a `oneshot::Sender` stored on the `WebhookServer` struct. The server uses `axum::serve(...).with_graceful_shutdown(...)`. The public `shutdown()` method sends the signal and awaits the task join handle, ensuring a clean drain-and-wait.

**Reference:** `src/channels/webhook_server.rs` — `shutdown()` method

---

## 3. Orchestrator Internal API

**Source:** `src/orchestrator/api.rs`, `src/orchestrator/auth.rs`

### Bind Address

Platform-dependent:

- **macOS / Windows**: `127.0.0.1:<port>` — Docker Desktop routes `host.docker.internal` through its VM to `127.0.0.1`
- **Linux**: `0.0.0.0:<port>` — containers reach the host via the Docker bridge gateway (`172.17.0.1`), which is not loopback

Default port: `50051`.

**Reference:** `src/orchestrator/api.rs` — `OrchestratorApi::start()`, platform-conditional bind address block

### Authentication

Per-job bearer tokens validated by `worker_auth_middleware`:

1. Tokens are **cryptographically random** (32 bytes, hex-encoded = 64 chars)
2. Tokens are **scoped to a specific job_id** — a token for job A cannot access endpoints for job B
3. Comparison uses **constant-time** `subtle::ConstantTimeEq`
4. Tokens are **ephemeral** (in-memory only, never persisted to disk or DB)
5. Tokens and associated credential grants are **revoked** when the container is cleaned up

**Reference:** `src/orchestrator/auth.rs` — `TokenStore::create_token()`, `TokenStore::validate()`, `generate_token()`

### Token Extraction

The middleware extracts the job UUID from the URL path (`/worker/{job_id}/...`) and validates the `Authorization: Bearer` header against the stored token for that specific job.

**Reference:** `src/orchestrator/auth.rs` — `worker_auth_middleware()`, `extract_job_id_from_path()`

### Credential Grants

The orchestrator can grant per-job access to specific secrets from the encrypted secrets store. Grants are:

- Stored alongside the token in the `TokenStore`
- Scoped to specific `(secret_name, env_var)` pairs
- Revoked when the job token is revoked
- Decrypted on-demand when the worker requests `/worker/{job_id}/credentials`

**Reference:** `src/orchestrator/auth.rs` — `CredentialGrant` struct, `src/orchestrator/api.rs` — `get_credentials_handler()`

### Rate Limiting

**None.** The orchestrator API has no rate limiting. All `/worker/*` endpoints are authenticated via per-job bearer tokens, but a compromised container could spam authenticated endpoints without throttling.

**Mitigation:** Tokens are scoped per-job so a compromised container can only abuse its own job's endpoints. Container execution is time-bounded (see [Docker Container Security](#docker-container-security)), which limits the window for abuse.

### Routes

| Route | Auth | Purpose | Response |
|-------|------|---------|----------|
| `/health` | None | Health check | `"ok"` (plain text) — no fingerprinting data |
| `/worker/{job_id}/job` | Per-job token | Get job description | Job JSON |
| `/worker/{job_id}/llm/complete` | Per-job token | Proxy LLM completion | LLM response |
| `/worker/{job_id}/llm/complete_with_tools` | Per-job token | Proxy LLM tool completion | LLM response |
| `/worker/{job_id}/status` | Per-job token | Report worker status | Ack |
| `/worker/{job_id}/complete` | Per-job token | Report job completion | Ack |
| `/worker/{job_id}/event` | Per-job token | Send job events (SSE broadcast) | Ack |
| `/worker/{job_id}/prompt` | Per-job token | Poll for follow-up prompts | Prompt or empty |
| `/worker/{job_id}/credentials` | Per-job token | Retrieve decrypted credentials | Credentials JSON |

### Graceful Shutdown

**None.** The orchestrator calls `axum::serve(listener, router).await?` without `.with_graceful_shutdown()`. The server stops only when the task is dropped (process exit or tokio task cancellation). In-flight requests may be interrupted.

**Reference:** `src/orchestrator/api.rs` — `OrchestratorApi::start()`

---

## 4. OAuth Callback Listener

**Source:** `src/cli/oauth_defaults.rs`

### Bind Address

Always binds to **loopback only**: `127.0.0.1:9876`. Falls back to `[::1]:9876` (IPv6 loopback) if IPv4 binding fails for reasons other than `AddrInUse`. If the port is already in use, the error is returned immediately (fail-fast).

Both IPv4 and IPv6 loopback addresses are security-equivalent — they are only reachable from the local machine.

**Reference:** `src/cli/oauth_defaults.rs` — `OAUTH_CALLBACK_PORT` constant, `bind_callback_listener()`

### Lifecycle

The listener is **ephemeral** — it is started only when an OAuth flow is initiated (e.g., `lunarwing tool auth <name>`) and shut down after the callback is received or the timeout expires.

### Timeout

**5-minute timeout** (`Duration::from_secs(300)`). If the user does not complete the OAuth flow in the browser within 5 minutes, the listener shuts down.

**Reference:** `src/cli/oauth_defaults.rs` — `tokio::time::timeout(Duration::from_secs(300), ...)`

### Security Controls

- **HTML escaping**: Provider names displayed in the landing page are HTML-escaped to prevent XSS (escapes `&`, `<`, `>`, `"`, `'`)
- **Error parameter checking**: The handler checks for `error=` in the callback query string before extracting the auth code
- **URL decoding**: Callback parameters are URL-decoded safely

**Reference:** `src/cli/oauth_defaults.rs` — `html_escape()`

### Built-in OAuth Credentials

Google OAuth client ID and secret are compiled into the binary (with compile-time override via `IRONCLAW_GOOGLE_CLIENT_ID` / `IRONCLAW_GOOGLE_CLIENT_SECRET`). As noted in the source, Google Desktop App client secrets are [not actually secret](https://developers.google.com/identity/protocols/oauth2/native-app) per Google's documentation.

**Reference:** `src/cli/oauth_defaults.rs` — `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` constants

### Graceful Shutdown

Implicit. The listener is a raw `TcpListener` (not axum) inside a `tokio::time::timeout` future. Once the authorization code or error is received, the future returns and the `TcpListener` is dropped, closing the port. No explicit shutdown signal is needed.

**Reference:** `src/cli/oauth_defaults.rs` — `wait_for_callback()`

---

## 5. Sandbox HTTP Proxy

**Source:** `src/sandbox/proxy/http.rs`, `src/sandbox/proxy/allowlist.rs`, `src/sandbox/proxy/policy.rs`

### Runtime model (generic podman sandbox)

The generic sandbox executes commands from the `shell` tool in ephemeral
hardened containers driven by the **podman (preferred) or docker CLI** — no
daemon API socket is required (`src/sandbox/cli_runner.rs`). This is the same
rootless-CLI pattern the mt-admin external-worker tooling uses, and it keeps
the sandbox usable on OpenRC hosts where no `podman system service` socket
exists.

Client-version detection uses `podman version --format` rather than
`podman --version`. The subcommand bypasses Podman's rootless namespace
constructor; on Podman 6.1.2, the flag form can delete a valid `pause.pid` when
the hardened daemon cannot inspect the pause process. Runtime operations still
use the isolated workload context, including the background reaper's list and
remove commands with bounded execution and guarded process-tree cleanup. See
`docs/bugs/BUG-workload-scope-podman-userns.md` for the live reproduction.

> **BOUNDARY:** the generic sandbox (`src/sandbox/`, CLI runner,
> `lunarwing.sandbox` container labels) is separate from the orchestrator's
> live worker-container path (`src/orchestrator/job_manager.rs`, bollard,
> `lunarwing.job_id` labels). The two must not be wired together; the reaper
> (`src/orchestrator/reaper.rs`) is the only shared component.

### Network egress tiers

Sandboxed policies (`readonly`, `workspace_write`) select an egress mode via
`SANDBOX_NETWORK` / `[sandbox].network`:

| Mode | Mechanism | Enforcement |
|------|-----------|-------------|
| `none` (default) | `--network none` on the container | **Kernel-enforced** — no egress at all; the proxy is not started |
| `proxy-enforced` (WI#155) | Container keeps `--network none`; egress only through a per-execution **Unix-socket relay** to the allowlist proxy | **Enforced** — no direct egress path exists anywhere (loopback included); the proxy rejects non-public destinations and pins DNS |
| `proxy` | HTTP proxy + `http_proxy`/`https_proxy` env vars in the container | **Advisory (legacy)** — a process that ignores proxy env vars can still egress directly |

**Default policy (WI#154/155, v2.1.2.0):** the in-code default and mt-admin
both render `SANDBOX_NETWORK=none` (mt-admin preserves an operator-set `proxy`
on re-render). `none` is the safe default everywhere. `proxy` is advisory —
use `proxy-enforced` when sandboxed processes need allowlisted web access.
`proxy-enforced` is kernel-isolated everywhere except one per-execution Unix
socket relay to the public-only validating proxy:

- relay placement: `$XDG_RUNTIME_DIR` when it exists and belongs to our euid
  (fallback: process temp dir). This keeps the socket resolvable across mount
  namespaces: a `PrivateTmp` daemon and a manager-forked transient-service
  workload namespace both share `/run/user/<uid>`.
- relay interior: Python-in-container `--network none` runs a loopback-only
  socket server bound to `127.0.0.1:3128` and forwards to the host-side proxy.
  Only the socket directory is bind-mounted (`:ro,z`), Python runs in isolated
  mode (`python3 -I`), and a failed relay fails closed — it never degrades to
  advisory or direct egress.
- destination policy: the proxy rejects non-public destinations (private,
  loopback, link-local, multicast, CGNAT, and IPv4-mapped forms) and pins DNS
  to the resolved public addresses for both HTTP forwarding and CONNECT.
  Redirects are not followed on the host; environment proxies cannot bypass
  DNS pinning.

The advisory `proxy` mode remains supported for single-node backwards
compatibility but is no longer any default.

In proxy mode the container is pointed at the proxy via
`http://host.containers.internal:<port>` (podman: rootless pasta/slirp4netns
maps that alias to the host's loopback, so the proxy's `127.0.0.1` bind is
reachable) or `http://host.docker.internal:<port>` plus
`--add-host host.docker.internal:host-gateway` on the docker CLI fallback.
FullAccess keeps full, unsandboxed network.

Policy denials returned by the proxy carry the stable body prefix
`sandbox-proxy denied: <reason>` so a sandboxed agent reading command output
can attribute the failure to the proxy rather than the remote service.

### Bind Address

Always binds to **`127.0.0.1`** (localhost only). Port is OS-assigned (port `0`, ephemeral). Falls back to `[::1]` (IPv6 loopback) if IPv4 is unavailable.

Both IPv4 and IPv6 loopback addresses are security-equivalent — they are only reachable from the local machine.

**Reference:** `src/sandbox/proxy/http.rs` — `HttpProxy::start()`, `TcpListener::bind("127.0.0.1:0")`

### Purpose

Acts as an HTTP/HTTPS proxy for sandbox containers. Containers are configured with `http_proxy` / `https_proxy` environment variables pointing to this proxy, so outbound HTTP traffic is routed through it (see the advisory-enforcement caveat above).

### Domain Allowlisting

All requests are validated against a domain allowlist before being forwarded:

- **Empty allowlist = deny all** (fail-closed default)
- Supports exact matches and wildcard patterns (`*.example.com`)
- Validates URL scheme (HTTP/HTTPS only, rejects `ftp://`, `file://`, etc.)

**Reference:** `src/sandbox/proxy/allowlist.rs` — `DomainAllowlist` struct, `is_allowed()` method

### HTTPS Tunneling (CONNECT)

- CONNECT requests for HTTPS tunneling are subject to the same allowlist
- **30-minute timeout** on established tunnels to prevent indefinite holds
- **No MITM**: the proxy cannot inspect or inject credentials into HTTPS traffic (by design — containers that need credentials must use the orchestrator's `/worker/{job_id}/credentials` endpoint)

**Reference:** `src/sandbox/proxy/http.rs` — `handle_connect()` function

### Credential Injection (HTTP only)

For plain HTTP requests to allowed hosts, the proxy can inject credentials:

- Bearer tokens in `Authorization` header
- Custom headers (e.g., `X-API-Key`)
- Query parameters
- Credentials are resolved at request time from the encrypted secrets store
- Credentials never enter the container's environment or filesystem

**Reference:** `src/sandbox/proxy/http.rs` — credential injection block in `handle_request()`

### Hop-by-Hop Header Filtering

The proxy strips hop-by-hop headers to prevent header-based attacks: `connection`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`, `te`, `trailers`, `transfer-encoding`, `upgrade`.

**Reference:** `src/sandbox/proxy/http.rs` — `is_hop_by_hop_header()`

### Container Security

Containers that the generic sandbox spawns (`podman run`/`docker run`) are configured with defense-in-depth:

| Control | Setting | Reference |
|---------|---------|-----------|
| Capabilities | Drop ALL, add only CHOWN | `src/sandbox/cli_runner.rs` — `build_run_args()` |
| Privilege escalation | `no-new-privileges` | `src/sandbox/cli_runner.rs` — `build_run_args()` |
| Root filesystem | Read-only | `src/sandbox/cli_runner.rs` — `--read-only` |
| User | Non-root (`--user 1000:1000`, keep-id userns on podman ≥ 5; docker rootless runs `0:0` inside the userns, rootful docker uses the daemon's uid/gid) | `src/sandbox/cli_runner.rs` — `KeepIdMode`, `user_for()` |
| Process limit | `--pids-limit 256` (explicit; runtime defaults vary) | `src/sandbox/cli_runner.rs` — `build_run_args()`, `default_pids_limit()` |
| Stop grace | `--stop-timeout 10` so kill/rm terminates PID 1 instead of orphaning | `src/sandbox/cli_runner.rs` — `build_run_args()` |
| Seccomp | Runtime default profile (podman/docker default seccomp) — **supported contract**, no custom profile | this document |
| Workspace mount | Policy-selected `ro`/`rw` + SELinux relabel (`,z`) | `src/sandbox/cli_runner.rs` — `build_run_args()` |
| Daemon log mount | `/tenant-logs`, **always `ro`**: the daemon's own secret-scrubbed log copy (`<base>/logs/sandbox/`), daemon-chosen and vetted to expose no protected path (WI#287). Mounted only when `SANDBOX_TENANT_LOGS` is not `off` and the runtime maps the sandbox user onto the daemon uid (docker; podman ≥ 5; podman 4.3+ when the daemon uid is 1000). The copy is capped at INFO, or at DEBUG with `SANDBOX_TENANT_LOGS=debug` | `src/sandbox/mount_guard.rs` — `tenant_logs_mount_source()`; `src/sandbox/cli_runner.rs` — `daemon_owned_mounts_readable()`; `src/tracing_fmt.rs` — `SandboxLogWriter`, `sandbox_log_cap()` |
| Network | Proxy mode (allowlist) or `none` | `src/sandbox/cli_runner.rs`, this document above |
| Tmpfs | `/tmp` (512 MB), `/home/sandbox/.cargo/registry` (1 GB) | `src/sandbox/cli_runner.rs` — `build_run_args()` |
| Auto-remove | `--rm` + kill-by-name on timeout **and on cancellation** (drop guard) + reaper sweep | `src/sandbox/cli_runner.rs` — `run_container()`, `ContainerCleanupGuard`; `src/orchestrator/reaper.rs` |
| Labels | `lunarwing.sandbox=<RFC3339 created>` for the reaper | `src/sandbox/cli_runner.rs` — `SANDBOX_CONTAINER_LABEL` |
| Output limits | Capped stdout/stderr collection (UTF-8-safe) | `src/sandbox/cli_runner.rs` — `CappedBuf` |
| Timeout | Enforced with process-tree kill + `podman kill` | `src/sandbox/cli_runner.rs` — `run_container()` |

**Seccomp decision (WI#179 concern 5):** the supported contract is the
runtime's default seccomp profile. Podman and Docker both apply a default
seccomp profile that blocks the majority of syscalls; the sandbox does not
set a custom profile because none of the candidate profiles would be portable
across podman and docker without shipping a JSON file to every host. The
explicit `--pids-limit` is set because process limits are NOT covered by
runtime defaults uniformly (docker historically had no default pid limit),
whereas seccomp IS applied by default on both runtimes.

Fail-closed semantics: when the sandbox is enabled and the runtime CLI or
image is unavailable, shell commands **error** with a structured
`[sandbox:<kind>]` envelope (see `SandboxError::kind()`) instead of silently
running on the host.

### Graceful Shutdown

Shutdown is triggered via a `oneshot::Sender` stored on the proxy. The accept loop uses `tokio::select!` to race `listener.accept()` against the shutdown signal. The `stop()` method fires the signal; the loop breaks on the next iteration. Note: `stop()` does not await a join handle, so there is no drain-and-wait for in-flight connections.

**Reference:** `src/sandbox/proxy/http.rs` — `stop()` method, `tokio::select!` loop

---

## Egress Controls

### WASM Tool HTTP Requests

WASM tools execute HTTP requests through the host runtime, subject to:

1. **Endpoint allowlist** — declared in `<tool>.capabilities.json`, validated by `AllowlistValidator`
   - Host matching (exact or wildcard)
   - Path prefix matching
   - HTTP method restriction
   - HTTPS required by default
   - Userinfo in URLs (`user:pass@host`) rejected to prevent allowlist bypass
   - Path traversal (`../`, `%2e%2e/`) normalized and blocked
   - Invalid percent-encoding rejected
   - **Reference:** `src/tools/wasm/allowlist.rs`

2. **Credential injection** — secrets injected at the host boundary by `CredentialInjector`
   - WASM code never sees actual credential values
   - Secrets must be in the tool's `allowed_secrets` list
   - Injection supports: Bearer header, Basic auth, custom header, query parameter
   - **Reference:** `src/tools/wasm/credential_injector.rs`

3. **Leak detection** — `LeakDetector` scans both outbound requests and inbound responses for secret patterns
   - Runs at two points: before sending and after receiving
   - Uses Aho-Corasick for fast multi-pattern matching
   - **Reference:** `crates/lunarwing_safety/src/leak_detector.rs`

### Daemon Log Scrubbing and `/tenant-logs` (WI#287)

**Decision: scrub at write time (option A), and give the sandbox only the
scrubbed copy.** Every daemon log sink goes through one `LogScrubber`
(`crates/lunarwing_safety/src/log_scrubber.rs`, built by
`tracing_fmt::daemon_log_scrubber`):

- process stderr, which lands in the journal on systemd (single-node and
  multi-tenant), in `<base>/logs/daemon.stderr.log` under
  `lunarwing service install` on OpenRC and launchd, in
  `/var/log/lunarwing/lunarwing.err` under the committed OpenRC template
  (`systemd/lunarwing.openrc`), and in `<tenant root>/logs/lunarwing.err` on
  multi-tenant OpenRC. It is scrubbed before the 500-byte terminal cut;
- the SSE log stream (`/api/logs/events` and its history);
- `<base>/logs/sandbox/daemon.log`, the copy the sandbox sees.

What is masked:

- **Shapes:** the default leak shapes, with the 64-hex heuristic promoted from
  warn to redact for logs because it is the gateway token's own shape. Any
  standalone 64-hex value, SHA-256 digests included, therefore shows as
  `[REDACTED]` in daemon logs. Log-only shapes add credentials in URLs
  (`scheme://user:password@`), secret-named query parameters (`?token=`,
  `&client_secret=`, `&password=`, `&pwd=`, `X-Amz-Signature=`, ...), and
  `Authorization:`/`Bearer` values containing `+`, `/`, `=` or `%`, also inside
  quoted header-map dumps. The log-only shapes do not change tool-output or
  outbound-HTTP scanning.
- **Known values** of at least 12 bytes: the values of secret-named
  environment variables (`*TOKEN*`, `*SECRET*`, `*PASSWORD*`, `*PASSWD*`,
  `*API_KEY*`, `*PRIVATE_KEY*`, `*MASTER_KEY*`; `*_FILE`, `*_PATH` and `*_DIR`
  names point at secrets and are skipped), and the password of any URL-valued
  environment variable such as `DATABASE_URL`, as written and percent-decoded.
  Values learned after startup are registered through
  `tracing_fmt::register_log_secret`: the gateway token in
  `GatewayChannel::new` (generated, or loaded from DB settings on single-node
  installs, so never in the environment there), every value of the
  secrets-store overlay (`config::inject_single_var`, `merge_injected_vars`:
  LLM keys, `HTTP_WEBHOOK_SECRET`, `SECRETS_MASTER_KEY`, wizard keys), and
  secret-named runtime overrides (`config::set_runtime_env`, e.g.
  `LUNARWING_CLOUD_API_KEY` after an interactive login).

Every occurrence of every known value is found, overlapping ones included,
and all ranges (known values and shapes, in the original record's
coordinates) are merged before one masking pass. `LeakDetector`'s own
redaction merges nested and overlapping ranges the same way, so no
combination of matches re-emits the tail of a secret. Block-class records are
replaced whole; redact-class matches are masked in place. The stderr layer
colors field names, so stderr events are scrubbed with their color (SGR)
sequences stripped: a clean event keeps its colors, a redacted one is written
plain. Only SGR sequences are stripped, so a stray escape in a field value
cannot swallow the first characters of a token and hide it.

**Raw logs are never mounted, and the agent cannot reach them as a
`workdir`.** systemd hosts have no log files (journal). The committed OpenRC
template exports its log directories as `LUNARWING_SERVICE_LOG_DIRS`, which
`mount_guard::resolve_protected_paths` adds to `SANDBOX_PROTECTED_PATHS`. It
never exports `SANDBOX_PROTECTED_PATHS` itself: the daemon loads `./.env` and
`<base>/.env` without overriding the environment, so an exported value would
silently shadow an operator's setting there. `lunarwing service install`
(OpenRC and launchd) logs inside `<base>`, which is protected; the multi-tenant
`<root>/logs` directory, which also holds other services' raw logs (proxy,
xmpp-bridge, weechat, darkirc, babysitters), is protected by the
`add_mt_tenant_siblings` rule and the mt-admin `SANDBOX_PROTECTED_PATHS`
render, and the harness layout (`TEST_ROOT/{logs,state}`) by the same sibling
rule. A custom redirect elsewhere belongs in `SANDBOX_PROTECTED_PATHS`. The
boot banner prints the gateway URL with `?token=` on every start when the CLI
channel is enabled; when stdout is not a terminal (a redirect into one of
these files) the token is replaced by
`<hidden: GATEWAY_AUTH_TOKEN, or lunarwing config get channels.gateway_auth_token>`.

**The mount.** The shell sandbox mounts `<base>/logs/sandbox/` read-only at
`/tenant-logs` (`daemon.log`, rotated to `daemon.log.1` at 8 MiB; 4 KiB per
event). `SANDBOX_TENANT_LOGS` selects what the copy holds: `info` (default,
also rendered by mt-admin) caps it at INFO whatever the runtime level; `debug`
follows the runtime level up to DEBUG, where the per-tool-call lines are, and
never TRACE; `off` writes no copy and mounts nothing (for a single-node daemon
shared by several users, whose agents would otherwise all read one copy). The
mount stays for `--network none` runs: that is the multi-tenant default, and
with no egress the only channel out is tool output to the same agent. The
file is `0600` in a `0700` directory owned by the daemon, so it is mounted
only where the runtime maps the sandbox user onto the daemon's uid
(`cli_runner::daemon_owned_mounts_readable`): docker (rootful runs as the
daemon's euid, rootless maps container root onto the host user), podman ≥ 5
(`keep-id:uid=1000`), or podman 4.3+ (`keep-id`) when the daemon uid is 1000.
Older podman and other uids get no mount, and the shell tool's description
only says the logs are there "if /tenant-logs exists".

Limits: only tracing output of the daemon is scrubbed. Raw
`println!`/`eprintln!` and panic output, and other processes (CLI commands,
workers, bridges, which use plain `fmt` subscribers), are not. A secret is
recognized only by a shape or a known value; values under 12 bytes are never
matched literally; URL userinfo whose password holds an unencoded `/` or `@`
is not matched by shape. Credentials decrypted on demand from the secrets
store (channel, MCP and OAuth credentials) are deliberately not registered:
`DecryptedSecret` zeroizes after use, and registering every decrypt would keep
the plaintext for the process lifetime, so they are caught by shape only.
Docker `userns-remap` is not detected (the copy is then unreadable, not
exposed). Log lines the agent reads are still untrusted text and pass through
the usual tool-output sanitization.

### LunarVision Service Clients

`VISION_SERVICE_URL` is operator-controlled. Both the automatic XMPP enrichment
client and bundled `vision-analyze` WASM tool accept plaintext HTTP only for
loopback or `host.containers.internal`; external instances must use HTTPS. Base
URLs cannot contain userinfo, a path, query, or fragment.

The automatic client disables redirects and environment proxies, uses bounded
connect/request timeouts, caps responses at 1 MiB, and holds
`VISION_AUTH_TOKEN` in `SecretString`. The WASM host rewrites the tool's HTTP
capability to the configured host and `/v1/` POST path, then applies an additional
request-time exact-origin check covering scheme, host, and port. Its bearer token
is injected only after leak scanning and only for that exact origin/path. Local
service calls bypass proxies; external HTTPS calls retain normal outbound proxy
support. The managed external lifecycle accepts bearer tokens only through a
one-line file, health-gates the remote endpoint, and stores only its non-secret
URL/mode in the ports registry.

**References:** `src/vision_enrichment.rs`, `src/tools/wasm/wrapper.rs`,
`scripts/mt-admin-lib/02-containers.sh`

### Built-in HTTP Tool

The `http` tool (`src/tools/builtin/http.rs`) has its own SSRF protections:

| Protection | Details | Reference |
|-----------|---------|-----------|
| HTTPS only | Rejects `http://` URLs | `http.rs` — scheme check |
| Localhost blocked | Rejects `localhost` and `*.localhost` | `http.rs` — host check |
| Private IP blocked | Rejects RFC 1918, loopback, link-local, multicast, unspecified | `http.rs` — `is_disallowed_ip()` |
| DNS rebinding | Resolves hostname and checks all resolved IPs against blocklist | `http.rs` — DNS resolution block |
| Cloud metadata | Blocks `169.254.169.254` (AWS/GCP metadata endpoint) | `http.rs` — `is_disallowed_ip()` |
| Redirect blocking | Returns error on 3xx responses (prevents SSRF via redirect) | `http.rs` — status code check |
| Response size limit | **5 MB** max, enforced both via Content-Length header and streaming | `http.rs` — `MAX_RESPONSE_SIZE` constant, streaming cap |
| Outbound leak scan | Scans URL, headers, and body for secrets before sending | `http.rs` — `LeakDetector::scan_http_request()` |
| Approval required | Requires user approval before execution | `http.rs` — `requires_approval()` returns `true` |
| Timeout | 30 seconds default | `http.rs` — `reqwest::Client` builder |
| No redirects | `redirect::Policy::none()` — redirects are not followed | `http.rs` — `reqwest::Client` builder |

### MCP Client

MCP servers are external processes accessed via HTTP. The MCP client (`src/tools/mcp/client.rs`) uses `reqwest` with a 30-second timeout but has **no SSRF protections** — it connects to whatever URL is configured for the MCP server.

This is by design: MCP server URLs come from **operator-controlled configuration** (config files, environment variables, or the CLI `tool install` command), not from user input or LLM output. A compromised config file is outside LunarWing's threat model — it would imply the operator's machine is already compromised.

**Reference:** `src/tools/mcp/client.rs` — `reqwest::Client` builder

### Sandbox Domain Allowlists

Sandbox containers route all HTTP traffic through the proxy, which enforces a domain allowlist. The allowlist is built from:

1. A default set of domains (`src/sandbox/config.rs` — `default_allowlist()`)
2. Additional domains from `SANDBOX_EXTRA_DOMAINS` env var (comma-separated)

**Reference:** `src/config.rs` — sandbox allowlist assembly

---

## Authentication Mechanisms Summary

| Mechanism | Constant-Time | Used By | Reference |
|-----------|:------------:|---------|-----------|
| Gateway bearer token | Yes | Web gateway (header + query) | `src/channels/web/auth.rs` — `auth_middleware()` |
| Webhook shared secret | Yes | HTTP webhook (`ct_eq` comparison) | `src/channels/http.rs` — `webhook_handler()` |
| Per-job bearer token | Yes | Orchestrator worker API | `src/orchestrator/auth.rs` — `TokenStore::validate()` |
| OAuth callback | N/A | CLI OAuth flow (no auth, loopback-only) | `src/cli/oauth_defaults.rs` — `bind_callback_listener()` |
| Sandbox proxy | N/A | No auth (loopback-only, ephemeral) | `src/sandbox/proxy/http.rs` — `SandboxProxy::start()` |

---

## Known Security Findings

### Open

#### F-2. No TLS at the application layer

**Severity:** Low (for local deployment)
**Details:** None of the listeners terminate TLS. All communication is plain HTTP.
**Mitigation:** The web gateway and OAuth callback bind to loopback by default. For production, users are expected to front the gateway with a TLS reverse proxy (such as nginx or Caddy), NetBird Reverse Proxy, a custom managed tunnel, or an externally managed HTTPS tunnel configured through `TUNNEL_URL`.
**Recommendation:** Document the requirement for a TLS-terminating reverse proxy in deployment guides.

#### F-3. Orchestrator binds to `0.0.0.0` on Linux

**Severity:** Medium
**Location:** `src/orchestrator/api.rs` — platform-conditional bind in `OrchestratorApi::start()`
**Details:** On Linux, the orchestrator API binds to all interfaces because Docker containers reach the host via the bridge gateway (`172.17.0.1`), not loopback. This means the API is reachable from any network interface on the host.
**Mitigation:** All `/worker/*` endpoints require per-job bearer tokens (constant-time, cryptographically random). The `/health` endpoint is the only unauthenticated route and returns only `"ok"`. Firewall rules should block external access to port 50051.
**Recommendation:** Document firewall requirements for Linux deployments. Consider binding to the Docker bridge IP (`172.17.0.1`) instead of `0.0.0.0`.

#### F-6. WebSocket/SSE connection limit

**Severity:** Info
**Details:** The `SseManager` enforces a hard limit of **100 concurrent connections** (`MAX_CONNECTIONS` constant in `src/channels/web/sse.rs`). Both SSE subscribers and WebSocket connections share this counter. When exceeded, new WebSocket upgrades are rejected with a warning log and the connection is immediately closed.
**Reference:** `src/channels/web/sse.rs` — `MAX_CONNECTIONS`, `src/channels/web/ws.rs` — `handle_ws_connection()` early return

#### F-7. Orchestrator API has no rate limiting

**Severity:** Low
**Details:** The orchestrator API has no request-rate throttling. A compromised container could spam authenticated endpoints (e.g., `/worker/{job_id}/llm/complete`) to drive up LLM costs or degrade service for other jobs.
**Mitigation:** Tokens are scoped per-job, limiting blast radius. Container execution is time-bounded by the sandbox timeout, which caps the abuse window.
**Recommendation:** Consider adding per-token rate limiting on the LLM proxy endpoints.

#### F-8. Orchestrator API has no graceful shutdown

**Severity:** Info
**Details:** The orchestrator calls `axum::serve(listener, router).await?` without `.with_graceful_shutdown()`. In-flight requests (including LLM proxy calls) may be interrupted during process shutdown.
**Reference:** `src/orchestrator/api.rs` — `OrchestratorApi::start()`

### Resolved / Mitigated

<details>
<summary>Resolved and mitigated findings (click to expand)</summary>

#### F-1. ~~Webhook secret comparison is not constant-time~~ (Resolved)

**Severity:** Low
**Location:** `src/channels/http.rs` — `webhook_handler()`
**Status:** Resolved — webhook secret now uses `subtle::ConstantTimeEq` (`ct_eq`), consistent with web gateway and orchestrator auth.

#### F-4. ~~HTTP webhook server binds to `0.0.0.0` by default~~ (Mitigated)

**Severity:** Low
**Location:** `src/config.rs`, `src/main.rs`
**Status:** Mitigated — a `tracing::warn!` is now emitted at startup when the webhook server binds to an unspecified address (`0.0.0.0` or `::`), advising operators to set `HTTP_HOST=127.0.0.1` to restrict to localhost. The default bind address remains `0.0.0.0`, so webhook exposure is still controlled by operator configuration and external network controls (firewalls, ingress rules).

#### F-5. ~~Missing security headers on web gateway~~ (Mitigated)

**Severity:** Low
**Status:** Mitigated — `X-Content-Type-Options: nosniff` and `X-Frame-Options: DENY` are now set on all gateway responses via `SetResponseHeaderLayer::if_not_present`. Layer ordering ensures these headers are applied even to error responses generated by inner layers (e.g., `DefaultBodyLimit` 413 rejections).

</details>

---

## Review Checklist for Network Changes

Use this checklist for any PR that adds or modifies network-facing code.

### New Listener

- [ ] **Bind address**: Does it bind to loopback (`127.0.0.1`) or all interfaces (`0.0.0.0`)? Justify if `0.0.0.0`.
- [ ] **Port configuration**: Is the port configurable via env var? Is a sensible default set?
- [ ] **Authentication**: Is auth required? If yes, is it constant-time? If no, why not?
- [ ] **Rate limiting**: Is there a rate limiter? What are the limits?
- [ ] **Body size limit**: Is `DefaultBodyLimit` (or equivalent) set?
- [ ] **Content-Type validation**: Does the handler validate Content-Type (e.g., via axum `Json<T>` extractor)?
- [ ] **Graceful shutdown**: Does the listener support graceful shutdown via oneshot or similar?
- [ ] **Inventory update**: Is this document updated with the new listener?

### New Route on Existing Listener

- [ ] **Auth layer**: Is the route behind the auth middleware? If public, why?
- [ ] **Input validation**: Are path parameters, query parameters, and body fields validated?
- [ ] **Error responses**: Do error responses avoid leaking internal details?

### Egress (Outbound HTTP)

- [ ] **SSRF protection**: Does the code block private IPs, localhost, and cloud metadata endpoints?
- [ ] **DNS rebinding**: Are resolved IPs checked (not just the hostname)?
- [ ] **Redirect handling**: Are redirects blocked or validated?
- [ ] **Response size**: Is there a max response size?
- [ ] **Timeout**: Is a request timeout set?
- [ ] **Leak detection**: Is the outbound request scanned for secrets?

### Credential Handling

- [ ] **Constant-time comparison**: Are secrets compared with `subtle::ConstantTimeEq`?
- [ ] **No logging**: Are credentials excluded from log messages?
- [ ] **Ephemeral storage**: Are tokens stored in memory only (not persisted)?
- [ ] **Scope**: Are credentials scoped to the minimum necessary (per-job, per-tool)?
- [ ] **Revocation**: Are credentials revoked when no longer needed?

### Container / Sandbox

- [ ] **Capabilities**: Are all capabilities dropped except what's needed?
- [ ] **Filesystem**: Is the root filesystem read-only?
- [ ] **User**: Does the container run as non-root?
- [ ] **Network**: Is network access routed through the proxy?
- [ ] **Timeout**: Is there an execution timeout with forced cleanup?
- [ ] **Output limits**: Are stdout/stderr capped?
