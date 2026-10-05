# SSH Agent Harness

**Status:** As-built (revalidated against source 2026-09-28, v2.1.3.0; item-40 socket-directory mount; revalidated 2026-10-01, v2.1.4.0 — no `ic/src/bridge/ssh*.rs` or `ic/src/config/ssh.rs` change in the cycle; the WI#265 bridge work touched `router.rs` only, which this doc already notes is independent of the SSH harness; revalidated 2026-10-05, v2.1.5.0 — again no `ic/src/bridge/ssh*.rs` or `ic/src/config/ssh.rs` change; WI#307 kept `ssh`/`ssh_git`/`create_job` on the shared autonomous denylist (`lunarwing_engine::gate::tool_tier`), so the non-autonomous guarantee in §1 stands)
**Code:** `ic/src/bridge/ssh*.rs`, `ic/src/config/ssh.rs`
**Operator guide:** [`docs/ops/SSH-HARNESS-SETUP.md`](../ops/SSH-HARNESS-SETUP.md)
**Delivery mechanisms** (how the agent actually runs SSH work — worker mode, the `ssh`/`ssh_git` built-in tools, the WASM `ssh` tool): [`SSH_DELIVERY_MECHANISMS.md`](SSH_DELIVERY_MECHANISMS.md)

This document describes the design of the SSH Agent harness as it is actually
built. It is design-focused; for step-by-step setup, key provisioning, and
verification, see the operator guide linked above.

> **Naming.** The code and this doc use "SSH bridge" and "SSH agent harness"
> interchangeably. The subsystem physically lives in `ic/src/bridge/` next to
> the Engine V2 bridge, but it is **independent** of Engine V2 — it is compiled
> unconditionally and is not gated on `ENGINE_V2`.

## 1. What it is

The SSH Agent harness gives LunarWing worker containers (Pebble, OpenCode, and
OpenCode 2; the Kimi Code lane no longer mounts the socket — WI#206 removed it
because a headless auto-approved executor must not hold host credentials) a
signing socket and gives interactive chat the built-in `ssh`,
`ssh_git`, and WASM `ssh` delivery paths. `ssh`, `ssh_git`, and `create_job` are
excluded from autonomous jobs and routines; the harness does not make direct SSH
an autonomous routine capability.

Runtime state is split between non-sensitive host configuration and encrypted
credentials:

| Half | Where it lives | Sensitivity |
|------|----------------|-------------|
| **Host config** — hostname, port, user, key type, host-key policy, timeouts | startup `config.toml` `[ssh]` / `[[ssh.hosts]]`; API CRUD mutates only the live map | Non-sensitive |
| **Private keys + passphrases** | Encrypted secrets store (AES-256-GCM) | Sensitive persistent runtime credential |

At startup the daemon decrypts keys for configured hosts into memory and runs an
**in-process ssh-agent** on a per-tenant Unix domain socket. Workers receive the
socket path via `SSH_AUTH_SOCK`; signing happens in the daemon and raw key bytes
do not cross the container boundary.

This does **not** mean plaintext keys never exist on the host filesystem.
mt-admin's default bootstrap creates
`/home/<tenant>/.ssh/id_ed25519_lunarwing`, copies it to the mode-0600
`env/ssh_key_staged` upload file, then deletes only the staging copy after a
successful encrypted-store upload. The original private key remains in
`~/.ssh/`; on upload failure the staging copy also remains for retry. Manual/API
provisioning may have different disk behavior.

```text
┌──────────────────────── LunarWing daemon (per tenant) ─────────────────────────┐
│                                                                                 │
│   config.toml [[ssh.hosts]] ─────► SSHBridge (host map)                         │
│                                       │                                         │
│   Secrets store (AES-256-GCM) ──────► decrypt keys into memory (Zeroizing)      │
│                                       │                                         │
│                                       ▼                                         │
│                             SshAgentServer  ──► russh::keys agent protocol     │
│                             Unix socket at                                      │
│                /home/<tenant>/lunarwing/run/ssh-agent/ssh-agent.sock        │
│                                       ▲                                         │
│   HTTP mgmt API  (/hosts, /agent) ────┘  (mounted into the webhook server)      │
└───────────────────────────────────────┬─────────────────────────────────────────┘
                                         │  bind-mount (podman) + SSH_AUTH_SOCK
                                         ▼
                        ┌──────────── worker container ────────────┐
                        │  git / ssh  ──►  SSH_AUTH_SOCK  ──►  sign │
                        │  (pebble, opencode, kimi, opencode2)     │
                        └──────────────────────────────────────────┘
```

## 2. Component map

All paths are under `ic/src/`. Symbol names are authoritative; line numbers are
intentionally omitted because they drift as adjacent code changes.

| File | Responsibility |
|------|----------------|
| `bridge/ssh.rs` | **Core coordination + type definitions.** Owns `SSHBridge`, defines public config/credential/error/audit types, live-map host CRUD, and the load-bearing `start_agent_server()`. Pure orchestration — delegates crypto to `secrets` and the wire protocol to `ssh_agent`. |
| `bridge/ssh_agent.rs` | **In-process ssh-agent server.** `SshAgentServer` binds the Unix socket, parses keys with `russh::keys::decode_secret_key`, runs `russh::keys::agent::server::serve`, and self-connects to `add_identity` each key into russh's internal keystore. |
| `bridge/ssh_secrets.rs` | **Key ↔ secrets-store adapter.** `SshSecretsManager` derives secret names, stores/loads/deletes keys and passphrases, and heuristically classifies key format. |
| `bridge/ssh_hostkeys.rs` | **Host-key verification / known_hosts persistence.** `HostKeyVerifier` — Strict / AcceptFirst (TOFU), SHA256 fingerprints, mismatch detection, and tenant-scoped encrypted pin-set persistence. Used directly by built-in/WASM russh execution; `ssh_git` learns through the same verifier and materializes its pins into an ephemeral OpenSSH `known_hosts`. |
| `bridge/ssh_api.rs` | **HTTP REST management surface (axum).** Process-local host CRUD, persistent encrypted-key upload/delete/status, and agent status/keys. |
| `bridge/ssh_client.rs` | **In-process russh client.** `connect_and_exec` used by the built-in and WASM `ssh` tools. Wires `HostKeyVerifier` via russh's `Handler::check_server_key`. |
| `config/ssh.rs` | **Startup config deserialization.** `SshConfig` / `SshHostEntry` map `[ssh]` / `[[ssh.hosts]]` TOML into typed config; `to_host_map()` flattens global defaults against per-host overrides. |

Module registration: `bridge/mod.rs` declares all six submodules (no feature
gate); public types are re-exported from `lib.rs`.

## 3. Data model

### `SSHHostConfig` (`bridge/ssh.rs`) — the runtime per-host record

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `host` | `String` | — | hostname or IP |
| `port` | `u16` | `22` | |
| `user` | `String` | — | |
| `key_type` | `SSHKeyType` | — | `ed25519` / `ecdsa` / `rsa` (serialized lowercase); built-in/WASM russh execution rejects `rsa`, while `ssh_git` delegates algorithm negotiation to agent/OpenSSH |
| `host_key_mode` | `HostKeyMode` | `Strict` | `Strict` / `AcceptFirst` (serialized PascalCase) |
| `known_host_key` | `Option<String>` | `None` | pinned OpenSSH-format host public key |
| `connect_timeout_secs` | `u64` | `10` | |
| `operation_timeout_secs` | `u64` | `30` | |
| `keepalive_interval_secs` | `u64` | `60` | `0` disables |
| `keepalive_max_misses` | `u32` | `3` | |

- **`SSHKeyType`** (`bridge/ssh.rs`) — `Ed25519 | Ecdsa | Rsa`, serde `rename_all = "lowercase"`.
- **`HostKeyMode`** (`bridge/ssh.rs`) — `Strict` (default) or `AcceptFirst`. There
  is deliberately **no `AcceptAny`** variant — it would be insecure.
- **`SSHCredentials`** (`bridge/ssh.rs`) — the sensitive in-memory bundle:
  `key_data: Zeroizing<Vec<u8>>` and `passphrase: Option<SecretString>`. Both
  are zeroized on drop. Not `Serialize`/`Deserialize`.

### `SshConfig` / `SshHostEntry` (`config/ssh.rs`) — the TOML shape

`SshConfig` carries global defaults plus a `Vec<SshHostEntry>`. Each
`SshHostEntry` has the mandatory `host` / `user` / `key_type` and **optional**
per-host timeout overrides (`Option<u64>` etc.). `to_host_map()` resolves each
entry into an `SSHHostConfig`, filling any
absent per-host override from the global default.

> `SshHostEntry` does **not** derive `Default`, so `host`, `user`, and
> `key_type` are required for every host. There is **no `enabled` flag** —
> enablement is simply "the `hosts` list is non-empty."

### `SSHBridge` (`bridge/ssh.rs`) — the per-tenant coordinator

Fields: `tenant_id: Uuid`, `tenant_name: String`, `hosts:
Arc<RwLock<HashMap<String, SSHHostConfig>>>`, `secrets_store: Arc<dyn
SecretsStore>`, `audit_logger: Arc<dyn AuditLogger>`, `agent_server:
Option<Arc<SshAgentServer>>` (None until started), `host_key_verifier:
Arc<HostKeyVerifier>`.

Key methods: `new`, `validate`, `get_host_config`, `list_hosts`, `add_host`,
`remove_host`, `start_agent_server`, `stop_agent_server`,
`get_agent_socket_path`, `agent_server`, `load_key`, and `host_key_verifier`.

### `SshEvent` / `AuditLogger` (`bridge/ssh.rs`)

An audit event enum (`HostAdded`, `HostRemoved`, `ConnectionAttempt`,
`CommandExecuted`, `KeyRotated`, `HostKeyChanged`, `AgentStarted`,
`AgentStopped`) and a pluggable `AuditLogger` trait. **Only `NullAuditLogger`
is wired in production today** — audit events are defined but not
persisted anywhere. See §7.

## 4. Lifecycle & flows

### 4.1 Config load → bridge construction (`app.rs`)

1. `config.toml` `[ssh]` / `[[ssh.hosts]]` → `SshConfig` (`config/ssh.rs`),
   carried on `Settings.ssh` with `#[serde(default)]`.
2. In `AppBuilder`, **only if** `!config.ssh.hosts.is_empty()` **and** a secrets
   store is present:
   - `tenant_id = UUIDv5(NAMESPACE_DNS, owner_id)` — stable
     across restarts.
   - `tenant_name = owner_id`.
   - `host_map = config.ssh.to_host_map()`.
   - `audit_logger = NullAuditLogger`.
   - `SSHBridge::new(...)` → `validate()` → `start_agent_server()`.
3. The result is stored as `AppComponents.ssh_bridge:
   Option<Arc<RwLock<SSHBridge>>>`.

**Failure posture is deliberately soft.** `validate()` failure,
`start_agent_server()` failure, and even `SSHBridge::new` failure all only
`warn!` — a misconfigured `[ssh]` block never blocks daemon startup.
`validate()` returns `Ok` on an empty host
map, and checks only hostname validity, `port != 0`, and non-empty user — it
does **not** check that a key secret exists (keys may be uploaded later).

### 4.2 Agent startup — the load-bearing path (`bridge/ssh.rs` → `bridge/ssh_agent.rs`)

`start_agent_server()`:

1. Computes the socket path
   **`/home/<tenant_name>/lunarwing/run/ssh-agent/ssh-agent.sock`**.
   This is deliberately **not** in `/tmp`: the daemon runs with
   `PrivateTmp=true`, so a `/tmp` socket would be invisible to podman workers
   and could not be bind-mounted. It also lives in a **dedicated `ssh-agent/`
   subdirectory** of the run dir, not directly in `run/`: `mt-admin` mounts
   that directory into workers (see §4.3), and `run/` itself also holds the
   daemon's REPL control socket `lunarwing.sock`, which workers must not
   reach. The `mt-admin` script predicts this exact path.
2. For each configured host, derives the secret name
   `ssh_key_<sanitized-host>` (non-alphanumerics → `_`), calls
   `secrets_store.get_decrypted(tenant_name, secret_name)`, copies the bytes
   into a `Zeroizing<Vec<u8>>`, optionally loads a `<name>_passphrase` secret,
   and builds an `SSHCredentials`. **Fail-soft:** a missing or unreadable key is
   logged and skipped; the agent still starts for the remaining hosts.
3. `SshAgentServer::start(socket_path, keys)`:
   - creates the socket's parent directory if missing and chmods it `0o755`
      (the daemon unit runs with `UMask=0077`, and a daemon-created `0700`
      directory would block worker UIDs from traversing to the socket);
   - removes any stale socket, `UnixListener::bind`, then **chmods the socket
      `0o666`** so the worker's OS user (different from the daemon UID) can
      read/write it;
   - parses each key with `russh::keys::decode_secret_key`;
   - spawns `russh::keys::agent::server::serve` over the `UnixListenerStream`;
   - **sleeps 100 ms**, then self-connects as an `AgentClient` and calls
     `add_identity` for each key — this is what
     populates russh's internal keystore, which actually answers
     `REQUEST_IDENTITIES` / `SIGN`.
4. Returns `Arc<SshAgentServer>`, stored on the bridge; consumers read the path
   via `get_agent_socket_path()`.

> **Two keystores.** russh's agent server maintains its **own** internal
> `KeyStore`, separate from the `SshAgent`/`SshAgentServer` `keys` map. The
> struct's `keys` map is **status-reporting
> only**. Consequently the public `SshAgentServer::add_key` / `remove_key`
> methods update only the status map — they do
> **not** change what russh will sign with. Keys become signable **only** at
> `start()` (via `add_identity`). See the runtime-upload note in §4.4 and the
> known limitation in §7.

### 4.3 Injection into workers (out-of-process, via `mt-admin`)

The Rust daemon does **not** push credentials into workers, and the worker code
paths (`orchestrator/external_worker.rs`, `tools/builtin/job.rs`,
`context/state.rs`, `channels/wasm/wrapper.rs`) contain **no SSH code** at all.
Injection is done entirely by `ic/scripts/lunarwing-mt-admin.sh` (since the
v2.0.8.0 refactor a thin composition root over `mt-admin-lib/NN-*.sh` — the
socket pre-creation and daemon-first ordering live in
`mt-admin-lib/04-tenant-lifecycle.sh`, the worker bind-mounts in
`mt-admin-lib/09-worker-config.sh`, and the unit-level mounts in
`mt-admin-lib/07-units-systemd.sh` / `08-units-openrc.sh`):

1. **Pre-create** the socket directory (`run/ssh-agent`, mode 0755) before the
   daemon starts, so podman can bind-mount it even before the first socket
   exists.
2. **Start the daemon first** — it creates the directory if missing and binds
   the real socket inside it.
3. If `add-tenant` left `env/ssh_key_staged`, upload it to the local SSH API.
   A successful upload deletes that staging copy, then mt-admin restarts the
   daemon and waits for its gateway so `start_agent_server()` loads the key into
   russh's signing keystore. Before daemon start, mt-admin preserves or
   atomically backfills the tenant's dedicated `SSH_API_TOKEN`; an upload
   failure retains the staged key and stops startup before workers launch.
4. **Start workers** with the socket **directory** bind-mounted and
   `SSH_AUTH_SOCK` set (including generated container service definitions):

   ```
   -v <run_dir>/ssh-agent:/tmp/ssh-agent:ro,z -e SSH_AUTH_SOCK=/tmp/ssh-agent/ssh-agent.sock
   ```

   Mounting the directory — not the socket file — is the fix for the stale
   inode defect: a daemon restart binds a **fresh socket inode** at the same
   path, while a file-mount keeps the worker pinned to the old, dead inode
   until the container is recreated. With the directory mount the worker
   resolves `ssh-agent.sock` fresh at connect time, so a daemon-only restart
   self-heals. The read-only mount prevents a compromised worker from unlinking
   the host socket and binding a fake agent for sibling workers; `connect(2)`
   still uses the socket inode's `0o666` permission. Shared `:z` relabeling
   continues to allow multiple worker containers to access the socket. The
   bumped launch contracts (`OPENCODE_/OPENCODE2_/PEBBLE_ROOTLESS_LAUNCH_CONTRACT`,
   marker `ssh-agent-dir-ro`) force-recreate imperative containers still
   carrying the old writable mount, including rootful/OpenRC launches. A
   changed Quadlet `Volume=` line forces recreation on systemd user managers.

Inside the container, ordinary `git` and `ssh` read `SSH_AUTH_SOCK` and talk the
agent protocol back to the daemon. `<run_dir>` resolves to
`/home/<tenant>/lunarwing/run` (`mt-admin` `tenant_run_dir`). The agent protocol
provides a generic signing capability, not a destination-bound one: worker SSH
does not consult the daemon's configured host map, so container network policy
and the worker's own host-key policy must constrain destinations.

> The bridge does not export a process-wide `SSH_AUTH_SOCK` into the daemon's
> environment. Generic in-daemon shell/git code therefore sees an agent only if
> the daemon inherited one. The built-in `ssh_git` tool is the exception: it
> explicitly sets `SSH_AUTH_SOCK` to `SSHBridge::get_agent_socket_path()` for its
> scrubbed child process. The built-in and WASM `ssh` paths use russh and decrypted
> bridge credentials directly rather than an inherited environment.

### 4.4 HTTP management API (`ssh_api.rs`, mounted in `main.rs`)

The API is mounted into the unified webhook server **only if** `ssh_bridge`,
`secrets_store`, and a non-empty `SSH_API_TOKEN` exist. Every route requires a
constant-time-validated `Authorization: Bearer <SSH_API_TOKEN>` header.
`ssh_api::create_router()` registers these routes:

| Method + path | Handler | Notes |
|---|---|---|
| `GET /hosts` | `list_hosts` | live-map hosts; per-host `has_key` comes from the secrets store |
| `POST /hosts` | `add_host` | adds to the live map only; hardcodes timeouts (10/30/60/3) |
| `GET /hosts/{host}` | `get_host` | live map only |
| `DELETE /hosts/{host}` | `remove_host` | removes from the live map only; leaves the encrypted key and startup config |
| `POST /hosts/{host}/key` | `upload_key` | stores the encrypted key; updates only the agent's status map |
| `DELETE /hosts/{host}/key` | `delete_key` | deletes the encrypted secret; updates only the agent's status map |
| `GET /hosts/{host}/key/status` | `key_status` | encrypted-secret existence |
| `GET /agent/status` | `agent_status` | `running` / `socket_path` / status-map `keys_loaded` |
| `GET /agent/keys` | `agent_keys` | status-map host names, not a query of russh's signing keystore |

`SSHKeyType` serializes lowercase (`"ed25519"`); `HostKeyMode` serializes
PascalCase (`"Strict"`) — clients must match the casing.

> **Host CRUD is not durable.** `add_host` and `remove_host` mutate only
> `SSHBridge.hosts`; they do not rewrite `config.toml`. A daemon restart rebuilds
> the map from startup configuration, discarding API-added hosts and restoring
> API-removed hosts still present in the file. Durable changes require editing
> `[[ssh.hosts]]` through the operator-controlled config path.
>
> **Runtime key-mutation caveat.** Upload/delete persists the encrypted secret
> immediately, but `SshAgentServer::add_key` and `remove_key` modify only its
> reporting map, not russh's separate signing keystore. Built-in/WASM `ssh`
> reload credentials from the secrets store per call, so they see upload/delete
> immediately. `ssh_git` and workers use the agent socket: an uploaded key is not
> signable there until daemon restart, and a deleted key may remain signable
> there until restart. mt-admin therefore restarts the daemon after a successful
> staged-key upload and starts workers only after that bounce.

## 5. Security model

- **Runtime credential and host-pin store.** Persistent credentials and the
  versioned AcceptFirst host-key pin set consumed by the bridge are AES-256-GCM
  ciphertext in the tenant-scoped secrets store (`nonce || ciphertext || tag`
  plus a per-secret salt; per-secret key via HKDF-SHA256). The master key comes
  from `SECRETS_MASTER_KEY` or the OS keychain. This guarantee does not erase
  external provisioning copies: default mt-admin bootstrap leaves the generated
  mode-0600 private key in the tenant's `~/.ssh/` and temporarily creates a
  second staged copy for API upload.
- **Keys in the daemon.** Secret-store loads use `Zeroizing<Vec<u8>>` and
  passphrases use `SecretString`; workers receive signing capability through the
  bind-mounted socket and raw key bytes do not cross into the container. Some
  intermediate parse/API buffers are not zeroizing (listed below).
- **Per-tenant isolation.** `tenant_id = UUIDv5(NAMESPACE_DNS, owner_id)` is a
  stable internal bridge/audit identifier. Secret-store operations are scoped
  by the string `owner_id` (`tenant_name` in the bridge), and the socket lives
  in the tenant-owned run directory. The socket grants signing, not a
  destination-bound capability; worker egress must be constrained separately.
- **Host-key trust.** `Strict` (default) or `AcceptFirst` (TOFU + encrypted,
  tenant-scoped persistent pin). No `AcceptAny`. Built-in/WASM russh execution
  checks the live server key. `ssh_git` uses a russh host-key preflight to learn
  a missing AcceptFirst pin, then enforces the configured/persisted key through
  an ephemeral OpenSSH `known_hosts`. A pin mismatch, storage failure, or
  malformed persisted pin set fails closed; worker mode relies on its own client
  and container-local `known_hosts`.

### Hardening notes / current sharp edges

- **Socket is `0o666` (world rw)**. Safety rests entirely
  on the parent run directory being tenant-owned and on rootless-podman UID
  mapping. If the run-dir permissions are ever wrong, any local user could sign
  with the tenant's keys. The `set_permissions` error is intentionally ignored.
- **No per-signature confirmation or destination constraint.** `confirm` /
  `confirm_request` are hardcoded to `true` and `add_identity` uses empty
  constraints. Anyone who can open the socket can request arbitrary signatures,
  and the ssh-agent protocol does not bind them to a configured host.
- **Some plaintext buffers are not zeroized.** `parse_key` creates a normal
  `String` through `String::from_utf8_lossy(...).to_string()`. The key-upload API
  accepts a normal `String`, then creates and clones ordinary `Vec<u8>` buffers
  before wrapping the status-map copy. These allocations drop without an
  explicit wipe.
- **The HTTP API uses one tenant-wide bearer token.** `SSH_API_TOKEN` protects
  every SSH management route and missing configuration leaves the router
  unmounted. The webhook server defaults to `127.0.0.1`; operators can still
  configure a non-loopback bind, so token secrecy and network policy remain
  required.
- **Audit logging is a no-op in production** (`NullAuditLogger`). The `SshEvent`
  variants are defined but not persisted, and only host add/remove and agent
  start currently call the logger.

## 6. Design decisions

- **Split runtime topology/credentials.** Host topology is not a secret and
  starts in `config.toml`; bridge-consumed persistent credentials go to the
  encrypted store. This does not promise that external provisioning creates no
  plaintext copies (mt-admin's self-host bootstrap deliberately does).
- **Agent socket over key hand-off.** Handing workers an `SSH_AUTH_SOCK` means raw
  keys do not enter the container and standard `git`/`ssh` work without
  SSH-specific worker code. The trade-off is a broad signing capability rather
  than the alias/host-policy checks applied by built-in tools.
- **`russh::keys` in-process agent, not the OpenSSH `ssh-agent` binary.** Avoids
  spawning/managing an external agent and keeps the live signing keystore in the
  daemon. (`russh` 0.62; `russh-keys` is now a module within the `russh` crate,
  not a separate dependency.)
- **Run-dir socket, not `/tmp`.** Required because the daemon runs with
  `PrivateTmp=true`; a `/tmp` socket would be invisible to bind-mounted workers.
- **No `AcceptAny` host-key mode.** Refuses to offer a footgun; the strictest
  useful default (`Strict`) is the default.
- **Durable TOFU is transactional and fail-closed.** AcceptFirst writes the
  updated encrypted pin set before publishing the key in memory or accepting
  the handshake. Concurrent first connects re-check under the write lock. A
  load, decode, or write failure never falls back to an empty verifier.
- **Fail-soft at the daemon boundary.** A broken SSH config or pin store disables
  the SSH bridge; it never takes down unrelated LunarWing surfaces.

## 7. Implementation status & gaps

Built and working:

- `SSHBridge` + all types, process-local host CRUD, validation, and startup load
  of the tenant-scoped persistent host-key verifier.
- `SshAgentServer` — real `russh::keys` agent, socket lifecycle, startup key
  loading, chmod, self-`add_identity`.
- `SshSecretsManager` — persistent encrypted key/passphrase store operations.
- `HostKeyVerifier` — versioned encrypted AcceptFirst pin persistence, reload,
  mismatch rejection, per-port isolation, and persistent pin revocation when a
  live host is removed.
- `config/ssh.rs` — startup `[ssh]` / `[[ssh.hosts]]` parsing + `to_host_map()`.
- `ssh_api.rs` — live host management, persistent key management, and agent
  status HTTP surface, mounted in the unified webhook server.
- mt-admin plaintext key bootstrap, encrypted upload + daemon bounce, and
  bind-mount wiring for supported workers.

Now wired into live paths (via the delivery mechanisms — see
[`SSH_DELIVERY_MECHANISMS.md`](SSH_DELIVERY_MECHANISMS.md)):

- Built-in and WASM `ssh` feed the live server algorithm and wire key through
  `HostKeyVerifier::verify_from_config_with_key_type` in `ssh_client.rs`.
  `ssh_git` learns a missing AcceptFirst pin through a russh host-key-only
  preflight, then materializes the configured/persisted key into an ephemeral
  OpenSSH `known_hosts` and enforces it strictly. Worker mode is outside these
  checks and relies on its own SSH client and `known_hosts`.

Known limitations / follow-ups:

- **Runtime agent-key mutation is status-only.** Upload does not make a key
  signable through the socket, and delete does not revoke a key already in
  russh's signing keystore, until daemon restart. Online rotation requires
  `add_identity`/`remove_identity` against that keystore.
- **API host CRUD is not persisted.** Restart restores `config.toml`, so a
  removed configured host returns and an API-added host disappears.
- **No production `AuditLogger`.** Replace `NullAuditLogger` with a persistent
  implementation and emit the currently unused connection/command/key/stop
  events to realize the "auditable access" goal.
- **`DELETE /hosts/{host}` leaves the encrypted private-key secret.** It removes
  the live map entry and revokes the learned host-key pin, but the configured
  host itself returns after restart if it remains in `config.toml`.
- **Default mt-admin provisioning retains a plaintext source key** in the
  tenant's `~/.ssh/`; failed upload also retains the staging copy.
- **`from_utf8_lossy` on key bytes** (`ssh_secrets.rs`, `ssh_agent.rs`) assumes
  UTF-8 key material — fine for PEM/OpenSSH text, but binary key blobs would be
  corrupted — and creates a non-zeroized copy.

## 8. Test coverage

38 unit tests across the harness modules, all in-module (`#[cfg(test)]`), none in
`ic/tests/` (tool-specific `ssh` / `ssh_git` tests are additional):

| File | Tests |
|------|-------|
| `ssh.rs` | 5 — hostname and secret-name helpers, bridge create/add, bridge restart + persistent pin revocation |
| `ssh_agent.rs` | 2 — start/stop with an **empty** key map; socket parent-directory creation (permissions + cleanup) |
| `ssh_secrets.rs` | 6 — store/load, passphrase, missing key, delete, format detection (ed25519/rsa) |
| `ssh_hostkeys.rs` | 14 — fingerprint/parse, add/verify/remove, strict/mismatch/ports, encrypted restart persistence, tenant isolation, corruption failure, concurrent-first-key race |
| `ssh_api.rs` | 6 — conversion, secret-safe debug/logging, bearer authentication, disabled-router behavior |
| `ssh_client.rs` | 3 — `append_capped` under-limit, crossing-limit, and already-full output behavior |
| `config/ssh.rs` | 2 — parse config, `to_host_map` defaults/overrides |

Notable gaps: the agent server is never tested with a **real** key (both tests
use an empty map), and no test performs the full host-key preflight and command
flow against a live SSH server. Persistent-pin tests exercise the encrypted
in-memory secrets backend; normal database-backed stores share the same trait
surface and their own CRUD coverage.

> The `config/ssh.rs` tests parse a **bare** `SshConfig`, so their TOML uses
> top-level `[[hosts]]` (no `ssh.` prefix). Do **not** copy that shape into a
> real `config.toml` — see the operator guide for the real nested form.

## 9. File reference

| Concern | File |
|---------|------|
| Core bridge + types | `ic/src/bridge/ssh.rs` |
| Agent server (russh) | `ic/src/bridge/ssh_agent.rs` |
| Secrets adapter | `ic/src/bridge/ssh_secrets.rs` |
| Host-key verifier | `ic/src/bridge/ssh_hostkeys.rs` |
| HTTP API | `ic/src/bridge/ssh_api.rs` |
| In-process SSH client | `ic/src/bridge/ssh_client.rs` |
| Config parsing | `ic/src/config/ssh.rs` |
| Settings field | `ic/src/settings.rs` (`Settings.ssh`) |
| Startup wiring | `ic/src/app.rs` (`AppComponents.ssh_bridge`) |
| API mount | `ic/src/main.rs` |
| Deployment / injection | `ic/scripts/lunarwing-mt-admin.sh` + `ic/scripts/mt-admin-lib/{04-tenant-lifecycle,07-units-systemd,08-units-openrc,09-worker-config}.sh` |
| Operator guide | `docs/ops/SSH-HARNESS-SETUP.md` |
