# SSH Agent Harness

**Status:** As-built (revalidated against source 2026-07-20)
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

The SSH Agent harness gives LunarWing worker containers (nanocode, pebble,
opencode; codex removed in v1.1.9), routines, and git operations the ability to authenticate to remote SSH
hosts **without ever exposing raw private-key bytes to those consumers**.

It does this by splitting SSH state into two halves:

| Half | Where it lives | Sensitivity |
|------|----------------|-------------|
| **Host config** — hostname, port, user, key type, host-key policy, timeouts | `config.toml` `[ssh]` / `[[ssh.hosts]]` | Non-sensitive |
| **Private keys + passphrases** | Encrypted secrets store (AES-256-GCM) | Sensitive — never on disk in plaintext |

At startup the daemon decrypts the keys into memory and runs an **in-process
ssh-agent** on a per-tenant Unix domain socket. Workers receive **only the
socket path** via `SSH_AUTH_SOCK`. All signing happens inside the daemon; key
bytes never touch disk and never cross the container boundary.

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
│                     /home/<tenant>/lunarwing/run/ssh-agent.sock                 │
│                                       ▲                                         │
│   HTTP mgmt API  (/hosts, /agent) ────┘  (mounted into the webhook server)      │
└───────────────────────────────────────┬─────────────────────────────────────────┘
                                         │  bind-mount (podman) + SSH_AUTH_SOCK
                                         ▼
                        ┌──────────── worker container ────────────┐
                        │  git / ssh  ──►  SSH_AUTH_SOCK  ──►  sign │
                        │  (nanocode, pebble, opencode)            │
                        └──────────────────────────────────────────┘
```

## 2. Component map

All paths are under `ic/src/`. Symbol names are authoritative; line numbers are
intentionally omitted because they drift as adjacent code changes.

| File | Responsibility |
|------|----------------|
| `bridge/ssh.rs` | **Core coordination + type definitions.** Owns `SSHBridge`, defines all public config/credential/error/audit types, host CRUD, and the load-bearing `start_agent_server()`. Pure orchestration — delegates crypto to `secrets` and the wire protocol to `ssh_agent`. |
| `bridge/ssh_agent.rs` | **In-process ssh-agent server.** `SshAgentServer` binds the Unix socket, parses keys with `russh::keys::decode_secret_key`, runs `russh::keys::agent::server::serve`, and self-connects to `add_identity` each key into russh's internal keystore. |
| `bridge/ssh_secrets.rs` | **Key ↔ secrets-store adapter.** `SshSecretsManager` derives secret names, stores/loads/deletes keys and passphrases, and heuristically classifies key format. |
| `bridge/ssh_hostkeys.rs` | **Host-key verification / known_hosts (in memory).** `HostKeyVerifier` — Strict / AcceptFirst (TOFU), SHA256 fingerprints, mismatch detection. **Live** for the in-process `ssh` and `ssh_git` tool paths (see §7). |
| `bridge/ssh_api.rs` | **HTTP REST management surface (axum).** CRUD over hosts, key upload/delete/status, agent status/keys. |
| `bridge/ssh_client.rs` | **In-process russh client.** `connect_and_exec` used by the `ssh` and WASM `ssh` tools. Wires `HostKeyVerifier` via russh's `Handler::check_server_key`. |
| `config/ssh.rs` | **Config deserialization.** `SshConfig` / `SshHostEntry` map `[ssh]` / `[[ssh.hosts]]` TOML into typed config; `to_host_map()` flattens global defaults against per-host overrides. |

Module registration: `bridge/mod.rs` declares all six submodules (no feature
gate); public types are re-exported from `lib.rs`.

## 3. Data model

### `SSHHostConfig` (`bridge/ssh.rs`) — the runtime per-host record

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `host` | `String` | — | hostname or IP |
| `port` | `u16` | `22` | |
| `user` | `String` | — | |
| `key_type` | `SSHKeyType` | — | `ed25519` / `ecdsa` / `rsa` (serialized lowercase) — identification/logging only |
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
   **`/home/<tenant_name>/lunarwing/run/ssh-agent.sock`**.
   This is deliberately **not** in `/tmp`: the daemon runs with
   `PrivateTmp=true`, so a `/tmp` socket would be invisible to podman workers
   and could not be bind-mounted. The `mt-admin` script predicts this exact
   path.
2. For each configured host, derives the secret name
   `ssh_key_<sanitized-host>` (non-alphanumerics → `_`), calls
   `secrets_store.get_decrypted(tenant_name, secret_name)`, copies the bytes
   into a `Zeroizing<Vec<u8>>`, optionally loads a `<name>_passphrase` secret,
   and builds an `SSHCredentials`. **Fail-soft:** a missing or unreadable key is
   logged and skipped; the agent still starts for the remaining hosts.
3. `SshAgentServer::start(socket_path, keys)`:
   - removes any stale socket, `UnixListener::bind`, then **chmods the socket
     `0o666`** so the worker's OS user (e.g. `nanocode`,
     a different UID than the daemon) can read/write it;
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
Injection is done entirely by `ic/scripts/lunarwing-mt-admin.sh`:

1. **Pre-create** the socket path as a touch-file before the daemon starts, so
   podman doesn't materialize it as a directory.
2. **Start the daemon first** — it removes the touch-file and binds the real
   socket.
3. **Start workers after**, each with the socket bind-mounted and
   `SSH_AUTH_SOCK` set (including generated container service definitions):

   ```
   -v <run_dir>/ssh-agent.sock:/tmp/ssh-agent.sock:z -e SSH_AUTH_SOCK=/tmp/ssh-agent.sock
   ```

Inside the container, ordinary `git` and `ssh` read `SSH_AUTH_SOCK` and talk the
agent protocol back to the daemon. `<run_dir>` resolves to
`/home/<tenant>/lunarwing/run` (`mt-admin` `tenant_run_dir`).

> The bridge does not export a process-wide `SSH_AUTH_SOCK` into the daemon's
> environment. Generic in-daemon shell/git code therefore sees an agent only if
> the daemon inherited one. The built-in `ssh_git` tool is the exception: it
> explicitly sets `SSH_AUTH_SOCK` to `SSHBridge::get_agent_socket_path()` for its
> scrubbed child process. The built-in `ssh` and WASM `ssh` paths use russh and
> decrypted bridge credentials directly rather than an inherited environment.

### 4.4 HTTP management API (`ssh_api.rs`, mounted in `main.rs`)

The API is mounted into the unified webhook server **only if** both `ssh_bridge`
and `secrets_store` exist. `ssh_api::create_router()` registers these routes:

| Method + path | Handler | Notes |
|---|---|---|
| `GET /hosts` | `list_hosts` | per-host `has_key` looked up from the secrets store |
| `POST /hosts` | `add_host` | hardcodes timeouts (10/30/60/3) on the created config |
| `GET /hosts/{host}` | `get_host` | |
| `DELETE /hosts/{host}` | `remove_host` | removes **config only, not the key** → the secret is orphaned |
| `POST /hosts/{host}/key` | `upload_key` | stores the encrypted key; live-adds to a running agent (status only — see below) |
| `DELETE /hosts/{host}/key` | `delete_key` | |
| `GET /hosts/{host}/key/status` | `key_status` | |
| `GET /agent/status` | `agent_status` | `running` / `socket_path` / `keys_loaded` |
| `GET /agent/keys` | `agent_keys` | |

`SSHKeyType` serializes lowercase (`"ed25519"`); `HostKeyMode` serializes
PascalCase (`"Strict"`) — clients must match the casing.

> **Runtime upload caveat.** `upload_key` stores the key in the encrypted
> secrets store **and** calls `agent.add_key` on the running agent. Because
> `add_key` updates only the status map (§4.2), the key does not become usable
> for signing until the **next daemon restart**, when `start_agent_server`
> reloads it from the secrets store into russh's keystore. This is why the
> `mt-admin` provisioning flow (which uploads after startup) yields a working
> key on the following restart cycle.

## 5. Security model

- **Keys at rest.** Only as AES-256-GCM ciphertext in the secrets store
  (`nonce || ciphertext || tag` + a per-secret 32-byte salt; per-secret key via
  HKDF-SHA256 of the master key). The master key comes from
  `SECRETS_MASTER_KEY` (hex env) or the OS keychain. "Never on disk" precisely
  means "never on disk **in plaintext**."
- **Keys at runtime.** Decrypted only inside the daemon, held in `Zeroizing`
  (bytes) and `SecretString` (passphrase), both zeroed on drop. Workers receive
  **signing capability only** through the bind-mounted socket; key bytes never
  cross the container boundary.
- **Per-tenant isolation.** `tenant_id = UUIDv5(NAMESPACE_DNS, owner_id)` is a
  stable internal bridge/audit identifier. Secret-store operations are scoped
  by the string `owner_id` (`tenant_name` in the bridge), and the socket lives
  in the tenant-owned run directory.
- **Host-key trust.** `Strict` (default) or `AcceptFirst` (TOFU + pin). No
  `AcceptAny`. **Note:** the verifier is live for the in-process `ssh` and
  `ssh_git` tool paths (§7); the worker-mode path still relies on the worker's
  own `ssh` client and its container-local `known_hosts`.

### Hardening notes / current sharp edges

- **Socket is `0o666` (world rw)**. Safety rests entirely
  on the parent run directory being tenant-owned and on rootless-podman UID
  mapping. If the run-dir permissions are ever wrong, any local user could sign
  with the tenant's keys. The `set_permissions` error is intentionally ignored.
- **No per-signature confirmation.** `confirm` / `confirm_request` are hardcoded
  to `true` and `add_identity` is called with empty
  constraints. Anyone who can open the socket can request arbitrary signatures.
- **`parse_key` makes a non-zeroized copy** of the key bytes
  (`String::from_utf8_lossy(...).to_string()`) that drops
  without wiping — a small window that partially defeats the `Zeroizing`
  wrapper.
- **The HTTP API has no auth middleware**. It is merged into
  the webhook server (which binds `0.0.0.0` by default), so exposure depends
  entirely on the surrounding network/deployment. In the `mt-admin` model it is
  reached only over `127.0.0.1:<tenant-http-port>`.
- **Audit logging is a no-op in production** (`NullAuditLogger`). The `SshEvent`
  variants are defined but not persisted.

## 6. Design decisions

- **Split sensitive/non-sensitive.** Host topology is not a secret and lives in
  `config.toml` where it is easy to review; only key material goes to the
  encrypted store. This keeps `config.toml` diffable and auditable.
- **Agent socket over key hand-off.** Handing workers an `SSH_AUTH_SOCK`
  (capability) instead of a key file (bearer secret) means keys never leave the
  daemon and are never written to a container filesystem. It also means standard
  `git`/`ssh` "just work" with zero SSH-specific code in the worker.
- **`russh::keys` in-process agent, not the OpenSSH `ssh-agent` binary.** Avoids
  spawning/managing an external process and keeps key material inside the
  daemon's address space. (`russh` 0.62; `russh-keys` is now a module within the
  `russh` crate, not a separate dependency.)
- **Run-dir socket, not `/tmp`.** Required because the daemon runs with
  `PrivateTmp=true`; a `/tmp` socket would be invisible to bind-mounted workers.
- **No `AcceptAny` host-key mode.** Refuses to offer a footgun; the strictest
  useful default (`Strict`) is the default.
- **Fail-soft everywhere.** A broken SSH config degrades SSH only; it never
  takes down the daemon.

## 7. Implementation status & gaps

Built and working:

- `SSHBridge` + all types, host CRUD, validation.
- `SshAgentServer` — real `russh::keys` agent, socket lifecycle, key loading at
  startup, chmod, self-`add_identity`.
- `SshSecretsManager` — store/load/delete keys + passphrases against the
  encrypted secrets store.
- `config/ssh.rs` — `[ssh]` / `[[ssh.hosts]]` parsing + `to_host_map()`.
- `ssh_api.rs` — full CRUD HTTP surface, mounted in the webhook server.
- `mt-admin` provisioning + bind-mount wiring (verified working inside a
  nanocode worker).

Now wired into live paths (via the delivery mechanisms — see
[`SSH_DELIVERY_MECHANISMS.md`](SSH_DELIVERY_MECHANISMS.md)):

- **`HostKeyVerifier` (`ssh_hostkeys.rs`)** is a live consumer as of the
  delivery tools: the built-in `ssh` tool feeds live server keys into
  `verify_from_config` through russh's `check_server_key`
  (`ic/src/bridge/ssh_client.rs`), and the `ssh_git` tool materializes a
  `known_hosts` from its pins. The in-process **russh 0.62 client**
  (`ssh_client.rs`) is likewise now live (the harness previously used only the
  `russh::keys` agent server). Host-key pinning **is** enforced for the in-process
  tools; the worker-mode path (Option 1) still relies on the worker's own `ssh`
  client + `known_hosts`.

Known limitations / follow-ups:

- **Runtime key add is status-only.** A key uploaded to a *running* agent is not
  signable until the next daemon restart (§4.2, §4.4). If online key rotation is
  desired, `SshAgentServer::add_key`/`remove_key` must also drive the agent
  protocol (`add_identity`/`remove_identity`) against russh's keystore.
- **No production `AuditLogger`.** Replace `NullAuditLogger` with a persistent
  implementation to realize the "auditable access" goal.
- **`DELETE /hosts/{host}` orphans the key secret** — it removes config but not
  `ssh_key_<host>`.
- **`from_utf8_lossy` on key bytes** (`ssh_secrets.rs`, `ssh_agent.rs`)
  assumes UTF-8 key material — fine for PEM/OpenSSH text, but binary key blobs
  would be corrupted.

## 8. Test coverage

26 unit tests, all in-module (`#[cfg(test)]`), none in `ic/tests/`:

| File | Tests |
|------|-------|
| `ssh.rs` | 4 — hostname validation, secret-name sanitization, bridge create, add host |
| `ssh_agent.rs` | 1 — start/stop with an **empty** key map |
| `ssh_secrets.rs` | 6 — store/load, passphrase, missing key, delete, format detection (ed25519/rsa) |
| `ssh_hostkeys.rs` | 9 — fingerprint, parse, add/verify, strict reject, AcceptFirst pin, mismatch, remove, per-port |
| `ssh_api.rs` | 1 — `HostRequest` → `SSHHostConfig` conversion |
| `ssh_client.rs` | 3 — `append_capped` under-limit, crossing-limit, and already-full output behavior |
| `config/ssh.rs` | 2 — parse config, `to_host_map` defaults/overrides |

Notable gaps: the agent server is never tested with a **real** key (the sole
test uses an empty map); there are no HTTP-handler tests; and `HostKeyVerifier`
is unit-tested and wired into the in-process client path, though live-server
integration test coverage remains an open gap.

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
| Deployment / injection | `ic/scripts/lunarwing-mt-admin.sh` |
| Operator guide | `docs/ops/SSH-HARNESS-SETUP.md` |
