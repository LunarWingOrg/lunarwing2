# Release Notes for LunarWing v2.1.2.0 (DRAFT)

## Codename: Yoroi (鎧)

**Release date** TBD

>Plate by plate the armor is laced
>
>Not to stop every blade
>
>But to take the blow and stay standing
>
>The sand does not breach the walls
>
>The gate does not answer a stranger's hand
>
>The watch does not sleep through the third alarm
>
>鎧

## Overview

LunarWing v2.1.2.0 is a sandbox-hardening, approval-delivery, health-system,
conversation-integrity, and external-worker cycle. Its headline work is the
generic podman-first sandbox: the shell tool and container workloads now run
through a direct CLI runner that works rootless on both systemd and OpenRC —
no daemon socket required — with tiered network egress (proxied by default,
`none` or proxy-enforced isolation), cancellation-safe cleanup, and a reaper
that sweeps orphaned containers regardless of which runtime built them.
Workload spawning was then structurally fixed so podman runs correctly from
inside a tenant's hardened service context (WI#181: transient
manager-forked services instead of scope inheritance), and the
double-opt-in `full_access` lane no longer leaks the daemon's secret-bearing
environment into agent shells (WI#141).

Approval-gate delivery (WI#164) is the second pillar: pending gates are
authenticated, race-safely reconciled in the browser, resolvable through a
persisted conversation lookup with scoped auth-thread fallback, recoverable
by a bounded late-turn watcher that shares normal response hooks and channel
delivery, and pruned at startup — so a pending-gate resolution can no longer
be lost between the gate, the thread, and the channel.

The host health pipeline was reworked end-to-end (WI#180): LLM-stream
telemetry is actually serialized into reports, slow fleet probes keep partial
results instead of fabricating empty ones, alert freshness semantics are
scrape-time correct, the opencode2 and voice worker maps resolve to their
owning tenant for remediation, new runtime and knowledge healthpoints probe
the live gateway and bridges per tenant, and monitor-only policies follow
service identity instead of restarting diagnostic components.

Conversation-integrity work closes nine findings on the insights pipeline:
authority checks, automatic-recall isolation, generated-history feedback,
durable-save errors, host-captured evidence, atomic host-validated insight
writes, project-scoped event dispatch, a restart-safe durable cursor, and
deletion-as-persistent-opt-out. Files sent on any channel are now durably
retrievable from every other channel through a workspace-backed attachment
store with a read-only tool.

This cycle also adds the opencode2 external worker lane (OpenCode 2.x via a
new `@opencode/client/promise` bridge, ports-registry v17, and protected
CI image publication), hardens the Pebble and kimi workers (workspace-path
confinement, live mid-task cancellation, cancel-aware dispatch), fixes shell
output truncation reporting (WI#162), REPL argument corruption, the
knowledge stale-sweep scalability defects (WI#121), the skills registry
sidecar provenance bug, and pins current worker toolchains — Kimi Code
0.43.1, OpenCode v1.18.25, OpenCode 2 v2.0.8, Pebble v0.5.0. The CI/CD
pipeline itself was repaired for 16 GB-runner RAM contention and extended
with an opencode2 publication lane.

The release identifier is `2.1.2.0`; LunarWing-owned Rust crates and channel
packages use `2.1.2`. Rust 2024 and MSRV `1.96.1` are unchanged. There are
no new SQL migrations (`V27__indexed_at` from v2.1.1.0 remains latest). The
multi-tenant port registry moves from v16 to v17 (opencode2 worker block
51000–51999; `scripts/migrate-ports-v17.sh`).

See the [v2.1.1.0 release notes](docs/releases/RELEASE-v2.1.1.0.md) for the
previous compatibility baseline.

## Highlights

- **Generic podman-first sandbox.** A new CLI runner drives `podman run` /
  `docker run` directly — no Docker-compatible daemon socket — so sandboxing
  works rootless on systemd and OpenRC hosts alike. Hardened defaults:
  non-root UID, `cap-drop ALL`, `no-new-privileges`, read-only rootfs, tmpfs
  scratch, labeled in-flight registry, and `--stop-timeout`-bounded cleanup.
  Egress is tiered through `SANDBOX_NETWORK`: `proxy` (default, allowlisted),
  `none` (`--network none`), or WI#155 `proxy-enforced` — full isolation plus
  a per-execution Unix relay to the public-only, DNS-pinned proxy that fails
  closed. The reaper sweeps forged CLI containers without requiring an API
  socket, and a gated acceptance suite covers the hardening contract.
- **Sandbox workloads spawn as manager-forked transient services (WI#181 /
  WI#178).** `systemd-run --user --scope` inherited the daemon's
  `NoNewPrivileges=true` + `PrivateTmp=true` context, which podman's rootless
  userns join cannot cross. Workloads (shell tool direct path, sandbox
  execute-direct, sandbox podman CLI, registry WASM builds) now spawn through
  `systemd-run --user --wait --pipe --collect` transient services under
  `lunarwing-workloads.slice`, with mode-0600 `EnvironmentFile=` handoff and
  a `systemctl --user stop` teardown guard. Podman works from inside the
  tenant sandbox scope; the `fix-podman-userns` repair verb remains for
  pairings broken before this fix.
- **Daemon secrets no longer leak into agent shells (WI#141).** The
  double-opt-in `SANDBOX_POLICY=full_access` lane spawned `sh -c` inheriting
  the daemon's entire environment — including `DATABASE_URL`. The sandbox
  manager now applies the same allowlist scrub as the other lanes
  (`env_clear()` → `SAFE_ENV_VARS` → spawner runtime dir → caller env), which
  also strips `SANDBOX_POLICY`/`SANDBOX_ALLOW_FULL_ACCESS` from the child.
- **Approval-gate delivery, end to end (WI#164).** Pending gates are
  authenticated; the browser reconciles gate state race-safely; resolution
  survives restarts through a persisted conversation lookup and scoped
  auth-thread fallback; auto-approve configuration and legacy env aliasing
  are resolved with diagnostics; a bounded late-turn watcher re-drives
  expired responses through normal hooks and channel delivery; startup prunes
  disk gates; and proactive gateway messages route to the correct thread.
  Auth-gate identity survives token and cancel controls, and cancellation is
  durable.
- **Health pipeline rework (WI#180).** All eight tracker findings addressed
  and one full review round absorbed: `llm_streams` telemetry is serialized
  into saved reports; slow fleet probes emit partial results and degrade for
  coverage loss instead of timing out silently; report-age alerts evaluate at
  scrape time; LunarWing-specific alerts no longer match on the wrong status
  class (missing report is its own alert; skipped is a distinct status code);
  opencode2/voice worker units resolve to their owning tenant for
  remediation; slow OpenRC sweeps discover `rc-service` on a safe PATH; new
  `health-runtime.sh` probes the live gateway and XMPP bridge contracts per
  tenant within a shared sweep budget that marks budget-exhausted instances
  unprobed rather than unreachable; and new `health-knowledge.sh` monitors
  knowledge sync/sweep state with enablement-aware grace windows.
  Monitor-only policies now follow service identity, and runtime/knowledge
  escalate as monitor-only diagnostic components — never restarts.
  Verification: 926/926 tests across all 19 health suites.
- **Conversation-insights hardening (nine findings).** Authority checks on
  insight writes; automatic-recall isolation; generated history no longer
  feeds back as fresh user evidence; durable-save errors surface instead of
  pretending success; the host captures eligible user evidence with
  content-derived batch identity; insight writes are atomic and
  host-validated (citation re-validation, host-minted identity, upserts,
  five-per-run budget, explicit/inferred recall eligibility); event dispatch
  is project-scoped; the batch cursor is durable and restart-safe; and
  deletion is a persistent opt-out with optional scoped purge and explicit
  resume. Investigation record: `docs/ops/v2.1.2.0_GOAL8_CONVERSATION_INSIGHTS_FINDINGS.md`.
- **Durable cross-channel attachments.** Every inbound attachment — XMPP,
  Signal, DarkIRC, WASM channels, WebUI — is persisted into workspace memory
  under `attachments/<channel>/<id>/<filename>.md` via the normal workspace
  write path: injection-scanned, search-indexed, and visible in the WebUI
  memory tree. A built-in read-only `attachment_store` tool (list/read) is
  registered in single-user and multi-tenant paths, protected from dynamic
  shadowing and attenuation-safe. A file sent on one channel stays
  retrievable from all other channels.
- **opencode2 external worker.** OpenCode 2.x joins the external-worker
  fleet as a separate lane (v1 stays pinned and default): new worker image, a
  TypeScript bridge over `@opencode/client/promise`, v2 configuration,
  ports-registry v17 block (51000–51999), mt-admin lifecycle mirroring with a
  34-assertion harness, workspace-path confinement, and protected CI
  candidate/scan/promote jobs with the same digest-bound reproducibility
  contract as the other workers. Pinned at upstream v2.0.8
  (`7673ed6bd6547ee0dcb81aab55f1392fb751d652`), with the upstream diff
  reviewed for bridge-relevant compatibility.
- **OpenCode 1 stays the default lane at v1.18.25**
  (`cb7d8b2f5e44876ef98b661dc10590c915af3a9f`), the last release proven
  end-to-end in this worker; v1.18.29's documented upstream regressions keep
  the rollback in force.
- **Kimi Code worker bumped to 0.43.1** (npm `latest` for this line;
  `node >=22.19.0` engine satisfied by the pinned node 22 base). The 2.x line
  is deliberately not adopted. Pebble remains at v0.5.0 (upstream HEAD
  `2d0f3786`).
- **Pebble worker hardening.** A daemon-supplied `project_dir` can no longer
  point the worker at arbitrary host directories: the bridge resolves and
  confines the requested project dir before dispatch (escapes get a
  `task_result` error envelope and nothing spawns), `spawn_pebble` re-resolves
  fail-closed, and a global `WorkspaceRegistry` enforces one task per
  workspace subtree across all connections. The WS read loop now stays live
  for the entire connection, so mid-task `cancel` envelopes are received and
  kill the pebble subprocess with a `cancelled` `task_result` instead of
  running to the 60-minute timeout. Restart parity fixed (WI#128): Pebble
  Quadlet shutdown stops the owning systemd user unit first, and startup
  clears any `StartLimitBurst` lockout.
- **Shell output truncation is reported, not silent (WI#162).** Both the
  sandbox-manager path (32,768 bytes per stream) and the direct path (65,536
  bytes) now return `truncated` plus a `warning`, and partial output starts
  with `[output truncated]` so prefix-only previews retain the signal.
  Live-verified on a fresh OpenRC tenant running the real-container
  regression through podman 5.8.2.
- **REPL/CodeAct argument integrity.** Name-only and final argument assembly
  plus nested-fence truncation are fixed; pre-execution REPL argument
  validation returns a distinct bounded repair failure, and
  `repl_argument_corruption_total` is exported in tenant-aware stream
  telemetry.
- **Knowledge stale-sweep defects fixed (WI#121).** The 1,000-note
  `KNOWLEDGE_SCAN_CAP` window is gone — `knowledge_list_notes` pages the whole
  store with a keyset cursor — and the sweep gained a heartbeat-independent
  daemon loop (`KNOWLEDGE_SWEEP_ENABLED`), so default multi-tenant
  configurations (heartbeat disabled) actually sweep. The Python orchestrator
  also stamps `_cached_at` on Tier 2 tool outputs, making the pre-execution
  staleness sanitizer (WI#109) fully effective.
- **Pre-execution state sanitizer (WI#109).** Before the first model call of
  every thread (missions included), stale cached tool state is purged:
  time/date/datetime always; web_search/http/shell when older than 30 minutes
  via `_cached_at`. Sanitized state persists back to the runtime checkpoint
  with a `sanitized_at` marker.
- **Skills registry sidecar fixed.** `read_registry_sidecar` was looking one
  directory too high, so registry provenance silently never loaded —
  catalog-pulled skills were excluded from update checks and stale
  filesystem skills re-imported on every restart. The reader now joins
  `.registry.json` on the skill's own directory with a legacy fallback;
  provenance stamps complete `V2SkillMetadata`.
- **CI/CD repairs and audit.** Giant-crate compiles are serialized under the
  `rust-compile-16gb` resource group (including cavepony-wasm) so two 16 GB
  jobs no longer OOM on the same runner — confirmed by reference pipeline
  #3742's `signal 9` — with a worker-scan actionable guard, OOM/ENOSPC
  runbooks, and a full audit doc. ShellCheck floor raised to a
  checksum-pinned 0.11.0. New opencode2 candidate/scan/promote jobs match the
  other workers' digest-bound contract.
- **Version sweep.** All 11 LunarWing-owned release packages moved 2.1.1 →
  2.1.2 with refreshed lockfiles; channel registry packages bumped (darkirc
  0.1.8, multica 0.1.11, weechat 0.2.9, xmpp 0.1.14); external-crate audit
  found zero semver-compatible updates across 26 tracked lockfile roots
  (behind-latest set is MSRV-1.96.1-blocked); `ic/vendor/` regenerated.
- **Documentation repasses.** All 31 guides, all 10 architecture docs, and
  the ops/MT docs were revalidated against the 2.1.2.0 tree, including the
  ports-registry v15/v16/v17 block tables, the podman sandbox environment,
  the attachment tool, knowledge sweep rework, and current worker pins.

## Compatibility

| Area | v2.1.2.0 behavior |
|---|---|
| Release version | `2.1.2.0` |
| Crate/package version | `2.1.2` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | None; `V27` (from v2.1.1.0) remains latest |
| Multi-tenant port registry | v17 (was v16): opencode2 block 51000–51999; migrate with `scripts/migrate-ports-v17.sh` |
| Channel registry patches | darkirc 0.1.8, multica 0.1.11, weechat 0.2.9, xmpp 0.1.14 (all bumped) |
| WASM runtime | Wasmtime / wasmtime-wasi 36.0.14 (unchanged) |
| Dependency resolution | `ic/vendor/` source-replaces crates.io for `ic/`-rooted manifests; refreshed this cycle (no external version changes) |
| Worker image tool pins | kimi-code `0.43.1`; opencode `v1.18.25` at `cb7d8b2f5e44876ef98b661dc10590c915af3a9f` (unchanged, still the proven default); opencode2 `v2.0.8` at `7673ed6bd6547ee0dcb81aab55f1392fb751d652`; pebble `v0.5.0` at `2d0f3786` |
| Worker image publication | Digest-bound reproducible candidates/promotions now cover four workers (kimi, pebble, opencode, opencode2) |
| Sandbox defaults | Production shell tool sandboxed and fail-closed; `SANDBOX_NETWORK=none` is the in-code and mt-admin default; dev direct path requires `ALLOW_LOCAL_TOOLS`/`SANDBOX_ENFORCE` opt-outs |
| GitLab tool WASM | Registry patch bumped for the discriminated-union schema fix (gitlab 0.1.5) |
| CI scope | Protected worker-image candidate publication/promotion for four workers; daemon/package publication and tenant deployment remain deferred |

## Changes

### Generic Podman Sandbox and Egress (items #6, #21/#26, WI#179/WI#155/WI#178)

- New `ic/src/sandbox/cli_runner.rs` drives `podman run`/`docker run`
  directly with no daemon socket; rootless works on systemd and OpenRC.
  `SandboxManager` swapped bollard for the CLI runner; the proxy starts only
  for sandboxed policies in `proxy` network mode. A standalone CLI-sweep
  reaper loop handles socketless podman deployments and scans the runtime
  the runner actually selected.
- Hardened execution: non-root UID, cap-drop ALL, no-new-privileges,
  read-only rootfs, tmpfs, `--rm`, `lunarwing.sandbox` labels plus an
  in-flight registry; explicit `--pids-limit 256`; seccomp policy recorded as
  the runtime default profile. Docker UID mapping (`user_for()`) handles
  rootless (`0:0` in userns) vs rootful docker; podman keeps the keep-id
  ladder.
- Tiered egress: `SANDBOX_NETWORK=proxy` (allowlist proxy via
  `host.containers.internal`, deny bodies prefixed `sandbox-proxy denied:`),
  `none` (`--network none`, in-code and mt-admin default), or WI#155
  `proxy-enforced` — `--network none` plus a per-execution Unix relay to the
  public-only, DNS-pinned proxy. Relay setup fails closed; the proxy pins DNS
  per request, rejects non-public destinations, and never follows redirects
  on the host. New knobs: `SANDBOX_RUNTIME` / `SANDBOX_NETWORK` /
  `SANDBOX_ENFORCE` (env + `[sandbox]` settings).
- Cancellation-safe cleanup: `InflightGuard` (RAII unregister) +
  `ContainerCleanupGuard` (detached kill/rm on drop) plus `--stop-timeout 10`
  mean the shell tool's outer timeout can no longer leak a container.
- Transient-service workload spawning (WI#181/WI#178): workloads spawn as
  manager-forked `systemd-run --user --wait --pipe --quiet --collect`
  transient **services** under `lunarwing-workloads.slice` instead of scopes
  that inherit `NoNewPrivileges=true` + `PrivateTmp=true` context that blocks
  podman's userns join. Scrubbed environment and working directory are handed
  over via a mode-0600 `EnvironmentFile=` and `--working-directory=`;
  `ProcessTreeGuard` carries the unit name and stops it via
  `systemctl --user stop` (SIGTERM→SIGKILL per `TimeoutStopSec`,
  `LUNARWING_WORKLOAD_TIMEOUT_STOP` knob). All four workload call sites are
  wired: shell tool direct path, sandbox execute-direct, sandbox podman CLI,
  registry WASM builds. Verified: 5,104/5,104 lib tests; real `podman ps`
  through a transient service exits 0; doc `docs/ops/v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md`.
- The shell tool registers fail-closed via `register_sandboxed_shell`
  (production always sandboxed; the dev `ALLOW_LOCAL_TOOLS` direct path is
  opt-in and opt-out-enforceable) and errors carry the structured
  `[sandbox:<kind>]` envelope. A gated acceptance suite
  (`ic/tests/generic_sandbox_acceptance.rs`) records the hardening,
  isolation, and egress contracts for runtime hosts.
- WI#141: the `SANDBOX_POLICY=full_access` + `SANDBOX_ALLOW_FULL_ACCESS=true`
  direct lane now scrubs the daemon environment before spawning, closing the
  `DATABASE_URL` leak; regression tests pin the scrub and the passthrough
  contract.
- Boundary documentation separates the generic sandbox (agent shell/command
  execution) from the orchestrator `job_manager`; docs updated across
  `NETWORK_SECURITY.md`, `.env.example`, `ENABLING_DEV_TOOLS.md`,
  `WORKER-CONTAINERS.md`, and `FEATURE_PARITY.md`.

### Approval-Gate Recovery and Delivery (item #31, WI#164)

- Authenticated pending-gate recovery with race-safe browser reconciliation;
  resolved auto-approve configuration and legacy env-alias diagnostics.
- Persisted conversation lookup and scoped auth-thread fallback so a gate
  resolution can find its conversation after a restart; startup prunes
  stale disk gates.
- A bounded late-turn watcher shares normal response hooks, history,
  auth/gate handling, and channel delivery; proactive gateway messages route
  to the thread.
- MR-follow-up hardening: exact auth-gate identity preserved through
  token/cancel controls; cancellation is durable.
- Verified: bridge 230/230, gateway server 30/30, gates 44/44, agent loop
  21/21, 13 browser scenarios; clippy `-D warnings` clean. Evidence:
  `docs/ops/v2.1.2.0_GOAL31_WI164_APPROVAL_DELIVERY.md`. This records the
  implementation; release-line acceptance over live channels is the release
  gate's final check.

### Host Health Pipeline (item #25, WI#180)

- Telemetry serialization fixed: the `llm_streams` component is now included
  in the driver's saved report instead of being aggregated but dropped.
- Probe robustness: safe PATH includes `/usr/local/sbin:/usr/sbin` for
  scheduled OpenRC hosts; sweep deadlines emit unprobed entries and degrade
  for coverage loss; the driver keeps valid partial JSON when a component
  hits the outer timeout; runtime probes are capped by the remaining sweep
  budget so a scaled fleet never exits 124 with an empty report.
- Alert hygiene: report age is evaluated at scrape time
  (`time() - report_timestamp_seconds`); new `LunarWingHealthReportMissing`
  alert; unit-down matches status 2 only; skipped is a distinct code 4;
  example thresholds and dashboard updated.
- Tenant/role resolution: opencode2 and voice roles (including `-sup`
  supervisor suffixes) strip to the owning tenant so remediation stays on the
  tenant user bus; health-systemd probes opencode2 container health.
- New `health-runtime.sh` queries each tenant's live gateway
  (`/api/gateway/status`, `/api/chat/pending-gates`) and XMPP bridge
  (`/v1/status`) contracts via ports-registry discovery with bearer tokens
  from tenant env files; unhealthy gateway channels degrade the tenant and an
  unobservable pending-gates surface degrades as an observability gap.
- New `health-knowledge.sh` reads `knowledge_sync_state.json` and
  `knowledge_sweep_state.json` with `KNOWLEDGE_SYNC_ENABLED` /
  `KNOWLEDGE_SWEEP_ENABLED` enablement awareness and cadence-derived overdue
  budgets; enabled-but-never-observed degrades only after a startup grace
  window.
- Self-heal semantics: runtime/knowledge register as monitor-only diagnostic
  components that escalate via synthetic `runtime@<tenant>` /
  `knowledge@<tenant>` identities — never restarts; LunarVoice monitor-only
  follows service identity; the absent legacy sessions dir is
  healthy-with-note instead of degraded.
- Verified: 926/926 tests across all 19 suites (new gateway, runtime,
  knowledge, and expanded matrix/exporter suites); repo-wide shellcheck
  clean with the new 0.11.0 floor.

### Conversation Insights and Confabulation Hardening (item #8, WI#132/WI#143/WI#183)

- Nine documented findings addressed across two batches
  (`8insights` / `8insightsaction`): authority checks on insight writes;
  automatic-recall isolation; generated-history feedback prevention; durable
  save-error surfacing; host-captured eligible user evidence with
  content-derived batch identity; atomic host-validated insight writes
  (citation re-validation, host-minted identity, upserts, five-per-run
  budget, explicit/inferred recall eligibility); project-scoped event
  dispatch; durable restart-safe batch cursor; and deletion as a persistent
  opt-out with optional scoped purge and explicit resume.
- Verified: 611/611 engine tests, 24/24 production-store tests, clippy
  `-D warnings`. Records:
  `docs/ops/v2.1.2.0_GOAL8_CONVERSATION_INSIGHTS_FINDINGS.md` and
  `docs/ops/v2.1.2.0_GOAL8_INSIGHTS_HARDENING.md`. The September 16 incident's
  causal trigger remains under investigation on its tracked work item.

### Durable Cross-Channel Attachments (item #3)

- `agent::attachment_store` persists every inbound attachment to workspace
  memory (`attachments/<channel>/<id>/<filename>.md` with sanitized metadata,
  extracted text, bounded base64 inline data) via the normal workspace write
  path before any channel processes the message — injection-scanned,
  search-indexed, WebUI-visible.
- Built-in `attachment_store` tool (list/read) registered alongside memory
  tools in single-user and multi-tenant paths, shadowing-protected,
  read-only under skill attenuation, seeded AlwaysAllow, schema-validated,
  path-confined to `attachments/`.
- Verified: full lib suite 5,026 passed with 12 new tests; docs updated in
  `docs/architecture/ENGINE-V2.md`.

### opencode2 External Worker Lane (items #13/#21/#30/#40/#48, plan `docs/plans/OPENCODE2_EXTERNAL_WORKER_PLAN.md`)

- New lane alongside v1 opencode: worker image, `@opencode/client/promise`
  bridge, v2 config, ports-registry v17 opencode2 block (51000–51999),
  mt-admin lifecycle mirroring (ports, env, units, health), a 34-assertion
  harness, and workspace confinement. The daemon side required zero Rust
  changes — `sandbox.external_workers` is fully generic.
- CI: `worker-opencode2-candidate/scan/promote` jobs extend the shared
  templates with the digest-bound reproducible-candidate contract, Trivy
  CRITICAL gate + SBOM, and the `$CI_COMMIT_TAG-opencode2` immutable
  promotion tag; validation cross-checks the Dockerfile↔build-script pins
  (v2.0.8 / `7673ed6b…`).
- Pinned opencode2 `v2.0.5` → `v2.0.8` (`7673ed6b…`, tag SHA verified against
  both upstream remotes); upstream build script is byte-identical between
  v2.0.5 and v2.0.8, so the reproducible-build patch applies unchanged;
  bridge-relevant SDK surfaces reviewed as additive-only.
- Eval-tenant rollout remains the deferred final phase per plan.

### Worker Hardening and Pins (items #12/#17/#20, WI#128/WI#101)

- Kimi Code worker: 0.42.0 → 0.43.0 → 0.43.1 (lock integrity matches the npm
  registry sha512; engine requirement `node >=22.19.0` satisfied by node 22).
  The 2.x line deliberately not adopted.
- Pebble worker: workspace-path confinement (resolve + workspace-overlap +
  symlink/`..` escape rejection at both bridge and spawn layers), a global
  `WorkspaceRegistry` enforcing one task per workspace subtree, and a live WS
  read loop so mid-task cancels kill the pebble subprocess and return
  `cancelled`. Restart parity: Quadlet shutdown stops the owning user unit
  first with a container-stop fallback; startup clears `StartLimitBurst`
  lockouts (WI#128).
- Pins: kimi-code `0.43.1`; opencode `v1.18.25` `@cb7d8b2f` (default lane);
  opencode2 `v2.0.8` `@7673ed6b`; pebble `v0.5.0` `@2d0f3786`. The attempted
  OpenCode v1.18.29 update remains rolled back (documented upstream
  regressions).

### Runtime Correctness Fixes

- Shell truncation (WI#162): both buffered paths report `truncated` +
  `warning`; partial output is prefixed `[output truncated]`; combined
  clipping is flagged; live-verified in real containers on podman 5.8.2
  (OpenRC tenant). Evidence:
  `docs/ops/v2.1.2.0_GOAL33_WI162_SHELL_TRUNCATION.md`.
- REPL argument integrity (WI#163 investigation re-verification plus item #34
  in progress): name-only/final argument assembly fixed, nested-fence
  CodeAct truncation fixed, pre-execution REPL argument validation added with
  a distinct bounded repair failure, and `repl_argument_corruption_total`
  telemetry exported. Engine/LLM/bridge/integration suites pass; live-tenant
  verification steps are recorded in
  `docs/ops/GOAL34_REPL_ARGUMENT_INTEGRITY.md`.
- Knowledge sweep (WI#121): keyset pagination replaces the 1,000-note cap
  (both Postgres and libSQL comparison forms, 500-row pages, 100k ceiling,
  no-progress guard); a heartbeat-independent `run_knowledge_sweep_loop`
  gives every `KNOWLEDGE_SWEEP_ENABLED` tenant a cadence even with the
  heartbeat disabled.
- Pre-execution state sanitizer (WI#109): Tier 1 temporal and stale Tier 2
  cached tool state is purged synchronously before the first model call of
  every thread; the orchestrator stamps `_cached_at` on Tier 2 outputs
  (follow-up commit).
- Skills registry sidecar path fixed: `.registry.json` is read from the
  skill's own directory (with a legacy file-shaped fallback), restoring
  provenance stamps and the registry-installed migration dedup guard.
- Stream-integrity blocking path closed (WI#139 follow-up): the blocking
  `complete_with_tools()` path in `StreamIntegrityProvider` now clears
  tool calls from corrupted output, matching the streaming path.
- Missions (WI#163): re-verified all four fix commits (broadcast
  `thread_start`, `system:<source>/<event_type>` cadence grammar, `_cached_at`
  stamp, terminal-event fleet) against HEAD; investigation record in
  `docs/ops/v2.1.2.0_ITEM32_WI163_REVERIFICATION.md`. Tracker hygiene for
  pre-lint mission rows remains.

### CI/CD Repairs and Publication (item #28)

- The `rust-compile-16gb` resource group serializes giant-crate compile jobs
  (rust-check-default/lib/libsql/postgres clippy/test targets and
  cavepony-wasm), eliminating the cross-pipeline overlap that killed
  reference pipeline #3742's `rust-test-lib` with `sccache: Compile
  terminated by signal 9`.
- Worker-scan actionable guard; OOM/ENOSPC runbooks (`.gitlab/README.md`);
  full audit with five improvement suggestions in
  `docs/ops/v2.1.2.0_CICD_AUDIT_ITEM28.md`.
- ShellCheck floor is 0.11.0 via checksum-pinned
  `.gitlab/scripts/install-shellcheck.sh`; the CI job installs it into
  `.ci-bin`; bookworm's 0.9.0 is no longer used.
- New opencode2 protected publication lane (candidate → Trivy scan →
  digest-bound promotion), image built from `opencode2.4lunarwing` with
  `OPENCODE_VERSION=v2.0.8` / `OPENCODE_REF=7673ed6b…`.
- Verified locally: `validate_config.py`, `promote-worker-image.py`
  self-test, `/ci/lint` valid on the merged config, shell-syntax/python-syntax
  suites green. Pipeline #3742's corroborating OOM forensics are in the audit
  doc.

### Version, Vendor, and Tool Bump Sweep (item #1)

- All 11 LunarWing-owned release packages 2.1.1 → 2.1.2 (daemon, five
  workspace crates, XMPP bridge, four WASM channels); six lockfiles
  refreshed; channel registry darkirc 0.1.8, multica 0.1.11, weechat 0.2.9,
  xmpp 0.1.14. A tracked-manifest/lock audit confirms no project-owned
  package remains at 2.1.1.
- External crates: 26 tracked lockfile roots all report
  `Locking 0 packages`; the behind-latest set is MSRV-1.96.1-blocked;
  wasmtime stays 36.0.14; `ic/scripts/refresh-vendor.sh` regenerated
  `ic/vendor/` (1,053 dirs, 844 unique names) with no content diff.
- WASM tool registries carry lock-consistent bump discipline; the gitlab
  tool advertises discriminated-union variant-required fields again
  (compact-schema fix, registry 0.1.5) after the v2.1.1.0 regression.
- Verified clean: `cargo check --locked` across default/PostgreSQL-only/
  libSQL-only/all-features; XMPP bridge native check; all four channels on
  `wasm32-wasip2`; `cargo deny check`; rustfmt; clippy `-D warnings` on all
  features.

### Documentation (items #38/#42/#43/#45/#47)

- All 31 guides revalidated against the 2.1.2.0 tree with substantive updates
  to MT-ADMIN-QUICKSTART (`fix-podman-userns`), ENABLING_DEV_TOOLS (generic
  podman sandbox env/knobs), CONTRIBUTOR-ONBOARDING, TESTING_GUIDE
  (attachment tier, sandbox checks), and TENANT_KNOWLEDGE_SEED (sweep
  rework).
- All 10 architecture docs revalidated; ENGINE-V2 records the attachment
  pipeline and thread-start broadcast; SEMANTIC-MEMORY-SEARCH stays current
  on the V27 outbox.
- `docs/ops/MULTITENANCY-PRODUCTION.md` port-blocks now cover the v15
  LunarVoice, v17 opencode2, and host-global health_metrics slots;
  `docs/ops/TENANT-CONFIGURATION.md` documents registry v17; a real code gap
  the repass exposed — the ports-registry collision probe and `ports list`
  omitting the opencode2 map — was fixed in mt-admin.
- `docs/releases/README.md` remains the canonical release index; this draft
  will be moved into `docs/releases/` at tag time.

## Upgrade Notes

1. Back up PostgreSQL, tenant environment files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state.
2. Run `ic/scripts/migrate-ports-v17.sh` to take the port registry from v16
   to v17 (adds the opencode2 worker block). No SQL migration is required;
   `V27` remains latest.
3. Rebuild the daemon and installed WASM extensions through the normal
   deployment path. All four channel registry versions changed (darkirc
   0.1.8, multica 0.1.11, weechat 0.2.9, xmpp 0.1.14); reinstall channels.
4. Sandbox behavior changes: the shell tool is fail-closed sandboxed in
   production. Review `SANDBOX_RUNTIME` (podman default), `SANDBOX_NETWORK`
   (`none` default; `proxy` for allowlisted egress; `proxy-enforced` for
    isolated-plus-relay egress), and `SANDBOX_ENFORCE`. If you relied on the
   dev direct path, note it now requires `ALLOW_LOCAL_TOOLS` and honors
   `SANDBOX_ENFORCE=true`. Operators with pre-existing broken podman pairings
   should run the `fix-podman-userns` repair verb once after upgrade.
5. If you run agent shells with the double-opt-in `full_access` lane, audit
   any automation that depended on inheriting the daemon environment — it now
   receives only the allowlist plus explicit caller env.
6. Health pipeline: the new runtime/knowledge healthpoints and alert
   semantics may change alert volume on first rollout. Review
   `HEALTH_*` thresholds, the new `LunarWingHealthReportMissing` alert, and
   the skip/status-code mapping before promoting dashboards. Leave
   `HEALTH_LUNARVISION_URL` unset on multi-tenant hosts.
7. Workers: rebuild/pull worker images by the digest-evidence path (no
   `latest` or short-SHA). New pins: kimi-code 0.43.1; opencode stays
   v1.18.25 (do not restore v1.18.29); opencode2 v2.0.8 if you participate in
   that lane; pebble v0.5.0. Kimi Code 0.43.x requires the existing node 22
   base.
8. opencode2 tenants: opt in by completing the eval-tenant rollout steps in
   `docs/plans/OPENCODE2_EXTERNAL_WORKER_PLAN.md` (image + bridge + mt-admin
   mirror + ports v17). v1 opencode remains the default and recommended lane
   for production.
9. Approval-gate operators: review
   `docs/ops/v2.1.2.0_GOAL31_WI164_APPROVAL_DELIVERY.md` for the accepted
   delivery contract and remaining live-channel acceptance steps before
   treating the path as fully accepted fleet-wide.
10. Insights/mission operators: deletion is now a persistent opt-out for
    insights; review `docs/ops/v2.1.2.0_GOAL8_INSIGHTS_HARDENING.md` if your
    tenant tooling manages insight rows directly. Tracker hygiene for
    pre-lint tenant mission rows (WI#163 follow-up) is an operator action.

### Active Canonical Runtime and Integration Follow-ups

- **WI#164 live-channel acceptance.** Repo-side implementation and test
  evidence are complete; final acceptance across live XMPP/WeeChat/gateway
  channels is the release gate's remaining step (tracked in the goal-31
  evidence doc).
- **Item #34 live-tenant verification.** REPL argument-integrity fixes are
  repo-verified; fresh-tenant verification steps are prepared in
  `docs/ops/GOAL34_REPL_ARGUMENT_INTEGRITY.md` pending the operator run.
- **September 16 confabulation incident.** The controlled reproduction plan
  is in the goal-8 findings report; the causal trace remains open.
- **Tracker hygiene.** Several work items (WI#109, WI#121, WI#141, WI#163)
  are source-fixed and awaiting tracker closure/verification by their filers.

### Security, Deployment, and Release Limitations (carried)

- DarkIRC Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
  delivery remain disabled behind separate acceptance gates.
- MCP does not auto-install npm/pip packages, has no signed-package
  marketplace CLI, and does not pass configured MCP servers into
  `lunarwing-agent-v1` external workers; `header_secrets` covers static
  bearer headers only.
- The onboarding web app remains a trusted local console without user
  accounts or TLS; keep loopback binding and token auth, use SSH tunnels for
  remote access.
- OpenRC hosts without usable cgroup v2 delegation fall back to nice-only
  build scheduling instead of hard CPU/memory/task limits.
- `ALLOW_PRIVATE_IPS=1` opts out of public-address SSRF restrictions; enable
  only for trusted private-network deployments.
- Kawarimi remains PostgreSQL-only.
- GitLab CI publishes protected, digest-bound worker images (now four
  workers); it still does not publish daemon packages or deploy tenants —
  mt-admin performs tenant-local activation.
- Large worker-image pushes can still hit registry `499 Client Closed
  Request` until proxy-chain read timeouts are raised on the GitLab host.
  See `docs/ops/REGISTRY_PUSH_499.md`.

### Resolved or Source-Fixed Since v2.1.1.0

- Generic podman-first sandbox with rootless systemd+OpenRC support, tiered
  egress (proxy / none / proxy-enforced), cancellation-safe cleanup, and a
  socketless reaper.
- WI#181 transient-service workload spawning restores podman inside tenant
  sandbox scopes; WI#141 ends daemon-environment inheritance in the
  `full_access` shell lane.
- WI#164 approval-gate delivery: authenticated, restart-safe, race-safe
  gate resolution with durable cancellation and late-turn watching (live
  acceptance pending).
- WI#180 health pipeline: complete rework with new runtime/knowledge
  healthpoints and 926 passing suite assertions.
- Conversation-insights hardening: all nine findings implemented and
  regression-covered.
- Durable cross-channel attachments with the `attachment_store` tool.
- opencode2 worker lane with protected CI publication; pins at v2.0.8.
- WI#162 shell truncation reporting (live-verified), WI#163/`loop` REPL
  argument integrity (repo-verified batch 1/2), WI#109 pre-execution
  sanitizer, WI#121 knowledge sweep fixes, skills sidecar path fix, WI#139
  blocking stream-integrity guard.
- CI/CD OOM serialization fix (rust-compile-16gb), ShellCheck 0.11.0 floor,
  opencode2 publication lane.
- Worker pins: kimi-code 0.43.1, opencode v1.18.25 (held), opencode2 v2.0.8,
  pebble v0.5.0; Pebble workspace confinement and live cancellation.
- All LunarWing-owned packages at 2.1.2 with refreshed lockfiles and a clean
  external-crate/window audit.
