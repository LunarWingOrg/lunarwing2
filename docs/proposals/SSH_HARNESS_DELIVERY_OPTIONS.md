
# SSH Harness — Delivery Options

> **Current status (reverified 2026-08-25): IMPLEMENTED.** All three delivery
> mechanisms are in tree. `mode="worker"` now means LunarWing's Docker sandbox;
> persistent external workers use their configured names (`pebble`, `opencode`,
> or `kimi`). Nanocode is retired. The mechanism and security summaries below
> were reconciled with the current implementation.

**Date:** 2026-06-25 (updated 2026-07-01)  
**Status:** ✅ **All three options implemented.** This is the original options
overview; the authoritative as-built doc is
[`../architecture/SSH_DELIVERY_MECHANISMS.md`](../architecture/SSH_DELIVERY_MECHANISMS.md)
(design detail in
[`SSH_HARNESS_OPTION_2_3_IMPLEMENTATION.md`](SSH_HARNESS_OPTION_2_3_IMPLEMENTATION.md)).

## Overview

Three delivery mechanisms for SSH operations in LunarWing:

---

## Option 1: `create_job` in a Worker Container ✅ IMPLEMENTED

```
Agent → create_job(mode="worker" | "pebble" | "opencode" | "kimi")
  ├── mode="worker": ephemeral Docker sandbox when enabled
  ├── named mode: configured persistent external worker
  ├── SSH_AUTH_SOCK mounted from the gateway
  └── worker git/ssh chooses its destination and asks the socket to sign
```

**Pros:**
- Strong process/filesystem isolation (separate container)
- Raw key bytes never enter the worker container
- Leverages existing sandbox and external-worker infrastructure

**Cons:**
- Requires the Docker sandbox or a configured named worker
- Latency and resource overhead from container execution
- The socket is a generic signing capability, not a configured-host allowlist;
  worker network policy and `known_hosts` must constrain destination and trust

**Status:** ✅ Implemented for the Docker worker and supported Pebble, OpenCode,
and Kimi Code external workers.

---

## Option 2: Built-in Rust SSH Tool

```
Agent → SSH tool (compiled into lunarwing binary)
  ├── Wraps russh directly
  ├── Executes remote commands / git ops inline
  └── No external worker needed
```

**Pros:**
- No worker overhead (faster execution)
- Simpler deployment (no worker infra needed)
- Direct access to russh API

**Cons:**
- Less isolation (SSH runs in gateway process)
- More attack surface in core binary
- Harder to sandbox/limit resources
- SSH credentials loaded into gateway memory space

**Status:** ✅ Implemented — the `ssh` and `ssh_git` built-in tools
(`ic/src/tools/builtin/ssh.rs`, `ssh_git.rs`; russh client in
`ic/src/bridge/ssh_client.rs`).

**Use Cases:**
- Quick one-off SSH commands
- Git operations that don't justify full worker spin-up
- Environments without worker infrastructure

---

## Option 3: WASM SSH Tool

```
Agent → WASM guest (sandboxed via WASI) → ssh-exec host function
  ├── guest is limited by WASM/WASI capabilities and an allowed-hosts grant
  ├── host function resolves the configured alias and runs russh in the gateway
  └── key material and network sockets never enter the guest
```

**Pros:**
- Sandboxes the guest logic and exposes only a narrow `ssh-exec` interface
- Adds a per-tool `ssh.allowed_hosts` capability ceiling
- Portable guest component; host reuses the built-in russh implementation

**Cons:**
- The SSH operation itself is not isolated from the gateway process
- One-shot command execution only; no PTY or interactive stream
- More plumbing than the equivalent built-in `ssh` call

**Status:** ✅ Implemented — the WASM `ssh` guest (`ic/tools-src/ssh/`) calls an
`ssh-exec` host function in `ic/src/tools/wasm/wrapper.rs`, defined by
`ic/wit/tool.wit`. It does not compile russh into WASM or expose the ssh-agent
protocol to the guest.

---

## Recommendation

| Priority | Option | When |
|----------|--------|------|
| 1 | Worker container (Option 1) | Multi-step/long work where process isolation matters |
| 2 | Rust tools (Option 2) | Quick remote command or git operation without a worker |
| 3 | WASM tool (Option 3) | A plugin needs a narrow per-tool host capability |

**Rationale:** worker mode provides the strongest isolation but a broad signing
socket. Built-in tools are the direct low-latency path with configured-host and
host-key checks. The WASM option narrows which guest may invoke which alias, but
it does not sandbox the host-side SSH exchange.

---

## Implementation Notes

- **Socket location:** Tenant run directory (`/home/<tenant>/lunarwing/run/ssh-agent.sock`)
- **Why not /tmp:** Daemon runs with `PrivateTmp=true`
- **Mounting:** Socket mounted into worker containers via `SSH_AUTH_SOCK` env var
- **Runtime key store:** encrypted secrets are loaded into the daemon/agent; raw
  key bytes are not passed to workers or WASM guests
- **Provisioning caveat:** default mt-admin setup also leaves its generated
  mode-0600 key at `/home/<tenant>/.ssh/id_ed25519_lunarwing`; the temporary
  `env/ssh_key_staged` copy is removed only after a successful API upload
