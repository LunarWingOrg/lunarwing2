# Release Notes for LunarWing v2.1.2.0 (DRAFT)

## Codename: Yoroi (鎧, armor)

**Release date** 2026-09-23

> After the lacing, the plates
>
> Each hammer-blow remembered
>
> Not one gap remains
>
> 鎧

## Overview

LunarWing v2.1.2.0 is an isolation, sandboxing, worker-contract, health
system, and confabulation-hardening release — the largest goals checklist in
the project's history at fifty-nine items, worked by the widest herd yet. Its
headline work is the generic Podman-first agent-shell sandbox: daemonless
per-command execution with fail-closed defaults, daemon-socket-free operation
(works rootless and on OpenRC), non-root UID mapping, all capabilities
dropped, no-new-privileges, read-only rootfs, bounded tmpfs, tiered egress
through a DNS-pinned allowlist proxy — including an enforced `proxy-enforced`
mode that combines `--network none` with a per-execution Unix relay — and
cancellation-safe container cleanup with an in-flight registry and a
socketless reaper sweep.

Around that core, the armor closes from several directions at once. The
sandbox bind mount is now credential-guarded (work item #205): an
agent-supplied working directory can no longer point the single mount at the
tenant `env`/`state`/`run` directories, protected directories inside a
legitimate mount are shadowed by empty read-only binds, and external workers
authenticate their inbound dispatch handshake with a new scoped
`WORKER_AUTH_TOKEN` instead of the full gateway bearer. Workload
processes on systemd multi-tenant hosts now spawn as manager-forked transient
services, structurally fixing rootless Podman user-namespace joins from
tenant scopes (work item #181/#178). Multi-tenant provisioning now defaults
every new tenant to a lean per-tenant Podman sandbox image — mt-admin builds
`lunarwing-sandbox:latest` and renders `SANDBOX_RUNTIME=podman`,
`SANDBOX_IMAGE=lunarwing-sandbox:latest`, and `SANDBOX_AUTO_PULL=false` at
add-tenant time (work item #223; upgrade-tenant deliberately leaves older
tenants untouched). A late-cycle mt-admin hardening pass fixed four
operator-facing defects found on live tenants: an invalid Quadlet key that
silently prevented the kimi worker unit from ever existing (WI#206
follow-up), LunarVoice stranded after upgrade-tenant by an admin-store image
gate (WI#229), a dead standalone health-pipeline installer (WI#228), and a
tenant-environment ownership loss during worker token backfill (WI#205
follow-up); upgrade-tenant now also refreshes the deployed host-global
health pipeline (WI#232). CodeAct scripting serves
`date.today()`/`datetime.now()` from the host clock instead of denying
every OS call (work item #227). External workers gained a new opencode2
lane (v2 line, pinned at v2.0.15 on a reviewed upstream commit) with a
reproducible-build bridge over the `@opencode/client/promise` SDK, full
mt-admin mirroring, CI/CD publication jobs, and its own ports-registry v17
block; the kimi worker moved to the 2.x line (`2.0.2`) with a headless-safe
permission contract, a fail-closed PreToolUse command guard (work item #206),
and a deterministic CLI contract gate. The Pebble
worker confines daemon-supplied project directories to the
workspace root and keeps its WebSocket read loop live so mid-task cancels
finally land. Session secrets are scrubbed from sandboxed and direct
workloads alike (work item #141).

The host health pipeline was rebuilt for multi-tenant scale: a report
model with staleness alerts, per-instance monitor-only diagnostic components
(runtime gateway probes and knowledge sync/sweep state), partial-result
tolerance under sweep deadlines, opencode2/voice unit resolution, and
self-heal escalation that never restarts on observability gaps. Conversation
insights — the release's largest reliability investigation — received a
nine-finding hardening program (goal 8) against confabulation and feedback
incidents. Missions gained a retry policy with exponential backoff,
restart-safe re-arming, and at-least-once event delivery (work item #188),
a daily thread budget default raised from 10 to 30 (work item #189), and
honest mission-tool parameters (work item #203); the release-cycle audit's
mission-retry and sanitizer follow-ups were closed in-cycle (goals 38/39).
Durable cross-channel
attachments, a pre-execution stale
tool-output sanitizer, keyset-paginated full-store knowledge sweeps that no
longer depend on the heartbeat, a compact-schema fix for
discriminated-union WASM tools, and a bundled-skills catalog trimmed to
product capabilities only (work item #185) round out the correctness work.

The release identifier is `2.1.2.0`; LunarWing-owned Rust crates and channel
packages use `2.1.2`. Rust 2024 and MSRV `1.96.1` are unchanged. There are no
new SQL migrations (`V27__memory_index_outbox.sql` remains latest). The
multi-tenant port registry migrates to v17 (opencode2_wss + opencode2_health
dedicated in range 51000-51999). Zanshin stayed aware after the strike;
Yoroi is what that awareness built.

See the [v2.1.0.0 release notes](docs/releases/RELEASE-v2.1.0.0.md) for the
previous compatibility baseline.

## Highlights

- **Generic Podman-first agent-shell sandbox.** The new
  `ic/src/sandbox/cli_runner.rs` drives `podman run`/`docker run` directly —
  no daemon socket — with hardened arguments (non-root UID 1000/docker-aware
  mapping, cap-drop ALL, no-new-privileges, read-only rootfs, tmpfs, `--rm`,
  `lunarwing.sandbox` labels, explicit `--pids-limit 256`). The shell tool is
  wired fail-closed via `register_sandboxed_shell` with the structured
  `[sandbox:<kind>]` error envelope. Tiered egress: `SANDBOX_NETWORK=proxy`
  (allowlist proxy via `host.containers.internal`), `none` (`--network
  none`), or `proxy-enforced` (`--network none` plus a per-execution Unix
  relay to the public-only DNS-pinned proxy; relay setup fails closed). The
  in-code default is now `none`, matching what mt-admin renders into tenant
  environments. New knobs: `SANDBOX_RUNTIME` / `SANDBOX_NETWORK` /
  `SANDBOX_ENFORCE`.
- **Sandbox credential containment (WI#205).** New
  `ic/src/sandbox/mount_guard.rs` validates the sandbox bind mount at both
  the manager and runner boundaries: canonicalization first (symlinks and
  `..` resolve before any check), tenant `env`/`state`/`run` directories are
  never mountable (nor is a mount inside them), credential-bearing filenames
  (`*.env`, `pg.secret`) are refused anywhere in the tree, and a protected
  directory inside a legitimate mount is shadowed by an empty read-only
  bind. Refusals are a hard `[sandbox:mount_path_denied]` error — never a
  fallback to unsandboxed execution. External workers authenticate their
  inbound dispatch handshake with a new per-tenant `WORKER_AUTH_TOKEN`
  (minted and migrated by mt-admin), so a leaked worker secret no longer
  grants the full gateway API surface. Evidence:
  [`docs/ops/v2.1.2.0_WI205_CREDENTIAL_EXPOSURE_FIX.md`](docs/ops/v2.1.2.0_WI205_CREDENTIAL_EXPOSURE_FIX.md);
  the sandbox-scoped credential design remains proposed follow-up.
- **Workload processes are manager-forked transient services (WI#181).**
  `ic/src/tools/process.rs` replaces `systemd-run --user --scope` with
  `systemd-run --user --wait --pipe --quiet --collect` transient services
  under `lunarwing-workloads.slice`, so rootless Podman demonstrably works
  even with the daemon's `NoNewPrivileges=true` context. Scrubbed
  environment travels via a mode-0600 `EnvironmentFile=`; timeout/cancel
  stops the unit through the user manager before the process-group kill.
  All four workload call sites (shell direct, sandbox direct, sandbox CLI
  runner, registry WASM builds) are wired.
- **New external worker: opencode2.** The OpenCode v2 lane runs alongside
  v1 as a separate worker: reproducible-build-hardened worker image, a
  TypeScript bridge over `@opencode/client/promise`, ports-registry v17
  slots (51000-51999), mt-admin mirroring with a 34-assertion harness, and
  protected CI publication jobs (candidate/scan/promote) with the
  digest-bound reproducible contract. Pinned at `v2.0.15` on reviewed
  upstream commit `6f3639d82ed0760091792189b78f8eeb44f699b1`; the v1 lane
  stays pinned at proven `v1.18.25` and remains the default.
- **Kimi worker fail-closed headless policy (WI#206).** The pinned CLI's
  print mode auto-approves commands and skips the upstream dangerous-command
  guard, so the worker now enforces its own baked `PreToolUse` Bash guard —
  privilege escalation, power/disk destruction, fork bombs,
  curl-pipe-shell, recursive chmod/chown on system roots, block-device
  writes, SSH-agent use, and `rm -rf` outside temp roots are denied with an
  explicit reason; unparseable input denies. The guard config is
  read-only in-container, re-injected over operator config overrides, and
  its env-var bypass is scrubbed from every task environment. The tenant
  SSH-agent socket is no longer mounted into the kimi lane and containers
  run with `no-new-privileges`.
- **Pebble worker confinement and live cancellation.** The Pebble worker
  now resolves and confines `project_dir` to the workspace root before
  dispatch (kimi's `resolveWorkspacePath` ported to Rust, with a
  symlink-aware overlap check and defense-in-depth re-resolution in
  `spawn_pebble`), and a global `WorkspaceRegistry` enforces one task per
  workspace subtree. The WebSocket read loop stays live for the whole
  connection — `dispatch_task` spawns the executor in the background and
  cancels route through a per-connection oneshot map keyed by `task_id`, so
  mid-task cancels actually kill the pebble subprocess (work item #101).
- **Conversation insights hardening (goal 8).** A nine-finding program
  against confabulation and generated-history feedback: authority checks on
  insight writes, automatic-recall isolation, generated-history feedback
  removal, durable-save error surfacing, host-captured eligible user
  evidence with content-derived batch identity, atomic host-validated
  insight writes (citation re-validation, host-minted identity, upserts,
  five-per-run budget), project-scoped event dispatch, durable cursor with
  restart-safe batching, and deletion-as-persistent-opt-out with optional
  scoped purge. Evidence:
  [`docs/ops/v2.1.2.0_GOAL8_CONVERSATION_INSIGHTS_FINDINGS.md`](docs/ops/v2.1.2.0_GOAL8_CONVERSATION_INSIGHTS_FINDINGS.md)
  and
  [`docs/ops/v2.1.2.0_GOAL8_INSIGHTS_HARDENING.md`](docs/ops/v2.1.2.0_GOAL8_INSIGHTS_HARDENING.md).
- **Corrupted blocking tool responses are demoted to text (WI#139 follow-
  up).** The blocking `complete_with_tools()` path in
  `StreamIntegrityProvider` now clears tool calls when stream-integrity
  corruption is detected, matching the streaming path — corrupted output can
  never reach a tool executor. A regression test pins the demotion.
- **Durable cross-channel attachments.** Every inbound attachment is
  persisted into workspace memory under `attachments/<channel>/<id>/` via
  the normal injection-scanned, search-indexed workspace write path, so a
  file sent on one channel stays retrievable from all other channels. The
  new built-in `attachment_store` tool (list/read) is path-confined,
  read-only under skill attenuation, and seeded AlwaysAllow.
- **Pre-execution stale tool-output sanitizer (WI#109).** Before the first
  model call of every engine thread, Tier 1 keys (`time`/`date`/`datetime`)
  are always purged and Tier 2 keys (`web_search`/`http`/`shell`) are purged
  when older than 30 minutes via the orchestrator-stamped `_cached_at`
  marker — purging what the mission trigger could not, because it runs
  before the first inference.
- **Mission retry with exponential backoff (WI#188).** Mission threads
  interrupted mid-run no longer lose their fire: `ThreadOutcome::Stopped`
  records the interruption in `approach_history`, notifies channels as an
  error, and schedules a retry from the new `MissionRetryPolicy` (default:
  3 retries, 60s initial delay, ×2 backoff, capped at 1h; `max_retries=0`
  disables). Retry state is durable on the failing thread's metadata and
  re-armed after daemon restart; event/webhook fires release their trigger
  dedup key on terminal non-completion so external redelivery can re-fire
  (at-least-once; an approval-paused fire keeps its key, so redelivery
  cannot double-fire — goal 38 audit fix). `MaxIterations` is deliberately
  never auto-retried.
- **Honest mission-tool parameters (WI#203).** `mission_create` rejects
  missing/non-string `name` and `goal` instead of silently minting "unnamed
  mission" or empty-goal missions; the manual-cadence default is logged; and
  `current_focus` is now tool-settable (`mission_update` schema exposes it,
  leave/set/clear semantics).
- **Daily mission thread budget default raised to 30 (WI#189).** An hourly
  mission without an explicit `max_threads_per_day` now gets full-day
  coverage plus retry headroom instead of going dark after ten hours.
  Budget exhaustion is already surfaced (`budget_exhausted`, warn log,
  explicit fire reason).
- **Bundled skills are product skills only (WI#185).** The compile-time
  embedded catalog ships exactly seven skills to fresh tenants; three
  contributor-dev workflow skills (web-ui-test, local-test,
  review-checklist) were removed. A regression test pins the catalog.
- **Host health pipeline rebuilt (WI#180).** The report model now includes
  staleness alerts (`LunarWingHealthReportStale`/`Missing`), monitor-only
  diagnostic components (`health-runtime.sh` probes live gateway and
  pending-gate contracts per tenant; `health-knowledge.sh` monitors
  knowledge sync/sweep state enablement-aware), partial results under sweep
  deadlines, opencode2/voice unit resolution on both inits, and self-heal
  escalation that treats runtime/knowledge as monitor-only diagnostics —
  never a restart on an observability gap.
- **Health metrics listener crate.** `ic/crates/lunarwing_health_metrics`
  provides the optional loopback listener for the healthpoint metrics
  exporter, with installer and systemd units; the host-global
  `health_metrics` slot remains the v16 registry slot.

## Compatibility

| Area | v2.1.2.0 behavior |
|---|---|
| Release version | `2.1.2.0` |
| Crate/package version | `2.1.2` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | None; `V27__memory_index_outbox.sql` remains latest |
| Multi-tenant port registry | v17 (opencode2_wss + opencode2_health dedicated in range 51000-51999); v15 LunarVoice and v16 health_metrics remain current |
| Channel registry patches | darkirc 0.1.8, multica 0.1.11, weechat 0.2.9, xmpp 0.1.14 |
| Tool registry patches | gitlab 0.1.5 (was 0.1.4); cavepony 0.4.2, llm-context 0.1.1, multica-bridge 0.1.2, ssh 0.1.1, vision-analyze 0.1.3, voice-transcribe 0.1.1, web-search 0.2.2, gotify 0.1.0 (no change) |
| WASM runtime | Wasmtime / wasmtime-wasi 36.0.14 (no change) |
| Dependency resolution | `ic/vendor/` source-replaces crates.io for the `ic/` workspace and satellite manifests rooted under `ic/`; no external version changed, so the vendor snapshot regenerated with no diff |
| Worker image tool pins | opencode `v1.18.25` at `cb7d8b2f5e44876ef98b661dc10590c915af3a9f`; kimi-code `2.0.2` (adopted 2026-09-21 after the 2.x line proved out); opencode2 `v2.0.15` at `6f3639d82ed0760091792189b78f8eeb44f699b1`; pebble at upstream HEAD `2d0f3786cbebb705cbd4f6212b0aaf47d8d8590b` |
| Sandbox default | `SANDBOX_NETWORK=none` is the in-code and mt-admin-rendered default; podman is the preferred runtime (`SANDBOX_RUNTIME`) |
| Kimi worker lane | No longer mounts the tenant SSH-agent socket and runs with `no-new-privileges` (WI#206); the fail-closed `PreToolUse` command guard is baked into the image |
| Worker image publication | Digest-bound reproducible candidates/promotions for kimi, pebble, opencode, and opencode2 |
| ShellCheck floor | 0.11.0 (checksum-pinned installer; CI gate remains blocking) |
| Bundled skills | Exactly seven product skills ship to fresh tenants: cavepony, delegation, lunarwing-workflow-orchestrator, multica-poll, plan-mode, routine-advisor, study-notes (WI#185; three contributor-dev skills removed from the embedded catalog) |
| Gateway mascot default | Disabled; `MASCOT_ENABLED=true` per tenant opts in |
| CI scope | Protected worker-image candidate publication/promotion is supported for kimi, pebble, opencode, and opencode2; daemon/package publication and tenant deployment remain deferred |

## Changes

### Generic Podman-First Agent-Shell Sandbox (work items #179, #154, #155)

The sandbox abstraction previously had a Docker-only bollard implementation.
The concept-branch design (`generic-podman-sandbox/concept-branch/v2.1.2.0`)
is now the supported runtime:

- `ic/src/sandbox/cli_runner.rs` drives `podman run`/`docker run` directly —
  no daemon socket, so rootless deployments and OpenRC hosts work without an
  API socket. Hardened arguments: non-root UID mapping (`user_for()` —
  rootless docker runs `0:0` inside the userns, rootful docker runs the
  daemon's euid/egid, podman keeps the keep-id ladder), cap-drop ALL,
  no-new-privileges, read-only rootfs, tmpfs, `--rm`, explicit
  `--pids-limit 256`, `--stop-timeout 10`, and `lunarwing.sandbox` labels
  with a shared in-flight registry.
- The sandbox manager swapped bollard for the CLI runner; the egress proxy
  starts for every non-`none` network mode — advisory `proxy` and
  `proxy-enforced` alike — behind a never-relaxed ACL. The shell tool
  is wired fail-closed via `register_sandboxed_shell` (production sandboxed;
  the dev `ALLOW_LOCAL_TOOLS` path stays direct unless
  `SANDBOX_ENFORCE=true`) with the structured `[sandbox:<kind>]` error
  envelope. Runtime detection (`check_podman`), doctor, and the setup wizard
  are runtime-agnostic.
- Tiered egress: `SANDBOX_NETWORK=proxy` (default for pre-existing tenants;
  allowlist proxy via `host.containers.internal`, deny bodies prefixed
  `sandbox-proxy denied:`), `none` (`--network none`), and WI#155's
  `proxy-enforced` — `--network none` plus a per-execution Unix relay to the
  public-only DNS-pinned proxy; relay setup fails closed, DNS is pinned per
  request, non-public destinations are rejected, and redirects are never
  followed on the host. The in-code default is now `none`, and mt-admin
  renders `SANDBOX_NETWORK=none` into every new tenant env
  (preserve-on-rerun, add-on-upgrade).
- Cancellation-safe cleanup: `InflightGuard` (RAII unregister) and
  `ContainerCleanupGuard` (detached kill/rm on drop) mean the shell tool's
  outer timeout can no longer leak a container the reaper would skip
  forever. `SandboxReaper::new` no longer fails without a socket; the daemon
  spawns a standalone CLI-sweep loop for socketless podman deployments, and
  the sweep scans the runtime the runner actually selected.
- Verified: sandbox/CLI-runner/reaper/shell/config unit tests; real-podman
  e2e tests on podman 5.8.2 and 6.1.1 (echo, rw/ro mounts, `--network none`
  egress block, proxy env wiring, timeout kill, outer-timeout cleanup,
  in-flight drain, proxy allowlist enforcement); live acceptance on fresh
  tenant `wi162verify` (OpenRC). Rootless builds keep `docker/sandbox.Dockerfile`
  lean. Evidence: `docs/ops/v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md` and
  `docs/ops/v2.1.2.0_GOAL33_WI162_SHELL_TRUNCATION.md`.

### Sandbox Credential Containment (work items #205, #209, #210, #211, #215)

The sandbox shell could read the tenant env file and obtain the plaintext
`GATEWAY_AUTH_TOKEN` — not via any env-file mount, but because the single
bind mount is agent-chosen: an unvalidated `workdir` pointing at the tenant
config directory exposed `lunarwing.env` (0600, readable through podman
`keep-id`'s tenant-UID mapping). Two exposures were fixed:

- **Mount guard.** New `ic/src/sandbox/mount_guard.rs`, enforced at both the
  manager and runner boundaries (defense in depth). Canonicalization happens
  before any check, so symlinks, `..`, and lexical tricks resolve to the real
  mounted path. The tenant `env`, `state`, and `run` directories are never
  mountable (nor is a mount inside them) — with WI#215's defensive sibling
  derivation covering stale MT renders where only `state`-under-base is
  known. Credential-bearing filenames (`*.env`, `pg.secret`, `.env`,
  case-insensitive) are refused anywhere in the tree. A protected directory
  *inside* an otherwise legitimate mount is shadowed by an empty read-only
  bind (`--tmpfs` was tested and does not shadow; only the empty-directory
  bind does). Refusals are a hard `[sandbox:mount_path_denied]` error —
  never a fallback to unsandboxed execution.
- **Scoped worker credential.** External workers previously received the
  tenant's full gateway bearer as their WebSocket handshake secret, readable
  via `podman inspect` and tenant-readable unit files — while the worker only
  ever needed to authenticate one inbound task-dispatch handshake. A new
  per-tenant `WORKER_AUTH_TOKEN` is minted by mt-admin and resolved through
  one helper across all consumers (daemon `config.toml`, all four worker
  container envs, the systemd Quadlet). A leaked worker secret now only
  permits impersonating the daemon toward that one worker's local socket;
  the gateway never accepts it. Legacy tenants converge via
  `upgrade_external_worker_auth_token()` on the next lifecycle pass
  (WI#210: it mints and persists the scoped token instead of silently
  substituting the gateway bearer on the upgrade path).
- WI#209 review fixes: masking planning is existence-independent (a
  protected dir absent at plan time — e.g. `run/` on a fresh tenant — is
  still planned for masking), and an unresolvable mount root is a hard
  refusal rather than an empty mask list. WI#211: protected-filename
  matching applies throughout the mount tree, not only the mount root.
  Evidence: `docs/ops/v2.1.2.0_WI205_CREDENTIAL_EXPOSURE_FIX.md`; the
  sandbox-scoped gateway credential (route-scoped badge) remains a proposed
  follow-up design
  (`docs/ops/v2.1.2.0_WI205_SANDBOX_SCOPED_CREDENTIAL_DESIGN.md`).

### Workload Transient Services Under the User Manager (work items #181, #178)

`systemd-run --user --scope` forked the daemon and inherited
`NoNewPrivileges=true` + `PrivateTmp=true`, a mount-namespace context that
rootless Podman's user-namespace join cannot cross. Workloads now spawn as
manager-forked transient **services**:

- `ic/src/tools/process.rs` uses `systemd-run --user --wait --pipe --quiet
  --collect` with a generated `--unit=` under `lunarwing-workloads.slice`.
  The user manager forks workloads in the same context class as tenant
  Quadlet units, so rootless Podman demonstrably works even with
  `NoNewPrivileges=true`.
- Scrubbed environment and working directory are handed over via a
  mode-0600 `EnvironmentFile=` (double-quoted values, file deleted on every
  guard exit path) and `--working-directory=`. `ProcessTreeGuard` stops the
  unit via `systemctl --user stop` (new `LUNARWING_WORKLOAD_TIMEOUT_STOP`
  knob) on timeout/cancel before the process-group kill.
- All four workload call sites are wired: shell tool direct path, sandbox
  `execute_direct`, sandbox CLI runner (podman CLI), and registry WASM
  builds. Full lib suite and the `podman_works_from_transient_service`
  regression (real `podman ps` through a transient service) pass. The
  `fix-podman-userns` mt-admin repair verb remains the operator-side
  recovery for pairings broken before this fix.

### Sandbox Environment Scrubbing (work item #141)

The double-opt-in `SANDBOX_POLICY=full_access` direct lane spawned `sh -c`
inheriting the daemon's entire environment, including `DATABASE_URL`. The
manager now applies the same allowlist scrub as the container lanes:
`env_clear()`, forward `SAFE_ENV_VARS`, re-apply the spawner's
`XDG_RUNTIME_DIR`, then caller-provided env on top — which also scrubs
`SANDBOX_POLICY`/`SANDBOX_ALLOW_FULL_ACCESS` from the child. Regression tests
pin both the leak and the passthrough contract.

### Shell Output Truncation Visibility (work item #162)

`ShellTool` discarded the sandbox manager's `truncated` flag (32,768 bytes
per stream), and its direct path silently discarded drained bytes beyond
65,536. Both paths now return `truncated` plus `warning`, and partial output
starts with `[output truncated]` so prefix-only previews retain the signal.
Combined-display clipping is flagged; exact-limit captures stay complete;
real exit codes and `success` semantics are preserved. Live acceptance on
fresh tenant `wi162verify` (OpenRC) passed the real-container regression
through podman 5.8.2.

### Pre-Execution Stale Tool-Output Sanitizer (work items #109, #126)

WI#109's stated purpose — purge stale cached tool output before threads
execute — is implemented as a synchronous pre-execution sanitizer in
`ExecutionLoop::run`: Tier 1 (`time`/`date`/`datetime`) always purged,
Tier 2 (`web_search`/`http`/`shell`) purged when older than 30 minutes via
the `_cached_at` RFC 3339 timestamp the Python orchestrator now stamps at
the persisted-state write site. Sanitized state is written back with a
`sanitized_at` observability marker, before the `Running` transition and
first model call of every thread — an ordering a mission trigger cannot
satisfy. WI#126 was re-verified (fleet remains terminal-event-driven);
WI#163, a re-file, was reverified with no code change.

### Sanitizer Role-Token Neutralization Hardening (goal 39)

The legacy sanitizer (`ic/crates/lunarwing_safety/src/sanitizer.rs`) had a
gap the source audit called out: its critical-only escape prepended a
backslash before `<|`, leaving the special-token opener intact for
chat-template tokenizers, and role-token neutralization was gated on
scanner severity. `Sanitizer::sanitize` now runs
`neutralize_role_token_openers` unconditionally — a zero-width space (U+200B)
is inserted after `<` in every `<|` opener independently of detection
severity — and `SafetyLayer::sanitize_tool_output` applies the same
neutralizer even when injection scanning is disabled, with `was_modified`
accurately reflecting either transformation. This is defense in depth on
top of the existing prompt sinks (e.g. the rig adapter and provider), which
already neutralized role-token openers. Note for byte-for-byte consumers:
token openers in returned tool text now carry U+200B; the raw tool response
is unchanged. Regression tests cover both injection-scan settings and
already-backslashed role delimiters
(`docs/ops/2.1.2_FINDINGS.md`).

### Mission Reliability: Retry, Budget, and Honest Parameters (work items #188, #189, #203; goals 22, 29, 31, 38)

- **Retry with exponential backoff (WI#188).** An interrupted (signal-stopped)
  mission fire no longer loses its work permanently: `ThreadOutcome::Stopped`
  records the interruption in `approach_history`, notifies notify-channels as
  an error, and schedules a retry per the new `MissionRetryPolicy` on
  `Mission::retry_policy` (default: `max_retries = 3`, `initial_delay = 60s`,
  `×2.0` backoff, capped at 1h; `max_retries = 0` disables). The policy is
  tool-tunable through `mission_create`/`mission_update` (adapter params
  `max_retries`, `initial_delay_secs`, `backoff_multiplier`, `max_delay_secs`).
  Retry state is durable on the failing thread's metadata (`RetryState`), so
  a daemon restart re-arms pending retries after checkpoint recovery.
  Event/webhook fires release their trigger dedup key on terminal
  non-completion (`Failed`/`Stopped`/`MaxIterations`) so an external
  redelivery can re-fire (at-least-once); a `GatePaused` fire retains its
  key while awaiting approval, and a successful retry
  re-records the key, restoring dedup. `MaxIterations` is deliberately never
  auto-retried — it is notified with a model-facing suggestion instead.
  Post-landing audit (goal 9, `docs/ops/v2.1.2.0_ITEM9_COMMIT_AUDIT.md`)
  flagged follow-ups F1/F2/F3/F10; all four were remediated in-cycle at goal
  38 (`212g38`, see below) before release.
- **Audit follow-ups remediated (goal 38, `212g38`).** The goal-9 commit
  audit's four mission-retry findings were closed in-cycle:
  **F1** — the retry re-fire path no longer injects the internal
  `retry_attempt` key into the trigger payload; the attempt counter stays in
  the spawned thread's top-level metadata (`persist_retry_attempt_marker`)
  and `refire_mission_thread` forwards the original payload unchanged, so
  neither the model's trigger context nor `Mission::last_trigger_payload`
  sees it. **F2** — dedup keys are released only on terminal
  non-completions (`Failed`/`Stopped`/`MaxIterations`); `GatePaused` retains
  the key while the original thread awaits approval, so a redelivered
  trigger during an approval window can no longer double-fire. **F3** —
  startup retry recovery enumerates persisted missions across projects via
  `Store::list_all_missions_global` instead of the in-memory active index,
  so retries scheduled in a previous daemon life are re-armed; project
  listing failures propagate rather than silently skipping. **F10** — the
  `mission_update` tool schema now advertises all four retry-policy scalars
  (`max_retries`, `initial_delay_secs`, `backoff_multiplier`,
  `max_delay_secs`) the effect adapter already parsed. Targeted engine
  tests cover unchanged retry payloads, paused-gate dedup, and recovery
  without an active-index entry; live tenant restart acceptance remains an
  operator step (`docs/ops/2.1.2_FINDINGS.md`).
- **Daily thread budget default 10 → 30 (WI#189).** An hourly mission
  without an explicit `max_threads_per_day` now covers a full 24-hour day
  plus retry headroom instead of going dark after ten hours. Retry respawns
  draw from the same budget (documented interaction); only spawn-failure
  rollback refunds a slot, and deduped redeliveries cost nothing.
- **Honest mission-tool parameters (WI#203).** `mission_create` errors on
  missing or non-string `name`/`goal` (matching the schema's `required`)
  instead of silently minting "unnamed mission" or empty-goal missions; the
  `manual` cadence default is now logged naming the user and mission;
  `current_focus` is tool-settable via `mission_update` with leave/set/clear
  semantics (absent → leave, JSON `null` → clear, string → set).

### Conversation Insights Investigation and Hardening (goal 8, WI#132, WI#143, WI#183)

The release's largest reliability program against confabulation incidents:

- Investigation report `docs/ops/v2.1.2.0_GOAL8_CONVERSATION_INSIGHTS_FINDINGS.md`
  documents nine open findings, the corruption/provenance contracts, and a
  controlled incident-reproduction plan.
- All nine findings are now implemented: authority checks (G8-F1), automatic
  recall isolation (G8-F3), generated-history feedback prevention (G8-F5),
  durable-save errors (G8-F9), host-captured eligible user evidence with
  content-derived batch identity (G8-F2), atomic host-validated insight
  writes with citation re-validation, host-minted identity, upserts, and a
  five-per-run budget (G8-F4), project-scoped event dispatch (G8-F6),
  durable cursor with restart-safe batching (G8-F7), and deletion-as-
  persistent-opt-out with optional scoped purge and explicit resume (G8-F8).
- Verified: engine and production-store test suites, clippy `-D warnings`.
  Records: `docs/ops/v2.1.2.0_GOAL8_CONVERSATION_INSIGHTS_FINDINGS.md` and
  `docs/ops/v2.1.2.0_GOAL8_INSIGHTS_HARDENING.md`; tracker WI#183.

### Durable Cross-Channel Attachments (goal 3)

Every inbound attachment persists into workspace memory under
`attachments/<channel>/<id>/<filename>.md` — sanitized metadata, extracted
text, bounded base64 inline data — via the normal workspace write path, so it
is injection-scanned, search-indexed, and visible in the WebUI memory tree.
The built-in `attachment_store` tool (list/read) is registered alongside
memory tools in single-user and multi-tenant paths, protected from dynamic
shadowing, read-only under skill attenuation, and seeded AlwaysAllow. Paths
are confined to `attachments/`.

### Knowledge Sweep Rework (goal 18, work item #121)

The stale-note sweep previously saw only the 1,000 most-recently-updated
notes and ran only inside the heartbeat loop, which mt-admin disables for
every tenant. `KNOWLEDGE_SCAN_CAP` is gone: `knowledge_list_notes` takes an
`(updated_at, path, user_id)` keyset cursor and `knowledge_scan_all` pages
the entire store (500-row pages, 100k safety ceiling, no-progress guard).
A heartbeat-independent `run_knowledge_sweep_loop` cadence (gated by
`KNOWLEDGE_SWEEP_ENABLED`, deterministic startup jitter, hourly-to-6h poll)
runs alongside the existing heartbeat path, so multi-tenant fleets sweep
too.

### Skill Registry Sidecar Path Fix (goal 19)

The v1→v2 skill migration writer placed `.registry.json` inside the skill
dir while the reader looked one level too high at the skills root, so
registry provenance was silently never read and registry-installed skills
re-imported on every restart. `read_registry_sidecar` now joins on the
source dir directly with an `is_file()` → `parent()` fallback for legacy
file-shaped sources; registry provenance stamps into `V2SkillMetadata`
end-to-end.

### Bundled Skills Trimmed to Product Capabilities (work item #185, goal 30)

`ic/build.rs` compiles all `ic/skills/*/SKILL.md` into the embedded catalog,
so every fresh tenant previously received all ten bundled skills — three of
which were LunarWing contributor workflows, not tenant capabilities
(`web-ui-test`, `local-test`, `review-checklist`; the latter referenced
proprietary reviewers as product defaults). The catalog now ships exactly
the seven product skills (cavepony, delegation,
lunarwing-workflow-orchestrator, multica-poll, plan-mode, routine-advisor,
study-notes), pinned by the `bundled_catalog_is_exactly_the_product_skill_set`
regression test. Operator opt-in is unaffected: user/workspace skills always
override bundled ones. Record:
`docs/ops/v2.1.2.0_ITEM30_WI185_BUNDLED_SKILLS.md`.

### Host Health Pipeline Rebuild (work item #180)

All eight findings addressed, then all six review findings:

- opencode2/voice unit resolution on systemd and OpenRC (incl. `-sup`
  supervisor suffix stripping); `llm_streams` serialized into the saved
  report; scheduled-safe PATH on OpenRC; slow fleet probes keep partial
  results with coverage-loss degradation; alert freshness/skip semantics
  (`report_age_seconds`, `LunarWingHealthReportMissing`,
  `LunarWingHealthReportStale` evaluated at scrape time, distinct skip code
  4, `LunarWingUnitDown` on status==2 only).
- New `health-runtime.sh` probes each tenant's live gateway
  `/api/gateway/status` and `/api/chat/pending-gates` plus bridge contracts
  (ports-registry discovery, bearer tokens from tenant env files), with
  every request capped by the remaining sweep budget. New
  `health-knowledge.sh` monitors knowledge sync/sweep state — enablement-
  aware, disabled features never degrade.
- Self-heal treats runtime/knowledge as monitor-only diagnostic components
  with synthetic `runtime@<tenant>`/`knowledge@<tenant>` identities —
  escalation, never a restart, on observability gaps. The gateway check
  reports an absent legacy sessions dir as healthy-with-note, never
  degraded, so obsolete telemetry cannot schedule daemon restarts.
- Verified: `tests/run-all.sh` 926 passed / 0 failed across all 19 suites;
  repo-wide ShellCheck exit 0.

### External Workers: opencode2 Lane (goals 21, 43)

The OpenCode v2 lane is a full first-class worker:

- Reproducible-build-hardened worker image (upstream build script patched
  for deterministic output; byte-identical digest contract between candidate
  and clean rebuild), a TypeScript bridge over `@opencode/client/promise`,
  ports-registry v17 slots (51000-51999), mt-admin mirroring with a
  34-assertion harness, and workspace confinement ported from the kimi
  bridge. The daemon side needed zero Rust change — the external-worker
  surface was already generic.
- CI/CD: `worker-opencode2-candidate/scan/promote` jobs extend the shared
  publication templates (digest-pinned images, reproducible candidate +
  clean-rebuild digest contract, Trivy CRITICAL gate + SBOM,
  `$CI_COMMIT_TAG-opencode2` immutable promotion); `build-worker-image.sh`
  gained the opencode2 case; `validate_config.py` and
  `promote-worker-image.py` guard the lane.
- Pin history this cycle: v2.0.4 → v2.0.5 (goal 21/MR !988) → v2.0.8
  → v2.0.11 → v2.0.14 → v2.0.15. The v2.0.15 tag resolves to reviewed
  commit `6f3639d82ed0760091792189b78f8eeb44f699b1`; the upstream CLI
  build script is unchanged from v2.0.14. The promise client adds base-URL
  path-prefix preservation and declared-error wrapping, and session metadata
  support is additive to the bridge's existing API calls. Image digest
  reproducibility remains a CI gate for publication.

### Pebble Worker Hardening (goals 17, 20, work items #101, #149)

- **Workspace confinement.** `pebble4lunarwing/src/workspace_path.rs` ports
  kimi's `resolveWorkspacePath` + workspace-overlap check: the bridge
  resolves and confines the requested project dir before dispatch (escapes
  get a `task_result` error envelope and nothing spawns), and `spawn_pebble`
  re-resolves fail-closed as defense in depth. A global `WorkspaceRegistry`
  enforces one task per workspace subtree across all connections.
- **Live read loop, real cancels (WI#101/WI#149).** `dispatch_task` no
  longer parks the WebSocket read loop: it validates/registers, spawns the
  executor as a background future, and returns immediately. Outbound
  envelopes flow through an mpsc channel drained by a single writer task;
  cancel envelopes are keyed by `task_id` in a per-connection map and kill
  the pebble subprocess (`Outcome::Cancelled` → `task_result` `cancelled`).

### Kimi Worker and OpenCode Pins (goals 12, 36)

- kimi-code `0.42.0` → `0.43.0` → `0.43.1` → **`2.0.2`** (adopted
  2026-09-21; npm `latest`; package-lock integrity matches the npm registry
  per bump). Dockerfile ARG, build-args, package-lock, mt-admin harness
  assertion, and docs synchronized per bump. The 2.0.2 adoption also fixed
  the baked TensorZero base_url to the NetBird hostname and hardened the CLI
  contract test for 2.x behavior (provider-connection retries with backoff
  instead of exit 0; `--yolo` still rejected alongside `-p`; seeded minimal
  config so model resolution succeeds in clean image builds).
- **Headless permission contract (WI#195).** The worker's default
  permission mode moved yolo → auto and both entrypoint modes export
  `KIMI_CODE_PERMISSION_MODE_REMINDER=0` (operator-overridable) so headless
  `-p` runs no longer stall on interactive prompts. A real-CLI contract test
  lane (`scripts/kimi_cli_contract_test.ts`) executes the installed kimi
  binary — asserting `-p --output-format stream-json` is accepted and
  `--yolo` is rejected — and the Dockerfile runs that lane against the
  pinned version post-`npm test`. Unknown stream-json roles are warn-logged
  instead of silently dropped.
- **Fail-closed headless command guard (WI#206, goal 36).** Empirical
  verification against the pinned CLI (2.0.2) showed that print mode forces
  the auto permission policy, that the upstream dangerous-command guard is
  skipped entirely in auto mode, and that `[[permission.rules]]` never load
  from config — so auto-approve meant no approval boundary at all. The
  worker now ships a baked `PreToolUse` Bash guard
  (`hooks/headless_command_guard.mjs`, wired via `[[hooks]]` in
  `config.toml`) that denies privilege escalation, power/disk destruction,
  fork bombs, remote-content-piped-to-shell, recursive chmod/chown on
  system roots, block-device writes, SSH-agent socket use, and recursive or
  forced `rm` outside temp roots; unparseable payloads deny. `config.toml`
  keeps `dangerous_command_guard = true` so a future upstream change fails
  closed, the entrypoint re-injects guard+hook into any operator-supplied
  `KIMI_CONFIG_SOURCE` that lacks them (and unsets
  `KIMI_CODE_DANGEROUS_COMMAND_GUARD`), the task executor scrubs
  guard-disabling env vars from per-task environments, and `/app/config` +
  `/app/hooks` are read-only to the runtime user so a task cannot edit its
  own guard. mt-admin no longer mounts the tenant SSH-agent socket into the
  kimi lane (both launch paths), adds `--security-opt no-new-privileges`,
  and bumps `KIMI_ROOTLESS_LAUNCH_CONTRACT` so existing containers
  force-recreate; other worker lanes keep the socket. Verified live on
  2.0.2: `sudo`, `shutdown`, `curl … | sh`, and `rm -rf` of non-temp paths
  deny in ~3s with an explicit reason while ordinary builds and tests run
  unchanged.
- opencode2 `v2.0.5` → `v2.0.8` → `v2.0.11` → `v2.0.14` → `v2.0.15` at reviewed
  commit `6f3639d82ed0760091792189b78f8eeb44f699b1` (tag SHA verified
  against anomalyco/opencode; upstream build script byte-identical to v2.0.14,
  so the reproducible-build patch applies unchanged).
- opencode v1 stays deliberately pinned at `v1.18.25`
  (`cb7d8b2f5e44876ef98b661dc10590c915af3a9f`) — v1.18.29's live upstream
  regressions remain disqualifying. Pebble remains at upstream HEAD
  `2d0f3786cbebb705cbd4f6212b0aaf47d8d8590b`.

### CI/CD Improvements (goals 5, 21, 28)

- Giant-crate compile concurrency is runner-managed: the project-wide
  `rust-compile-16gb` resource-group serialization trialled during the
  cycle (including the previously-bypassing `cavepony-wasm` job) was
  deliberately removed again before release — only the per-worker
  image-promotion resource groups serialize. The OOM class it targeted
  (reference pipeline #3742's `sccache: Compile terminated by signal 9`)
  has recurred under concurrent giant builds and remains a documented live
  caveat; OOM/ENOSPC operator runbooks were added for it.
- opencode2 gained full protected publication jobs (candidate/scan/promote)
  with the digest-bound reproducible contract and
  `$CI_COMMIT_TAG-opencode2` promotion.
- ShellCheck floor raised to 0.11.0 via a checksum-pinned installer
  (`.gitlab/scripts/install-shellcheck.sh`), clearing the SC2218 class that
  Bookworm's 0.9.0 misreported; the blocking gate stays zero-warning.
- The kimi worker image's in-build `npm test` gate failed on CI (barracuda,
  2026-09-22: 43/53) because the WI#206 regression fixtures
  (`hooks/headless_command_guard.mjs`, `config/config.toml`) were only
  COPYed into the final runtime stage — the builder-stage gate never saw
  them. The fixtures now ship into the builder stage (master `43eb871d3`),
  and the full gate re-verified green end-to-end from a clean state
  mirroring the Dockerfile builder context (53/53).
- The `rust-clippy-all` gate (zero-warning policy, `-D warnings`) failed on
  the WI#188 retry regression tests — two shadowed duplicate `store`
  bindings and an unneeded `mut` in `runtime/mission.rs` tests. The dead
  bindings were removed and the gate restored to green (`36e5a8197`,
  goal 5).

### Version Bumps and Supply Chain (goal 1)

- All LunarWing-owned release packages moved 2.1.1 → 2.1.2 (daemon, five
  workspace crates, XMPP bridge, four WASM channels); six lockfiles
  refreshed; channel registry packages bumped (darkirc 0.1.8, multica
  0.1.11, weechat 0.2.9, xmpp 0.1.14). A tracked-manifest/lock audit
  confirms no project-owned package remains at 2.1.1.
- External crates: all 26 tracked lockfile roots report zero pending
  semver-compatible updates; MSRV-1.96.1-blocked majors stay pinned with
  documented rationale. `ic/scripts/refresh-vendor.sh` regenerated
  `ic/vendor/` with no diff (no external version changed); `cargo deny
  check` clean on advisories, bans, licenses, and sources.
- The gitlab WASM tool's registry version moved 0.1.4 → 0.1.5 with the
  compact-schema fix; CI `check-version-bumps.sh` now validates registry
  bumps on tool source changes.

### WASM Tool Schema Fix (goal 27, work item #150)

`compact_schema` in `ic/src/tools/wasm/wrapper.rs` now keeps the union of
oneOf/anyOf/allOf variant-required properties in the advertised schema
(flattened, not top-level `required`), so discriminated-union tools like
gitlab_tool no longer collapse to the bare `action` discriminator. Models
see `project`/`title`/etc. without needing the tool_info hint. Registry
0.1.5 (gitlab); `check-version-bumps.sh` now requires the registry version
to move when tool sources change — hence the 0.1.5 bump rather than a no-op
realigned 0.1.4.

### GitLab WASM Always-Allow Grants Unified (goal 13)

"Always allow" for the GitLab WASM tool did not work in all cases: the
persisted permission key was written as `gitlab_tool` in some paths and
`gitlab-tool` in others, so a grant recorded under one spelling was not
honored when the tool registered under the other (across both legacy chat
and Engine V2). Both paths now share one canonical key while still reading
historical grants under either spelling — including session-only mode —
and an independently registered tool with the colliding name can no longer
inherit a grant meant for the GitLab tool. Diagnosis, security boundaries,
and live-tenant acceptance steps are in
`docs/ops/v2.1.2.0_GOAL13_GITLAB_ALWAYS_ALLOW.md`. Live-tenant
confirmation of the installed GitLab WASM tool remains for the operator.

### Approval Delivery and Gate Recovery (work item #164)

Authenticated pending-gate recovery with race-safe browser reconciliation;
resolved auto-approve configuration with legacy env alias/diagnostics;
persisted conversation lookup with scoped auth-thread fallback; bounded
late-turn watcher sharing normal response hooks, history, auth/gate
handling, and channel delivery; startup disk-gate pruning; proactive
gateway message thread routing. MR !1030 follow-up preserves exact
auth-gate identity through token/cancel controls and makes cancellation
durable. Evidence: `docs/ops/v2.1.2.0_GOAL31_WI164_APPROVAL_DELIVERY.md`.
Final live release acceptance for this thread is carried on the goals
checklist.

### REPL Argument Integrity (goal 34, work item #161)

Fixed name-only/final argument assembly and CodeAct nested-fence
truncation; added pre-execution REPL argument validation, a distinct
bounded repair failure, and `repl_argument_corruption_total` tenant-aware
stream telemetry. Engine, LLM/provider, bridge, and integration suites
passed; fresh live-tenant verification is carried on the goals checklist.
Evidence: `docs/ops/GOAL34_REPL_ARGUMENT_INTEGRITY.md`.

### mt-admin and Multi-Tenant Fixes

- The opencode2 port map is now included in the collision probe, doctor
  duplicate check, and `ports list`, so v17 allocations are probed and
  duplicated-checked alongside the other tenant port maps.
- WI#205 worker-credential migration: `resolve_worker_auth_token()` mints and
  persists a scoped `WORKER_AUTH_TOKEN` per tenant, and
  `upgrade_external_worker_auth_token()` converges legacy tenants
  (`config.toml` auth_token, worker container envs, Quadlet `Environment=`)
  on the next lifecycle pass. The kimi mt-admin harness derives the pinned
  CLI version instead of hard-coding it, so pin bumps can no longer leave
  the assertion stale.
- **Worker stop now goes through the owning systemd unit (goal 10).** On
  systemd + rootless hosts, `stop_tenant_kimi` stops the Quadlet
  `lunarwing-kimi-<t>.service` user unit first (with a container-stop
  fallback) instead of stopping the container directly — a bare container
  stop under a live unit is resurrected by the quadlet's `Restart=on-failure`,
  so stop-tenant/upgrade-tenant could previously leave the kimi worker
  running. This is the WI#128 pebble contract applied to kimi. A parity
  sweep found the identical gap in `stop_tenant_opencode`,
  `stop_tenant_opencode2`, and `stop_tenant_postgres` (its quadlet has
  `Restart=on-failure` too); all received the same unit-first branch.
  Vision/voice sidecars and the full-tenant path already stopped their
  units first. Regression coverage:
  `test_worker_quadlet_stop_parity` in
  `ic/scripts/tests/test-mt-admin-worker-lifecycle.sh` asserts unit-first
  stop + container fallback for kimi/opencode/opencode2.

### Documentation Parity (goals 41, 45, 46, 48, 49, 50, 52)

- All 31 current guides revalidated against the 2.1.2.0 tree;
  `MT-ADMIN-QUICKSTART` gained `fix-podman-userns`; `ENABLING_DEV_TOOLS`
  rewritten for the podman-first sandbox and WI#141 scrub; `TESTING_GUIDE`
  gained the attachment tier and worker cancel-ack note;
  `TENANT_KNOWLEDGE_SEED` notes the sweep rework.
- All architecture docs revalidated; `SEMANTIC-MEMORY-SEARCH` gained the
  V27 outbox and mission-subtree exclusion; `ENGINE-V2` gained attachment
  input, data-retention, and insights sections; `SELF_HEAL_*` docs gained
  2.1.2.0 lifecycle notes.
- `MULTITENANCY-PRODUCTION` port blocks now cover the v15 LunarVoice, v16
  health_metrics, and v17 opencode2 blocks; `TENANT-CONFIGURATION` registry
  schema corrected to v17.
- Goal 50 (guides repass) and goal 52 (architecture repass) revalidated all
  current docs against the WI#179/181/180/154/155 hardening and worker-pin
  changes; `TESTING_GUIDE` gained the three sandbox network modes and the
  opencode2 worker lane; `MT-ADMIN-QUICKSTART` gained
  `restart-worker <tenant> opencode2`; `SSH_AGENT_HARNESS`/
  `SSH_DELIVERY_MECHANISMS` cover all four worker types (since corrected for
  WI#206: the kimi lane no longer mounts the agent socket);
  `SELF_HEAL_*` docs
  carry the WI#180 parity subsections.
- Goal 49 (open work items audit repass, 2026-09-22) re-queried the live
  tracker — every open item's body and full comment thread — and rewrote
  `docs/ops/v2.1.2.0_OPEN_WORK_ITEMS_STATUS.md` with the 27-item
  disposition table; all fourteen items from the 09-14 audit are closed.
  WI#206's contract is also documented in `docs/ops/WORKER-CONTAINERS.md`
  (headless permission contract section) and the worker's own AGENTS.md
  invariants.

## Upgrade Notes

1. Back up PostgreSQL, tenant environment files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state.
2. No SQL migration is introduced; `V27` remains latest. The port registry
   migrates to **v17** (opencode2_wss + opencode2_health in 51000-51999);
   the migration is automatic on the next mt-admin registry touch and is
   additive only.
3. Rebuild the daemon and installed WASM extensions through the normal
   deployment path. All four channel registry versions changed (darkirc
   0.1.8, multica 0.1.11, weechat 0.2.9, xmpp 0.1.14). Rebuild/reinstall
   the gitlab tool for registry 0.1.5; other tool registry versions are
   unchanged from v2.1.0.0.
4. **Sandbox default change.** New tenants render `SANDBOX_NETWORK=none`;
   existing tenant environments are preserved on re-render and gain the
   setting only on upgrade. Review any tenant workloads that legitimately
   need egress and set `SANDBOX_NETWORK=proxy` (allowlist) or
   `proxy-enforced` per tenant. The in-code default is now `none`.
5. If tenants used rootless Podman from agent shells before this release,
   run the `fix-podman-userns` repair verb on pairings already broken; new
   spawns go through transient services and need no repair.
6. Review sandboxed-shell policy per tenant: production shells are
   fail-closed sandboxed; dev direct access requires `ALLOW_LOCAL_TOOLS`
   (and now respects `SANDBOX_ENFORCE=true`). Set `SANDBOX_RUNTIME=podman`
   (preferred) or `docker`, and choose `SANDBOX_NETWORK` per tenant.
7. Knowledge sweeps now run independently of the heartbeat via
   `KNOWLEDGE_SWEEP_ENABLED`; multi-tenant fleets that relied on the
   (disabled) heartbeat sweep will now actually sweep — review
   `KNOWLEDGE_SWEEP_*` cadence knobs before enabling at scale.
8. Worker tool pins: opencode `v1.18.25` (`cb7d8b2f…`), kimi-code `2.0.2`
   (the 2.x line is now the adopted pin — rebuild the kimi worker image; the
   0.4x line is superseded), opencode2 `v2.0.15` (`6f3639d82…`), pebble
   `2d0f3786`. Do not adopt the opencode 2.x line for the opencode lane or
   the opencode v1.18.29 image. Pull worker images by
   the digest recorded in `worker-image-evidence.json`.
9. **Worker credential migration (WI#205).** On the next mt-admin lifecycle
   pass, each tenant gains a scoped `WORKER_AUTH_TOKEN`; legacy
   `config.toml` entries, worker container envs, and Quadlet units converge
   automatically. No operator action is required, but expect a one-line
   warning per legacy tenant at migration time. Gateway bearers are no
   longer handed to worker lanes.
10. To deploy opencode2 for a tenant, use the mt-admin opencode2 verbs and
   CI-published `worker-opencode2` images; the lane defaults remain
   eval-tenant-scoped per the rollout plan until Phase E acceptance.
11. Pebble workers enforce workspace-path confinement and concurrent-task
    exclusivity; daemon-supplied `project_dir` values outside
    `WORKSPACE_ROOT` are now rejected by design.
12. **Kimi worker lane hardening (WI#206).** Rebuild the kimi worker image
    (2.0.2) and let the `KIMI_ROOTLESS_LAUNCH_CONTRACT` bump force-recreate
    existing containers: the tenant SSH-agent socket is no longer mounted,
    `no-new-privileges` is applied, and the baked fail-closed command guard
    ships. Tenants that legitimately need SSH from kimi worker tasks must
    move that work to the pebble/opencode/opencode2 lanes or a scoped
    credential broker — the shared tenant agent is no longer exposed to the
    headless executor.
13. ShellCheck 0.11.0 is the CI floor for contributors; the checksum-pinned
    installer is at `.gitlab/scripts/install-shellcheck.sh`.
14. Conversation-insights changes add durable cursors, batch identity, and
    deletion-as-opt-out state; on upgrade, existing insight stores are
    migrated in place by the production store tests' covered paths — no
    operator action required, but review `docs/ops/v2.1.2.0_GOAL8_INSIGHTS_HARDENING.md`
    for the new authority/eligibility contracts.
15. **Mission defaults changed.** The daily thread budget default is now 30
    (was 10), and interrupted mission fires retry automatically with
    exponential backoff (3 retries default; `max_retries=0` opts out per
    mission). Tenants with explicit `max_threads_per_day` are unaffected.
16. **mt-admin worker stops now target the systemd unit.** On systemd +
    rootless hosts, `stop-worker`/`stop` for the kimi, opencode, opencode2,
    and postgres containers stop the owning Quadlet user unit first —
    previously a bare container stop could be resurrected by the unit's
    `Restart=on-failure`, leaving a worker running after a stop/upgrade.
    No operator action is needed beyond the normal mt-admin update; the
    behavior converges on the next lifecycle pass.

### Security, Deployment, and Release Limitations (carried)

- DarkIRC Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
  delivery remain disabled behind separate acceptance gates (carried
  unchanged).
- The September 16 conversation-insights incident's causal trigger remains
  unconfirmed pending its trace; the nine-finding hardening program landed,
  but final live incident reproduction is explicitly carried.
- WI#155's `proxy-enforced` mode is implemented and gated; live docker-
  runtime e2e was not run on the dev VM (no docker). The acceptance file
  carries the exact commands for a runtime host.
- Live release acceptance remains for WI#164 (approval delivery) and
  goal 34 (WI#161 REPL argument integrity) fresh-tenant threads; evidence
  and limits are in their linked ops records. Goal 33's live acceptance
  (WI#162 shell truncation on fresh tenant `wi162verify`) completed
  2026-09-18 and is not carried.
- WI#205's path-blocking is not a claim that the sandbox is safe against the
  agent reading the daemon's environment by every other means: podman
  `keep-id` maps the container UID onto the tenant user, who legitimately
  owns those files. The durable fix — a sandbox-scoped gateway credential
  class — is designed but not implemented; see
  `docs/ops/v2.1.2.0_WI205_SANDBOX_SCOPED_CREDENTIAL_DESIGN.md`.
- MCP does not auto-install npm/pip packages and does not pass configured
  MCP servers into `lunarwing-agent-v1` external workers.
- The onboarding web app remains a trusted local console without user
  accounts or TLS; keep its loopback binding and use an SSH tunnel for
  remote access.
- OpenRC hosts without usable cgroup v2 delegation fall back to nice-only
  build scheduling.
- `ALLOW_PRIVATE_IPS=1` opts out of public-address SSRF restrictions; enable
  only for intentionally trusted private-network deployments.
- Kawarimi remains PostgreSQL-only.
- GitLab CI publishes protected, digest-bound worker images but does not
  publish daemon packages or deploy tenants automatically; mt-admin still
  performs tenant-local activation.
- OpenCode v1.18.29 remains deliberately un-adopted pending upstream proof
  (kimi-code 2.0.2 was adopted 2026-09-21 after the CLI contract gate
  proved the 2.x headless surface; if 2.x upstream regressions appear, the
  documented fallback is the last-proven 0.43.1 pin).
- The goal-9 commit audit's four mission-retry findings
  (`docs/ops/v2.1.2.0_ITEM9_COMMIT_AUDIT.md`: F1 `retry_attempt` payload
  injection, F2 unscoped dedup release incl. `GatePaused`, F3 restart-sweep
  ordering, F10 missing retry params in the `mission_update` schema) were
  all remediated in-cycle at goal 38 before release; live tenant restart
  acceptance of the recovery sweep remains an operator step
  (`docs/ops/2.1.2_FINDINGS.md`).
- The stale SSH-agent socket inode after daemon-only restarts was addressed
  in goal 40 by mounting the socket's dedicated parent directory rather
  than the socket file; existing worker containers switch mount shape on
  their next lifecycle pass. The separately reported `Permission denied`
  on fresh binds has not yet been confirmed fixed on a live tenant. The
  directory is now explicitly `0o755` for worker traversal; verify the
  fresh-bind and restart scenarios on a live tenant before closing that
  residual (`docs/ops/2.1.2_FINDINGS.md`).
- WI#159 (open): the engine event listener's terminal triggers
  (`thread_completed_with_issues`/`thread_completed_with_learnings`) fire
  for Research child threads — only Mission threads are filtered — so
  event-triggered missions can self-sustain via their own research
  children; with the WI#189 budget the blast radius is 30 spawns per day
  per mission.

### Resolved or Source-Fixed Since v2.1.1.0

- Rootless Podman works from tenant agent shells via manager-forked
  transient services (WI#181/#178, structural fix; live regression
  `podman_works_from_transient_service` runs real `podman ps`).
- The agent shell no longer inherits `DATABASE_URL` or other daemon secrets
  through the full-access sandbox lane (WI#141).
- Sandbox container leaks on shell-tool timeout are prevented by RAII
  in-flight cleanup guards and a socketless reaper sweep (WI#179).
- Shell output truncation is visible (`truncated` flag + warning) on both
  sandboxed and direct paths (WI#162, live-verified on fresh tenant).
- Stale cached tool output is purged before the first model call of every
  engine thread (WI#109; `_cached_at` orchestrator stamping landed).
- Pebble worker: project_dir confined to workspace root; mid-task cancels
  land (WI#101).
- Knowledge stale sweep covers the full store and runs independently of the
  heartbeat on multi-tenant fleets (WI#121).
- Skill registry sidecar provenance is read correctly; registry-installed
  skills no longer re-import on every restart.
- Sandbox cannot mount the tenant `env`/`state`/`run` directories, and
  workers no longer receive the full gateway bearer (WI#205, incl. the
  WI#209/#210/#211 review findings and WI#215 sibling derivation).
- Worker SSH-agent stale-inode fix: the agent socket lives in a
  dedicated `run/ssh-agent/` directory that mt-admin bind-mounts (not the
  socket file), so a fresh daemon-bound inode resolves at connect time and
  the `ssh-agent-dir` launch contracts recreate legacy file-mounted worker
  containers on their next lifecycle pass (item 40 / WI#206 stale-socket
  follow-up; live-tenant validation remains outstanding).
- Interrupted mission fires retry with backoff and survive daemon restarts;
  event/webhook delivery is at-least-once (WI#188; the goal-9 audit's
  follow-ups F1/F2/F3/F10 were remediated in-cycle at goal 38). Daily
  mission budget defaults to 30 (WI#189); mission tools reject
  nameless/goalless creates and expose `current_focus` (WI#203).
- Fresh tenants receive only the seven product skills, not contributor
  workflows (WI#185).
- GitLab WASM tool no longer collapses to the bare `action` discriminator
  under schema compaction (WI#150), and its "always allow" grants now work
  across both permission-key spellings in legacy chat and Engine V2
  (goal 13).
- Health pipeline: eight WI#180 findings plus six review findings fixed;
  runtime/knowledge are monitor-only diagnostics that escalate without
  restarts (WI#180).
- Conversation insights: nine-finding hardening program complete (goal 8,
  WI#132/WI#143/WI#183); durable cross-channel attachments; corrupted
  blocking tool responses demoted to text (WI#139 follow-up).
- CI/CD: giant-crate compile concurrency is runner-managed with OOM/ENOSPC
  runbooks for the known signal-9 class (promotion locks are the only
  serializing resource groups); opencode2
  publication jobs; ShellCheck 0.11.0 floor.
- Worker pins updated: kimi-code 2.0.2 (with the WI#195 headless permission
  contract, the WI#206 fail-closed PreToolUse command guard, and the
  deterministic CLI contract gate), opencode2 v2.0.15, gitlab
  tool registry 0.1.5; opencode stays v1.18.25; pebble stays `2d0f3786`.
