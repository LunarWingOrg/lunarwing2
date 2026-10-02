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
1. **Sanitizer** - Detects injection patterns, escapes dangerous content
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

- `none` (default): `--network none` — kernel-enforced isolation, no egress,
  no proxy started.
- `proxy-enforced` (WI#155): container keeps `--network none`; the only egress
  path is a per-execution Unix-socket relay to the validating proxy (public-only,
  DNS-pinned, redirect rechecked). Requires a local Linux runtime and Python 3
  in the image; relay setup failures fail closed (`[sandbox:proxy_error]`),
  never degrade to advisory or direct egress.
- `tunnel` (WI#248): sandbox runs join a long-lived tunnel sidecar container's
  network namespace (`--network container:lunarwing-sandbox-tunnel`; NetBird or
  kernel WireGuard inside a userns-owned netns). The sidecar is the only
  credential holder; runs keep every hardening flag. Egress policy is the
  tunnel's (`AllowedIPs` / NetBird ACLs) — enforced when the default route
  rides the tunnel, otherwise advisory. Health-gated start fails closed
  (`[sandbox:sandbox_unavailable]`), never degrades.
- `proxy`: legacy allowlist HTTP proxy + credential injection. **Advisory** —
  enforced via env vars, which a process may ignore.

**Default (WI#154/155):** the in-code default and every mt-admin-rendered
tenant is `none` (mt-admin preserves an operator-set `proxy` on re-render;
upgrades preserve an explicit `proxy`). `proxy` is advisory; use
`proxy-enforced` when sandboxed processes need allowlisted web access — it is
network-isolated everywhere except a Unix relay to the public-only proxy. The
relay socket lives under `$XDG_RUNTIME_DIR` when available so it stays visible
to both a `PrivateTmp` daemon and manager-forked workload namespaces.

**Hardening highlights:** explicit `--pids-limit 256`, `--stop-timeout 10`,
`--entrypoint sh`, `--workdir /workspace`, explicit `--user` resolved per
runtime (keep-id / rootless mapping / daemon euid). Rootful Docker under a
root daemon refuses writable workspaces (never container root). Detached
cleanup is backstopped by the API-independent CLI reaper with bounded probes.

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
