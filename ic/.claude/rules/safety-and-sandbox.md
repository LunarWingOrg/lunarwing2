---
paths:
  - "src/safety/**"
  - "src/sandbox/**"
  - "src/secrets/**"
  - "src/tools/wasm/**"
---
# Safety Layer & Sandbox Rules

## Safety Layer

All external tool output passes through `SafetyLayer`:
1. **Sanitizer** - Detects injection patterns, escapes dangerous content. Detection is position-free and deliberately stricter than its line-start-only neutralization (WI#289): do not "align" detection to positional rules — see the module docs in `crates/lunarwing_safety/src/sanitizer.rs`
2. **Validator** - Checks length, encoding, forbidden patterns
3. **Policy** - Rules with severity (Critical/High/Medium/Low) and actions (Block/Warn/Review/Sanitize)
4. **Leak Detector** - Scans for 15+ secret patterns at two points: tool output before LLM, and LLM responses before user

Tool outputs are wrapped in `<tool_output>` XML before reaching the LLM.

## Shell Environment Scrubbing

The shell tool scrubs sensitive env vars before executing commands. The sanitizer detects command injection patterns (chained commands, subshells, path traversal).

## Sandbox Policies

| Policy | Filesystem | Network |
|--------|-----------|---------|
| ReadOnly | Read-only workspace | Proxy allowlist or `none` (see egress tiers) |
| WorkspaceWrite | Read-write workspace | Proxy allowlist or `none` (see egress tiers) |
| FullAccess | Full filesystem (host execution) | Unrestricted |

The generic sandbox runs shell-tool commands in ephemeral hardened containers
via the **podman/docker CLI** (`src/sandbox/cli_runner.rs` — no daemon socket).
Shell-tool execution is **fail-closed**: when the sandbox is enabled but the
runtime or image is unavailable, commands error with a structured
`[sandbox:<kind>]` envelope instead of running on the host. Dev mode
(`ALLOW_LOCAL_TOOLS=1`) keeps direct host execution unless `SANDBOX_ENFORCE=true`.

## Network Egress Tiers

- `none` (default off Linux): `--network none` — kernel-enforced isolation, no egress, no proxy.
- `proxy-enforced` (Linux default; WI#155/537): container keeps `--network none`; the only egress
  path is a per-execution Unix-socket relay to the validating proxy (public-only,
  DNS-pinned, redirect rechecked). Requires a local Linux runtime and Python 3
  in the image; relay setup failures fail closed (`[sandbox:proxy_error]`),
  never degrade to advisory or direct egress. This mode has no credential
  mappings and must not inject daemon secrets.
- `tunnel` (WI#248/592): runs join their daemon instance's NetBird/WireGuard
  sidecar netns while keeping every hardening flag. Mandatory sidecar `nftables`
  chains drop new inbound flows (never an inbound listener) and every outbound
  flow that does not leave through the tunnel interface: the uplink carries only
  the provider's marked control traffic and, by default, DNS to the runtime
  resolvers; `SANDBOX_TUNNEL_DNS` removes that DNS allowance. Link-local IPv4
  (cloud metadata, host alias) is dropped. Provider health and both chains are
  rechecked rule by rule before every run; failures return
  `[sandbox:sandbox_unavailable]` and never degrade. NetBird setup keys use an
  atomic 0600 env-file in the private runtime dir; WireGuard configs are
  regular non-symlink 0600 files mounted read-only. Keys must never enter argv
  or logs. Never add an accept for the uplink or weaken either chain.
- `proxy`: legacy allowlist HTTP proxy + credential injection. **Advisory** —
  enforced via env vars, which a process may ignore.

**Default (WI#537):** on Linux the in-code default and new mt-admin-rendered
tenants use `proxy-enforced`; other platforms default to `none`. mt-admin
preserves every operator-set value on re-render: `none`, `proxy-enforced`,
`tunnel`, or legacy advisory `proxy`. Never make advisory `proxy` or a bridged
network the default. The enforced relay is the
only egress path to the public-only proxy; its socket lives under
`$XDG_RUNTIME_DIR` when available so it stays visible to both a `PrivateTmp`
daemon and manager-forked workload namespaces.

**Hardening highlights:** explicit `--pids-limit 256`, `--stop-timeout 10`,
`--entrypoint sh`, `--workdir /workspace`, explicit `--user` resolved per
runtime (keep-id / rootless mapping / daemon euid). Rootful Docker under a
root daemon refuses writable workspaces (never container root). Detached
cleanup is backstopped by the API-independent CLI reaper with bounded probes.

**Daemon logs (WI#287):** the only daemon-chosen data mount is `/tenant-logs`,
always read-only: the daemon's secret-scrubbed copy at `<base>/logs/sandbox/`,
vetted by `mount_guard::tenant_logs_mount_source`, and added only when
`SANDBOX_TENANT_LOGS` is not `off` and `cli_runner::daemon_owned_mounts_readable`
holds for the runtime. Never mount raw log directories (systemd hosts log to
the journal, so there are none; OpenRC/MT ones are unscrubbed and shared with
other services): MT `<root>/logs` is a protected path, and the OpenRC template
protects its raw log directories by exporting `LUNARWING_SERVICE_LOG_DIRS`.
Never export `SANDBOX_PROTECTED_PATHS` from an init script: dotenvy does not
override, so it would shadow the operator's `<base>/.env` value. All daemon
log sinks share `tracing_fmt::daemon_log_scrubber`; keep new sinks on it, and
register secrets learned at runtime through `tracing_fmt::register_log_secret`.

## BOUNDARY: generic sandbox vs. orchestrator worker containers

TWO container subsystems exist; never mix them:
- `src/sandbox/` — generic sandbox (CLI runner, `lunarwing.sandbox` labels,
  shell tool consumer).
- `src/orchestrator/job_manager.rs` — live worker jobs (bollard Docker API,
  `lunarwing.job_id` labels, job tokens).

Do not pass `SandboxManager`/`CliRunner` into the orchestrator, and do not
route shell execution through `ContainerJobManager`. The reaper
(`src/orchestrator/reaper.rs`) is the only shared component and scans the two
label namespaces separately. The generic-sandbox CLI sweep is independent of
the Docker-compatible API: on socketless podman deployments it runs as a
standalone loop (`SandboxReaper::run_cli_sweep_loop`) and scans the runtime
the sandbox runner actually selected (WI#179).

## Zero-Exposure Credential Model

Secrets are stored encrypted on the host and injected into HTTP requests by the proxy at transit time. Container processes never see raw credential values. Proxy denials are returned with the body prefix `sandbox-proxy denied: <reason>` so sandboxed agents can attribute them.
