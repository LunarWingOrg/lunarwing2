# Agent Rules

## Quick tips
- When working with LunarWing, use the lunarwing study-notes MCP if it is available and mark notes that may be stale as "stale" (if possible)
- When working with LunarWing, use the context7 MCP if it is available https://context7.com/git_git_lunarwing_org/lunarwing_lunarwing2
- When working with LunarWing, use memory search if it is available
- use tmux for long running builds/commands or else timeout will occur

## Purpose and Precedence

### Some information may be outdated

#### Some crappy plugins modify this file without real reason and fill it with GARBAGE. They should STOP BEING USED

- `AGENTS.md` is the quick-start contract for coding agents. It is not the full architecture spec.
- Read the relevant subsystem spec before changing a complex area. When a repo spec exists, treat it as authoritative.
Start with these deeper docs as needed (they can be outdated — verify anything you read against the tree):
- `ic/src/db/CLAUDE.md`
- `ic/src/setup/README.md`
- `ic/src/tools/README.md`
- `ic/src/workspace/README.md`
- `ic/src/NETWORK_SECURITY.md`
No specs exist yet for the agent, web-gateway, llm, e2e, or engine subsystems — read the code instead.
- In this repo, `ic/` contains the core daemon; however, the product name is LunarWing.

## Architecture Mental Model

- Channels normalize external input into `IncomingMessage`; `ChannelManager` merges all active channel streams.
- `Agent` owns session/thread/turn handling, submission parsing, the LLM/tool loop, approvals, routines, and background runtime behavior.
- `AppBuilder` is the composition root that wires database, secrets, LLMs, tools, workspace, extensions, skills, hooks, and cost controls before the agent starts.
- The web gateway is a browser-facing API/UI layered on top of the same agent/session/tool systems, not a separate product path.

## Where to Work - some places to check

- Agent/runtime behavior: `ic/src/agent/`
- Web gateway/API/SSE/WebSocket: `ic/src/channels/web/`
- Persistence and DB abstractions: `ic/src/db/`
- Setup/onboarding/configuration flow: `ic/src/setup/`
- LLM providers and routing: `ic/src/llm/`
- Workspace, memory, embeddings, search: `ic/src/workspace/`
- Extensions, tools, channels, MCP, WASM: `ic/src/extensions/`, `ic/src/tools/`, `ic/src/channels/`
- Docker sandbox and network proxy: `ic/src/sandbox/`
- Container orchestrator and external workers: `ic/src/orchestrator/`
- Secrets management: `ic/src/secrets/`
- Lifecycle hooks: `ic/src/hooks/`
- Tunnel abstraction (NetBird, custom commands, static URLs): `ic/src/tunnel/`
- SKILL.md prompt extensions: `ic/src/skills/`
- Engine V2 bridge: `ic/src/bridge/`
- Execution gate and approval pending state: `ic/src/gate/`
- DM pairing for channels: `ic/src/pairing/`
- Webhook ingress for tools: `ic/src/webhooks/`
- Observability: `ic/src/observability/`
- Extension registry catalog: `ic/src/registry/`
- OpenClaw port staging work: `ic/openclaw-ports/`. For OpenClaw port tasks, keep edits inside `ic/openclaw-ports/` unless the user explicitly approves touching core LunarWing files.
- DO NOT USE FULL DEBUG BUILDS DO NOT USE FULL DEBUG BUILDS. WASTEFUL AND TO BE AVOIDED AT ALL COSTS

## Build Constraints (Gentoo Dev VM)

This machine is a dedicated dev/test machine with limited resources. **All cargo commands must follow these rules:**

- **12 threads max**: prefix every cargo command with `taskset -c 0-11`
- **Use `cargo check` for compile verification, NOT `cargo build`** — DO NOT USE FULL DEBUG BUILDS DO NOT USE FULL DEBUG BUILDS. WASTEFUL AND TO BE AVOIDED AT ALL COSTS. Reserve release builds (`cargo build --release`) for deploying to a new tenant or upgrading an existing tenant's release binary.
- **Use `taskset -c 0-11` for every cargo command**, not just `cargo build`. This applies to `cargo check`, `cargo test`, `cargo clippy`, `cargo doc`, etc.
- DO NOT USE FULL DEBUG BUILDS DO NOT USE FULL DEBUG BUILDS. WASTEFUL AND TO BE AVOIDED AT ALL COSTS
- Using /tmp as a place to store logs or other small files is totally acceptable. However, in general, storing massive files (over 1GB) to /tmp should not happen. You shouldn't be building entire rust binaries to /tmp - it's really stupid so do not do this ever

```bash
taskset -c 0-11 cargo check -j12                              # compile check
taskset -c 0-11 cargo check -j12 --no-default-features --features postgres  # postgres-only
taskset -c 0-11 cargo check -j12 --no-default-features --features libsql    # libsql-only
taskset -c 0-11 cargo check -j12 --all-features               # all features
taskset -c 0-11 cargo test -j12 -- --test-threads=12            # unit tests
taskset -c 0-11 cargo clippy -j12 --all --benches --tests --examples -- -D warnings  # lint
taskset -c 0-11 cargo clippy -j12 --all --benches --tests --examples --all-features -- -D warnings
```

Long-running commands (5–20+ minutes) **must use tmux**:
```bash
tmux new-session -d -s build "taskset -c 0-11 cargo build --release -j12 2>&1 | tee /tmp/build.log"
```

## Vendored Dependencies

- crates-io is source-replaced by the committed directory `ic/vendor/` via `ic/.cargo/config.toml`. All cargo commands run under `ic/` resolve exclusively from `ic/vendor/` — no crates.io access needed (or permitted) for check/test/build/clippy.
- After any dependency add/update, refresh the vendor dir with `ic/scripts/refresh-vendor.sh` (runs `cargo vendor` over the ic workspace + satellite lockfile roots under `ic/`, asserts the resulting set matches the committed Cargo.lock pins — it reports drift only, never re-resolves — then runs `cargo deny check advisories`). Review the `ic/vendor/` diff and commit it together with the manifest/lockfile change. Advisory scanning keeps working unchanged (`cargo deny check advisories` reads Cargo.lock vs the RustSec db, not crates.io).
- Never hand-edit or delete entries under `ic/vendor/`. `ic/vendor/libsignal-protocol{,-sys}` are pre-vendored **path** dependencies (`ic/Cargo.toml`) — keep them, and always vendor with `--no-delete`. The one sanctioned exception is the WI#292 `ic/vendor/rig-core/` patch (see its `LUNARWING-PATCH.md`): `refresh-vendor.sh` verifies it pre-vendor and restores the committed patched tree from git post-vendor, so commit any rig-core patch changes before refreshing.
- cargo never re-checks vendored sources (it treats every crates.io `name@version` as immutable). A crate patched in place without a version change therefore leaves stale artifacts in every reused target dir, and CI, which builds from scratch, cannot see it. `ic/scripts/vendor-artifact-guard.sh` hashes each vendored crate's `.cargo-checksum.json`, records the hashes per target dir and profile, and runs `cargo clean -p` on crates that changed. It is wired into mt-admin `build_tenant` (build-tenant/upgrade-tenant), `build-wasm-extensions.sh`, `scripts/build-lunarwing.sh`, `build-all.sh` and `branch-test-loop.sh`. Any in-place vendor patch must ship a `LUNARWING-PATCH.md` so the guard's first run on an existing target dir cleans it. In a dev checkout, if `cargo check` reports items missing from a patched vendored crate, run `taskset -c 0-11 bash ic/scripts/vendor-artifact-guard.sh --profile dev` (or `cargo clean -p <crate>`).
- `ic/vendor/` is pinned third-party source: repo-wide CI scanners (`.gitlab/scripts/scan-secrets.sh`, `.gitlab/scripts/shell-syntax.sh`, `.gitlab/scripts/python_syntax.py`, `.gitlab/scripts/shellcheck.sh`) exclude it. Keep any new repo-wide scanner vendor-aware.

## Build, Test, and Lint Commands

Run these from the `ic/` directory. Apply `taskset -c 0-11` and `-j12` per the build constraints above.

```bash
# Compile check (preferred over cargo build for verification)
cargo check
cargo check --all-features

# Build (release only when needed for deploy)
cargo build --release --bin lunarwing

# Run all tests
cargo test -- --nocapture

# Run a single test (exact match)
cargo test <test_name> -- --exact --nocapture

# Run a specific integration test file
cargo test --test <file_name> -- --nocapture

# Run tests with specific features
cargo test --no-default-features --features postgres
cargo test --all-features

# Format check
cargo fmt --all -- --check

# Lint (zero warnings policy)
cargo clippy --all --benches --tests --examples -- -D warnings
cargo clippy --all --benches --tests --examples --all-features -- -D warnings

# Dependency audit
cargo deny check

# Compile benchmarks without running
cargo bench --all-features --no-run

# Build WASM extensions (needed for some integration tests)
./scripts/build-wasm-extensions.sh             # all (tools + channels)
./scripts/build-wasm-extensions.sh --tools     # tools only
./scripts/build-wasm-extensions.sh --channels  # channels only
```

*DO NOT USE FULL DEBUG BUILDS DO NOT USE FULL DEBUG BUILDS. WASTEFUL AND TO BE AVOIDED AT ALL COSTS*

## Ownership and Composition Rules

- Keep `ic/src/main.rs` and `ic/src/app.rs` orchestration-focused. Do not move module-owned logic into entrypoints.
- Module-specific initialization should live in the owning module behind a public factory/helper, not be reimplemented ad hoc.
- Keep feature-flag branching inside the module that owns the abstraction whenever possible.
- Prefer extending existing traits and registries over hardcoding one-off integration paths.

## Repo-Wide Coding Rules

- **Edition**: Rust 2024, MSRV 1.96.1
- **Formatting**: Standard `rustfmt`. Run `cargo fmt --all` before committing.
- **Imports**: Prefer `crate::` for cross-module references. Group std, external, then internal crates.
- **Error handling**: Use `thiserror` for structured errors and `anyhow` for propagation. Avoid `.unwrap()` and `.expect()` in production; they are allowed only in tests or for truly infallible invariants (e.g., literals/regexes) with a safety comment.
- **Types**: Use strong types and enums over stringly-typed control flow when the shape is known.
- **Naming**: Follow standard Rust conventions (`snake_case` for functions/variables, `PascalCase` for types/traits, `SCREAMING_SNAKE_CASE` for constants).
- **Complexity**: Keep functions under 100 lines, cognitive complexity under 15, and arguments under 7 (see `ic/clippy.toml`).
- **Secrets**: Use the `secrecy` crate for sensitive values; never log or expose secrets.
- **Logging**: Use `tracing` macros (`info!`, `warn!`, `error!`) rather than `println!`.
- **Compile-time directory embeds** (refinery migrations, registry manifests, skills) need a directory-level `cargo:rerun-if-changed` in `ic/build.rs`. Per-file watches miss newly added files, so reused target dirs (tenant upgrades) ship without them while clean CI builds pass.
- Keep clippy clean with zero warnings.

## Database, Setup, and Config Rules

- PostgreSQL is the primary backend and is far more heavily supported. libSQL support is aspirational — not every feature needs it, but do not regress existing libSQL coverage when possible.
- Add new DB operations to the shared DB trait first, then implement both backends. If libSQL work would be disproportionate to the feature, PostgreSQL-only is acceptable with a note in the relevant spec.
- Treat bootstrap config, DB-backed settings, and encrypted secrets as distinct layers; do not collapse them casually.
- If onboarding or setup behavior changes, update `ic/src/setup/README.md` in the same branch.
- Do not break config precedence, bootstrap env loading, DB-backed config reload, or post-secrets LLM re-resolution.

## Security and Runtime Invariants

- Review any change touching listeners, routes, auth, secrets, sandboxing, approvals, or outbound HTTP with a security mindset.
- Do not weaken bearer-token auth, webhook auth, CORS/origin checks, body limits, rate limits, allowlists, or secret-handling guarantees.
- Treat Docker and podman containers and external services as untrusted.
- Session/thread/turn state matters. Submission parsing happens before normal chat handling.
- Skills are selected deterministically. Tool approval and auth flows are special paths and must not be mixed into normal chat history carelessly.
- Persistent memory is the workspace system, not just transcript storage; preserve file-like semantics, chunking/search behavior, and identity/system-prompt loading.
- **Workers must never self-expose (WI#357).** LunarWing owns every public exposure path through `ic/src/tunnel/`; external workers (opencode, opencode2, kimi, pebble) must never open their own inbound path (tunnel, relay, pairing links, public listener) or publish session content to third-party share services. The only legitimate instruction channel into a worker is daemon job dispatch → in-container bridge → the worker's agent API on loopback. Enforced by the opencode2 lane guards (`opencode2.4lunarwing/scripts/opencode-exposure-guard.sh`, `exposure_policy_assert.py`, `compile_out_remote_tunnel.py`), the `worker-exposure-policy-guard` CI job, and `mt-admin worker-listeners <tenant>`; policy and bump-checklist details live in `docs/ops/WORKER-CONTAINERS.md`.
- *DO NOT USE FULL DEBUG BUILDS DO NOT USE FULL DEBUG BUILDS. WASTEFUL AND TO BE AVOIDED AT ALL COSTS*

## Tools, Channels, and Extensions

- Use a built-in Rust tool for core internal capabilities tightly coupled to the runtime.
- Use WASM tools or WASM channels for sandboxed extensions and plugin-style integrations.
- Use MCP for external server integrations when the capability belongs outside the main binary.
- Preserve extension lifecycle expectations: install, authenticate/configure, activate, remove.
- For any library, framework, SDK, or crate documentation lookup, use Context7 MCP (`context7_resolve-library-id` → `context7_query-docs`) before relying on internal knowledge. Verify API signatures, feature flags, and version-specific behavior against the resolved docs. However, keep in mind that even libraries from context7 (especially for the LunarWing project) can also be woefully out of date. As such, it's best to always VERIFY any information obtained from context7.

## Local XMPP and Service Operations

- Treat systemd unit environment values as secret-bearing. Do not paste passwords, bearer tokens, or webhook secrets into user-facing output; summarize or redact them.
- **Both systemd and OpenRC must be supported.** launchd is a distant third — prioritize OpenRC and systemd.
- `xmpp-bridge.service` is coupled to `lunarwing.service` with `PartOf=lunarwing.service`, so LunarWing restarts can also restart the bridge. Do not assume the bridge caused a LunarWing stop just because both units restarted together.
- For install-style harness tests, prefer the rendered service units over leaving `ic/scripts/lunarwing-xmpp-test-env.sh up` attached to a transient shell. The durable path is `render-systemd` plus `systemctl --user` on systemd hosts; `render-launchd` plus `launchctl` on macOS; OpenRC validation should use `lunarwing service install` or the committed OpenRC templates.
- The harness and service path intentionally seed `ALLOW_PRIVATE_IPS=1`, `DATABASE_SSLMODE=disable`, and `PGSSLMODE=disable` for private-network Postgres/TensorZero test setups. Preserve those defaults unless the task explicitly changes the network or SSL assumptions.
- Use `ic/scripts/xmpp-rate-limit.sh` for live XMPP outbound rate-limit changes. It requires `XMPP_BRIDGE_TOKEN`; `status`, `set <n>`, `off`, and `reset` are the main commands.
- Use `ic/scripts/xmpp-configure.sh` for bridge room/configuration checks and configure calls when working with the existing XMPP bridge API.
- The local service watchdog assets are `ic/scripts/lunarwing-watchdog.sh` (systemd), `ic/scripts/lunarwing-watchdog-openrc.sh` (OpenRC), `ic/scripts/lunarwing-watchdog-launchd.sh` (macOS), `ic/scripts/install-lunarwing-watchdog.sh`, `ic/systemd/lunarwing-watchdog.service`, `ic/systemd/lunarwing-watchdog.timer`, `ic/systemd/lunarwing-watchdog.confd`, `ic/systemd/lunarwing-watchdog.cron.hourly`, and `ic/systemd/com.lunarwing.watchdog.plist` (launchd). The installer auto-detects `systemd` vs `OpenRC` vs `launchd`; on OpenRC it also auto-selects the scheduler. `auto` prefers an existing `cronie`/`crond`/`dcron` hourly setup and only falls back to a managed `fcron` entry when that avoids interfering.
- The healthpoint metrics exporter assets are `ic-infrastructure-health-check/health-metrics-exporter.sh` (Prometheus textfile export, runs from `ic-infrastructure-health-check/cron-wrapper.sh`), the optional loopback listener crate `ic/crates/lunarwing_health_metrics`, its installer `ic/scripts/install-health-metrics-listener.sh` + units under `ic/systemd/lunarwing-health-metrics.*`, and the ports-registry v16 `health_metrics` slot (`ic/scripts/migrate-ports-v16.sh`). Design: `docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md`.
- Prefer read-only diagnostics first for **single-node / harness** service issues: `systemctl --user status` (or the matching OpenRC path), `journalctl --user`, and bridge status endpoints. For **multi-tenant** hosts always use `ic/scripts/lunarwing-mt-admin.sh status|…` — never bare system-bus `systemctl` against tenant units (see Multi-Tenant Ops below). Only restart services after identifying unit state or when the user explicitly asks.
- If harness `verify` only fails the TensorZero proxy check, inspect the upstream `TENSORZERO_URL` before treating the local service install as broken. The local proxy can be bound and healthy while the upstream `/openai/v1/models` probe still returns `500`.

## Multi-Tenant Ops and Init Agnosticism (HARD RULE)

This is **not** a lowest-common-denominator product. Multi-tenant LunarWing runs on **real** init systems, including **systemd (user manager)** and **OpenRC (Gentoo)**. Agents must not write code that only works on "normal" system-bus systemd hosts.

### Forbidden

- Bare `systemctl is-active`, `systemctl status`, `systemctl show`, or `systemctl restart` against tenant units from Python/CLI/web wrappers (e.g. `lunarwing_mt_onboard*`, health helpers outside mt-admin).
- Bare `rc-service` / OpenRC probes that reimplement what mt-admin already does.
- Assuming tenant units live on the **system** bus. They do not. On systemd hosts they are **`systemctl --user`** units under the tenant account (`XDG_RUNTIME_DIR=/run/user/<uid>`, linger enabled). On Gentoo they are OpenRC services.
- "Works on my Fedora/Ubuntu box" shortcuts that ignore OpenRC or user-manager paths.
- Reimplementing tenant lifecycle (add/build/start/stop/status/ports) outside `ic/scripts/lunarwing-mt-admin.sh`.
- Hardcoding single-node host ports for multi-tenant health probes — especially LunarVision `http://127.0.0.1:8088`. On MT hosts each tenant publishes OCR health on registry `vision_health` (not 8088). Leave `HEALTH_LUNARVISION_URL` unset so `ic-infrastructure-health-check/health-lunarvision.sh` discovers ports from the registry.

### Required

- Tenant lifecycle, status, and health checks go through **`ic/scripts/lunarwing-mt-admin.sh`** (override path with `LUNARWING_MT_ADMIN` if needed). Prefer `status <tenant>`, start/stop/build, etc.
- LunarVision managed tenants publish distinct API/health ports, health-gate starts, and recreate stale containers when image/env/port contracts change. systemd uses user Quadlet or the managed older-Podman fallback; OpenRC pairs its health unit with a `-sup` crash supervisor.
- Wrappers (`lunarwing_mt_onboard`, `lunarwing_mt_onboard_web`, other orchestration) are **thin drivers** of mt-admin. They must not invent parallel init logic.
- If you must parse status text, handle **both** init shapes (e.g. systemd `lunarwing-<t>.service: active` and OpenRC `lunarwing-<t>: started`), or better: use mt-admin exit codes / structured contracts when available.
- Init-specific code belongs **inside** `ic/scripts/lunarwing-mt-admin.sh` (and related infra-health tooling), behind helpers like `_systemctl_user` — not scattered through onboard/web/verify layers.
- **Both systemd and OpenRC are first-class.** launchd is a distant third. Never treat OpenRC/Gentoo as an edge case.
- Host health pipeline (`ic-infrastructure-health-check/`, installed via mt-admin) must stay ports-registry-aware. Empty registry uses the dedicated single-node operational listener on `8089`; never restore `8088` as the default health target.

### Why this is non-negotiable

A real provision of tenant `lunarium` was healthy (`mt-admin status` active, user unit running, HTTP agent status OK) while post-provision verify **false-failed** because it called bare `systemctl is-active` on the system bus. That class of bug must not reappear.

### Quick greps before you ship

```bash
# These should not appear in onboard/web/verify wrappers:
rg -n 'systemctl is-active|systemctl status|rc-service' lunarwing_mt_onboard lunarwing_mt_onboard_web
```

Exceptions: comments/docs forbidding the pattern; code inside `ic/scripts/lunarwing-mt-admin.sh` and dedicated infrastructure-health tooling that already abstracts init correctly.

## Docs, Parity, and Testing

- If behavior changes, update the relevant docs/specs in the same branch.
- Add the narrowest tests that validate the change: unit tests for local logic, integration tests for runtime/DB/routing behavior, and E2E or trace coverage for gateway, approvals, extensions, or other user-visible flows.
- *DO NOT USE FULL DEBUG BUILDS DO NOT USE FULL DEBUG BUILDS. WASTEFUL AND TO BE AVOIDED AT ALL COSTS*

## Risk and Change Discipline

- Keep changes scoped; avoid broad refactors unless the task truly requires them.
- Security, database schema, runtime, worker, CI, and secrets changes are high-risk. Call out rollback risks, compatibility concerns, and hidden side effects.
- Preserve existing defaults unless the task explicitly changes them.
- Avoid unrelated file churn and generated-file edits unless required.
- Respect a dirty worktree and never revert user changes you did not make.
- *DO NOT USE FULL DEBUG BUILDS DO NOT USE FULL DEBUG BUILDS. WASTEFUL AND TO BE AVOIDED AT ALL COSTS*

## Before Finishing

- Run the most targeted tests/checks that cover the change.
- Re-check security-sensitive paths when touching auth, secrets, network listeners, sandboxing, or approvals.
- Keep the final diff scoped to the task.

