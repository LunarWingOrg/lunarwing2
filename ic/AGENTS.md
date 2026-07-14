# Agent Rules

## Purpose and Precedence

- `AGENTS.md` is the quick-start contract for coding agents. It is not the full architecture spec.
- Read the relevant subsystem spec before changing a complex area. When a repo spec exists, treat it as authoritative.
Start with these deeper docs as needed:
- `src/agent/CLAUDE.md`
- `src/channels/web/CLAUDE.md`
- `src/db/CLAUDE.md`
- `src/llm/CLAUDE.md`
- `src/setup/README.md`
- `src/tools/README.md`
- `src/workspace/README.md`
- `src/NETWORK_SECURITY.md`
- `tests/e2e/CLAUDE.md`
- `crates/lunarwing_engine/CLAUDE.md`
- In this repo, `ic/` contains the core daemon; however, the product name is LunarWing.

## Architecture Mental Model

- Channels normalize external input into `IncomingMessage`; `ChannelManager` merges all active channel streams.
- `Agent` owns session/thread/turn handling, submission parsing, the LLM/tool loop, approvals, routines, and background runtime behavior.
- `AppBuilder` is the composition root that wires database, secrets, LLMs, tools, workspace, extensions, skills, hooks, and cost controls before the agent starts.
- The web gateway is a browser-facing API/UI layered on top of the same agent/session/tool systems, not a separate product path.

## Where to Work

- Agent/runtime behavior: `src/agent/`
- Web gateway/API/SSE/WebSocket: `src/channels/web/`
- Persistence and DB abstractions: `src/db/`
- Setup/onboarding/configuration flow: `src/setup/`
- LLM providers and routing: `src/llm/`
- Workspace, memory, embeddings, search: `src/workspace/`
- Extensions, tools, channels, MCP, WASM: `src/extensions/`, `src/tools/`, `src/channels/`
- Docker sandbox and network proxy: `src/sandbox/`
- Container orchestrator and external workers: `src/orchestrator/`
- Secrets management: `src/secrets/`
- Lifecycle hooks: `src/hooks/`
- Tunnel abstraction (cloudflare, ngrok, tailscale): `src/tunnel/`
- SKILL.md prompt extensions: `src/skills/`
- Engine V2 bridge: `src/bridge/`
- Execution gate and approval pending state: `src/gate/`
- DM pairing for channels: `src/pairing/`
- Webhook ingress for tools: `src/webhooks/`
- Observability: `src/observability/`
- Extension registry catalog: `src/registry/`
- OpenClaw port staging work: `ic/openclaw-ports/`. For OpenClaw port tasks, keep edits inside `ic/openclaw-ports/` unless the user explicitly approves touching core LunarWing files.
- Using /tmp as a place to store logs or other small files is totally acceptabl
e. However, in general, storing massive files (over 1GB) to /tmp should not happen. You shouldn't be building entire rust binaries to /tmp - it's really stupid.

## Ownership and Composition Rules

- Keep `src/main.rs` and `src/app.rs` orchestration-focused. Do not move module-owned logic into entrypoints.
- Module-specific initialization should live in the owning module behind a public factory/helper, not be reimplemented ad hoc.
- Keep feature-flag branching inside the module that owns the abstraction whenever possible.
- Prefer extending existing traits and registries over hardcoding one-off integration paths.

## Repo-Wide Coding Rules

- **Edition**: Rust 2024, MSRV 1.92.
- **Formatting**: Standard `rustfmt`. Run `cargo fmt --all` before committing.
- **Imports**: Prefer `crate::` for cross-module references. Group std, external, then internal crates.
- **Error handling**: Use `thiserror` for structured errors and `anyhow` for propagation. Avoid `.unwrap()` and `.expect()` in production; they are fine in tests and for truly infallible invariants (e.g., literals/regexes) with a safety comment.
- **Types**: Use strong types and enums over stringly-typed control flow when the shape is known.
- **Naming**: Follow standard Rust conventions (`snake_case` for functions/variables, `PascalCase` for types/traits, `SCREAMING_SNAKE_CASE` for constants).
- **Complexity**: Keep functions under 100 lines, cognitive complexity under 15, and arguments under 7.
- **Secrets**: Use the `secrecy` crate for sensitive values; never log or expose secrets.
- **Logging**: Use `tracing` macros (`info!`, `warn!`, `error!`) rather than `println!`.
- Keep clippy clean with zero warnings.

## Database, Setup, and Config Rules

- PostgreSQL is the primary backend and is far more heavily supported. libSQL support is aspirational — not every feature needs it, but do not regress existing libSQL coverage when possible.
- Add new DB operations to the shared DB trait first, then implement both backends. If libSQL work would be disproportionate to the feature, PostgreSQL-only is acceptable with a note in the relevant spec.
- Treat bootstrap config, DB-backed settings, and encrypted secrets as distinct layers; do not collapse them casually.
- If onboarding or setup behavior changes, update `src/setup/README.md` in the same branch.
- Do not break config precedence, bootstrap env loading, DB-backed config reload, or post-secrets LLM re-resolution.

## Security and Runtime Invariants

- Review any change touching listeners, routes, auth, secrets, sandboxing, approvals, or outbound HTTP with a security mindset.
- Do not weaken bearer-token auth, webhook auth, CORS/origin checks, body limits, rate limits, allowlists, or secret-handling guarantees.
- Treat Docker containers and external services as untrusted.
- Session/thread/turn state matters. Submission parsing happens before normal chat handling.
- Skills are selected deterministically. Tool approval and auth flows are special paths and must not be mixed into normal chat history carelessly.
- Persistent memory is the workspace system, not just transcript storage; preserve file-like semantics, chunking/search behavior, and identity/system-prompt loading.

## Tools, Channels, and Extensions

- Use a built-in Rust tool for core internal capabilities tightly coupled to the runtime.
- Use WASM tools or WASM channels for sandboxed extensions and plugin-style integrations.
- Use MCP for external server integrations when the capability belongs outside the main binary.
- Preserve extension lifecycle expectations: install, authenticate/configure, activate, remove.
- For any library, framework, SDK, or crate documentation lookup, use Context7 MCP (`context7_resolve-library-id` → `context7_query-docs`) before relying on internal knowledge. Verify API signatures, feature flags, and version-specific behavior against the resolved docs. However, keep in mind that even libraries from context7 (especially for the LunarWing project) can also be woefully out of date. As such, it's best to always VERIFY any information obtained from context7.

## Local XMPP and Service Operations

- Treat systemd unit environment values as secret-bearing. Do not paste passwords, bearer tokens, or webhook secrets into user-facing output; summarize or redact them.
- **Both systemd and OpenRC must be supported.** systemd is the primary target and can be fully verified on the dev VM. OpenRC code must still be correct and maintained — the dev VM cannot run OpenRC tests, but OpenRC-specific logic should be kept in the same service-management abstraction rather than duplicated.
- `xmpp-bridge.service` is coupled to `lunarwing.service` with `PartOf=lunarwing.service`, so LunarWing restarts can also restart the bridge. Do not assume the bridge caused a LunarWing stop just because both units restarted together.
- For install-style harness tests on Linux, prefer the rendered service units over leaving `scripts/lunarwing-xmpp-test-env.sh up` attached to a transient shell. The durable path is `render-systemd` plus `systemctl --user` on systemd hosts; OpenRC validation should use `lunarwing service install` or the committed OpenRC templates.
- The harness and service path intentionally seed `ALLOW_PRIVATE_IPS=1`, `DATABASE_SSLMODE=disable`, and `PGSSLMODE=disable` for private-network Postgres/TensorZero test setups. Preserve those defaults unless the task explicitly changes the network or SSL assumptions.
- Use `scripts/xmpp-rate-limit.sh` for live XMPP outbound rate-limit changes. It requires `XMPP_BRIDGE_TOKEN`; `status`, `set <n>`, `off`, and `reset` are the main commands.
- Use `scripts/xmpp-configure.sh` for bridge room/configuration checks and configure calls when working with the existing XMPP bridge API.
- The local service watchdog assets are `scripts/lunarwing-watchdog.sh`, `scripts/lunarwing-watchdog-openrc.sh`, `scripts/install-lunarwing-watchdog.sh`, `systemd/lunarwing-watchdog.service`, `systemd/lunarwing-watchdog.timer`, `systemd/lunarwing-watchdog.confd`, and `systemd/lunarwing-watchdog.cron.hourly`. The installer auto-detects `systemd` vs `OpenRC`; on OpenRC it also auto-selects the scheduler. `auto` prefers an existing `cronie`/`crond`/`dcron` hourly setup and only falls back to a managed `fcron` entry when that avoids interfering.
- Prefer read-only diagnostics first for **single-node / harness** service issues: `systemctl --user status` (or the matching OpenRC path), `journalctl --user`, and bridge status endpoints. For **multi-tenant** hosts always use `scripts/lunarwing-mt-admin.sh status|…` — never bare system-bus `systemctl` against tenant units (see Multi-Tenant Ops below). Only restart services after identifying unit state or when the user explicitly asks.
- If harness `verify` only fails the TensorZero proxy check, inspect the upstream `TENSORZERO_URL` before treating the local service install as broken. The local proxy can be bound and healthy while the upstream `/openai/v1/models` probe still returns `500`.

## Multi-Tenant Ops, Init Agnosticism, and Host Health (HARD RULE)

This is **not** a lowest-common-denominator product. Multi-tenant LunarWing runs on **real** init systems — **systemd user managers** and **OpenRC (Gentoo)** — with per-tenant port blocks from `/etc/lunarwing/ports.json`. Agents must not write code that only works on a single-node system-bus systemd box.

### Forbidden

- Bare `systemctl is-active` / `systemctl status` / `systemctl restart` against **tenant** units from wrappers, verify helpers, or one-off scripts (system bus only — false-fails healthy tenants).
- Bare `rc-service` probes that reimplement what `lunarwing-mt-admin.sh` already does.
- Assuming tenant units live on the **system** bus. They do not. On systemd they are **`systemctl --user`** under the tenant (`XDG_RUNTIME_DIR=/run/user/<uid>`, linger). On Gentoo they are OpenRC services.
- Hardcoding single-node host ports for multi-tenant health probes — especially LunarVision **`http://127.0.0.1:8088`**. On MT hosts each tenant publishes OCR health on registry **`vision_health`** (e.g. lunarium → `20006`), not 8088.
- Setting `HEALTH_LUNARVISION_URL=http://127.0.0.1:8088` in `/etc/lunarwing/health.env` on multi-tenant fleets (forces false-critical overall health while tenant sidecars are fine).
- Reimplementing tenant lifecycle (add/build/start/stop/status/ports) outside `scripts/lunarwing-mt-admin.sh`.

### Required

- Tenant lifecycle / status / operator health → **`scripts/lunarwing-mt-admin.sh`** (`status <tenant>`, start/stop/build, doctor, health pipeline install). Override path with `LUNARWING_MT_ADMIN` if needed.
- Host-global pipeline lives under `../ic-infrastructure-health-check/` (installed to `/usr/local/lib/lunarwing-health/` via mt-admin). It must stay **init-agnostic** and **ports-registry-aware**.
- `health-lunarvision.sh`: leave **`HEALTH_LUNARVISION_URL` unset** on MT hosts so it auto-discovers every tenant's `vision_health` (fallback `vision_service`) from `SELF_HEAL_TENANTS_FILE` / `/etc/lunarwing/ports.json`. Explicit URL is only for intentional single-target override; empty registry falls back to single-node `8088`.
- Wrappers (`lunarwing_mt_onboard*`) are **thin drivers** of mt-admin — no parallel init or port logic.
- Parse both init status shapes when reading mt-admin output (`….service: active` and OpenRC `…: started`), or use exit codes / structured contracts.
- **Both systemd and OpenRC are first-class.** Never treat Gentoo/OpenRC as an edge case.

### Why this is non-negotiable

1. Tenant `lunarium` was healthy (`mt-admin status` active, user units running) while onboard **verify** false-failed on bare system-bus `systemctl is-active`.
2. The same host's **health timer** reported overall **critical** because lunarvision still probed **8088** while the real sidecar answered on **`vision_health` 20006**. Units were fine; the probe was single-node-default wrong.

That class of bug must not reappear.

### Quick greps before you ship

```bash
# Must not appear in onboard/web/verify wrappers:
rg -n 'systemctl is-active|systemctl status|rc-service' ../lunarwing_mt_onboard ../lunarwing_mt_onboard_web

# LunarVision must not reintroduce hard-only 8088 without registry discovery:
rg -n '127\.0\.0\.1:8088' ../ic-infrastructure-health-check/health-lunarvision.sh
# (8088 as single-node *fallback after* ports-registry discovery is OK; sole/default-without-registry path must stay documented.)
```

Exceptions: comments/docs forbidding the pattern; init helpers **inside** `scripts/lunarwing-mt-admin.sh` (e.g. `_systemctl_user`); infrastructure-health tooling that already abstracts user units / OpenRC / ports.json correctly.

## Docs, Parity, and Testing

- If behavior changes, update the relevant docs/specs in the same branch.
- Add the narrowest tests that validate the change: unit tests for local logic, integration tests for runtime/DB/routing behavior, and E2E or trace coverage for gateway, approvals, extensions, or other user-visible flows.

## Risk and Change Discipline

- Keep changes scoped; avoid broad refactors unless the task truly requires them.
- Security, database schema, runtime, worker, CI, and secrets changes are high-risk. Call out rollback risks, compatibility concerns, and hidden side effects.
- Preserve existing defaults unless the task explicitly changes them.
- Avoid unrelated file churn and generated-file edits unless required.
- Respect a dirty worktree and never revert user changes you did not make.

## Before Finishing

- Run the most targeted tests/checks that cover the change.
- Re-check security-sensitive paths when touching auth, secrets, network listeners, sandboxing, or approvals.
- Keep the final diff scoped to the task.
