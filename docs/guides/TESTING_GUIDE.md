# LunarWing Release Testing Guide

Use this checklist for manual release validation. The resource-bounded commands, harness sequence, gateway smoke test, external-worker runner, and browser E2E instructions live in [`../ops/PRE-RELEASE-TESTING.md`](../ops/PRE-RELEASE-TESTING.md).

**Revalidated against source 2026-11-10 (v2.2.0.0).** The referenced ops
runbook, `ic/scripts/release-test.sh`, and channel/worker/multi-tenant tiers
were checked against the current tree. The checklist now covers the v2.1.9
response/cache and worker-source baselines plus v2.2 sandbox networking and
storage diagnostics, Multica startup errors, WeeChat rotation, billed-empty
retry bounds, legacy steering, and redacted mission-load diagnostics. Older
versioned entries below remain useful regression tiers, not claims that they
were rerun for this documentation change.

Record the exact commit, host profile, feature set, database backend, init system, and result for every tier. Never carry a previous release's pass count forward.

## Before Testing

- [ ] Define the release scope, success criteria, rollback trigger, and known deferrals.
- [ ] Confirm the release candidate commit and verify the worktree is understood.
- [ ] Review schema changes and take a restorable database backup before migration tests.
- [ ] Prepare isolated test credentials and disposable tenants; do not use production secrets.
- [ ] Start long-running Rust and harness checks in `tmux`.
- [ ] Limit every Cargo command to twelve CPUs and twelve jobs (`taskset -c 0-11`, `-j12`), as documented in the repo-root `AGENTS.md` build constraints.
- [ ] Use `cargo check` for compile verification; do not run a full debug build.

## Automated Baseline

Run the required tiers from [`../ops/PRE-RELEASE-TESTING.md`](../ops/PRE-RELEASE-TESTING.md):

- [ ] `cargo fmt --all -- --check`
- [ ] default and all-feature `cargo check`
- [ ] Rust tests with at most twelve test threads
- [ ] all-feature Clippy with warnings denied
- [ ] single-tenant/full-stack harness build, start, verify, and cleanup
- [ ] authenticated `ic/scripts/release-test.sh` gateway smoke tests
- [ ] configured external-worker smoke matrix
- [ ] browser E2E and supplemental boundary checks where available

Treat a missing external dependency as a documented deferral, not a pass. If only the harness TensorZero probe fails, check its configured upstream before declaring the local deployment broken.

## Channels

For each enabled channel, test inbound and outbound delivery, sender identity, authorization, restart recovery, and rate limits.

- [ ] **XMPP direct message** — send and receive text; verify pairing/owner policy.
- [ ] **XMPP room** — send and receive MUC traffic; verify sender attribution.
- [ ] **XMPP attachments** — exercise the configured upload path and verify size/auth limits.
- [ ] **Durable cross-channel attachments** (v2.1.2.0) — send a file on one channel, then verify it is retrievable from another via the `attachment_store` tool (list/read under `attachments/<channel>/…` in workspace memory).
- [ ] **Per-tenant sandbox image** (v2.1.3.0, WI#223) — for a fresh mt-admin tenant, confirm `SANDBOX_IMAGE=lunarwing-sandbox:latest` and `SANDBOX_AUTO_PULL=false` in the rendered env, the image exists in the tenant's rootless store, and a sandboxed shell execution succeeds; recover a missing image with `build-sandbox-image <tenant>` and re-verify.
- [ ] **Tenant system-prompt editor** (v2.1.4.0, WI#220) — in the gateway, open Settings → System Prompt, edit the primary-scope source, save, and confirm the workspace preview; create a missing identity file; verify a failed save preserves the draft and that scope isolation holds across tenants. Engine V2 activation requires `restart-tenant` plus a new conversation (see `TENANT-SYSTEM-PROMPTS.md`).
- [ ] **Tool runtime-estimate hints** (v2.1.4.0, WI#191) — with `LUNARWING_TOOL_ESTIMATES=1`, after five completed calls of one tool by one user, confirm an advisory "typical runtime" line appears on the next approval; with `=0`, confirm no recording and no hints; confirm the default (unset) observation-only behavior shows no hints.
- [ ] **routine_history over Engine V2 missions** (v2.1.4.0, WI#265) — fire an Engine V2 learning mission, then verify `routine_history <mission-name>` resolves the mission and renders recent fires; a not-found error must name both registries and point at `routine_list`/`mission_list`.
- [ ] **mt-admin read-only diagnostics** (v2.1.4.0, WI#268) — `mt-admin psql <tenant>` runs psql inside the tenant's own PostgreSQL container (operator discipline: SELECTs and fingerprints only — it does not itself enforce read-only, and DB rows can be secret-bearing); `mt-admin daemon-pid <tenant>` prints the validated daemon PID and exits non-zero (treated as uninspected) when the unit is down.
- [ ] **WeeChat WSS** — connect relay and adapter, exchange messages, then reconnect after a service restart.
- [ ] **WeeChat coordinated password rotation** (v2.2.0.0, WI#577) — with a deliberately stale `weechat_relay_password` store entry, run `rotate-relay-password <tenant>` through the current start-barrier units; WeeChat, adapter, and daemon must use the rotated managed `RELAY_PASSWORD`, `/api/wait` must not loop on 401, and unsetting the managed value must restore store fallback without printing either secret.
- [ ] **Multica startup diagnostics** (v2.2.0.0, WI#573) — leave `multica_url` or `workspace_id` null, absent, or blank and confirm extension/CLI status and gateway `channel_config_errors` name only the missing field, the channel is not active, and neither runtime watchdog restarts it. Plant a guest-side startup failure and verify health/listings say only `channel failed to start; see daemon logs`, never the guest text; corrected setup must pass full activation, and removal must clear the failure.
- [ ] **DarkIRC** — verify the tenant adapter, contact-key boundary, delivery, and health reporting.
- [ ] **WASM channels** — install, configure, activate, poll, and remove a disposable extension.
- [ ] **Gotify** — send a real test notification without exposing the application token.

## Agent, Threads, and Approvals

- [ ] Create a fresh thread and continue an existing thread.
- [ ] Switch threads while one has a pending approval; confirm the pending card is restored.
- [ ] Approve once, deny, and use **Always** on a configurable tool.
- [ ] Confirm intrinsically always-gated tools cannot be persisted as `always_allow`, except built-in `ssh` after explicit user selection.
- [ ] Enable supervised mode and confirm every tool action is gated, including normally auto-approved tools.
- [ ] Trigger context compaction and verify the conversation remains usable.
- [ ] Exercise a model/provider failure and verify bounded retry/failover behavior.
- [ ] **Response cache and malformed final recovery** (v2.1.9.0, WI#545/WI#572/WI#571/WI#585) — prove model/request metadata separates cache entries, smart-routed/failover and tool calls bypass it, and empty/reasoning-only/corrupt responses miss it; a stream hit preserves finish reason without discarded retry usage. Return an uppercase `FINAL…</arg_value>` pseudo-action and confirm its answer is consumed before lease enforcement while a real `finalize_order` action still executes.
- [ ] **Billed-empty retry input budget** (v2.2.0.0, WI#578/WI#595) — script billed-empty responses whose reported inputs cross `LLM_EMPTY_STREAM_RETRY_INPUT_TOKEN_BUDGET`; confirm the first attempt still gets one retry, then no further attempt opens after a later attempt crosses the threshold and the typed `EmptyStream` retains all usage. Verify `0` restores retry-count-only behavior, usage-less empty responses are unaffected, and failover legs and recovery re-issues have independent budgets.
- [ ] **Legacy idle/interrupted steering** (v2.2.0.0, WI#584) — queue a V1 steer note while idle and inspect the first real provider request: the note precedes the new request with explicit pre-turn framing and is drained. Repeat for approval resume and live iteration-boundary delivery. After a soft timeout whose turn still completes with a response, confirm pending messages and steer notes are both discarded, including a steer-only race, rather than leaking into a later turn. After a hard abort (the turn hangs past the hard-kill grace), confirm both queues are preserved and delivered to the next turn exactly once.

## Tools, Extensions, and Skills

- [ ] Sandbox policy behavior matches the active runtime (`SANDBOX_RUNTIME` auto/podman/docker) and network mode (`SANDBOX_NETWORK` none/proxy-enforced/tunnel/proxy). Linux defaults to `proxy-enforced`: the container keeps `--network none` and reaches only the validating public-only allowlist proxy through a per-execution Unix-socket relay. Other platforms default to `none`. `none` disables egress, `tunnel` selects the overlay sidecar, and `proxy` is the legacy advisory mode. Verify the platform default, zero credential mappings in enforced mode, and preservation of an operator override.
- [ ] **Sandbox storage diagnostics** (v2.2.0.0, WI#587) — simulate runtime ENOSPC during image inspect/pull/setup and confirm typed non-transient `storage_unavailable` leads with the real storage path and preserves runtime detail. Ordinary command stderr containing ENOSPC, a forged host path, or missing-image text must remain ordinary output when the command exits with any status other than 125 or prints no `Error:`-prefixed line; only the exit-125/error-prefix shape classifies host storage failure. Known gap: a sandboxed command that itself exits 125 with an `Error:` line is classified as a runtime failure, so that shape is not proof of origin. Review remediation: `df`, tenant-user dangling-only `podman image prune`, and never `prune -a` for the locally built image.
- [ ] **Sandbox tunnel fail-closed boundary** (v2.2.0.0, WI#248) — with a disposable WireGuard sidecar, verify overlay-only egress, no inbound accept, no host/metadata/uplink bypass, stable sidecar reuse, and refusal after injecting firewall drift. Keep provider-specific NetBird interoperability as a separately evidenced tier; do not substitute an argument/unit test for a live overlay claim (see `SANDBOX_TUNNEL_NETWORKING.md`).
- [ ] HTTP requests obey URL validation, private-address policy, redirects, and capability allowlists.
- [ ] Memory read, write, tree, and hybrid search preserve workspace semantics.
- [ ] **Daily-log backup generation cap** (v2.1.6.0, WI#335) — seed the day log, then drive agent `memory_write` replaces (cap is `MAX_DAILY_LOG_BACKUP_GENERATIONS = 8` numbered generations plus the rolling `.bak`): the first nine succeed (the ninth fills `.bak.8`) and the tenth fails closed as a `NotAuthorized` refusal naming the retained generations, nothing is written, no generation is lost, and appends still work; an operator replace through the web memory editor or CLI still succeeds on the same saturated day (see `ic/src/workspace/README.md`).
- [ ] Secret list/delete operations never return secret values.
- [ ] Install, authenticate, activate, deactivate, update, and remove a disposable tool.
- [ ] Install and invoke a WASM tool with host-injected credentials.
- [ ] List, search, inspect, install, and remove a disposable skill.
- [ ] Verify lifecycle operations that require approval cannot bypass the gate through the web API.
- [ ] Exercise image analysis or generation only when the configured provider supports it.

## Routines and Events

- [ ] Create, fire, inspect history for, disable, and delete a manual routine.
- [ ] Create a cron routine and verify one scheduled execution.
- [ ] Trigger an event-driven routine with `event_emit`.
- [ ] Run a lightweight routine that uses a harmless tool.
- [ ] **Lightweight routine tool-round budget** (v2.1.5.0, WI#291) — a tool-enabled lightweight routine gets `min(max_tool_rounds, ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS, 10)` tool-calling rounds plus one final text-only reply (previously N−1). With the default routine value of 3 (at or below the operator cap), confirm up to four model calls per run, and that a provider-recovery retry adds one model call without spending a round; raise one routine with `lunarwing routines edit --name <routine> --max-tool-rounds <n>` (values 1–10, refused not clamped) and confirm the limit reply names the knob(s) that bounded the run — both when the routine's value ties the `ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS` cap (the reply must say both must rise) and when only one knob binds (the reply names only that knob) (see `../ops/TENANT-CONFIGURATION.md`).
- [ ] Run a full-job routine through a configured supported worker.
- [ ] Verify cooldown, duplicate-fire prevention, failure recording, and notification routing.

## Workers and Jobs

Test only workers configured for the release candidate. Nanocode is retired and must not appear as a runnable option.

- [ ] Pebble job: creation, NDJSON events, prompt/follow-up, completion, and cancellation.
- [ ] OpenCode (v1) job: connection, event flow, completion, and failure handling.
- [ ] OpenCode 2 job (opt-in via mt-admin `--with-opencode2`): connection, event flow, completion, and failure handling.
- [ ] Kimi Code job: connection, event flow, completion, and failure handling.
- [ ] Worker disconnect: verify connection-level failover and clear terminal state.
- [ ] Container boundary: verify secrets are not copied into worker output or environment diagnostics.
- [ ] Job list, status, events, prompt, and cancel operations agree across CLI/web/tool surfaces.
- [ ] **External-worker proxied tools** (v2.1.6.0, WI#324) — on a worker with a `tool_allowlist` grant (kimi lane ships the `lunarwing` MCP shim), confirm a permitted proxied call (e.g. `memory_read` of an allowed workspace path) round-trips through the hub, an unlisted tool is absent from the worker's tool catalog, and a denied call returns the hub's sanitized reason; a cron-mode worker gets no worker-level grants on ordinary dispatches (see `../ops/WORKER-CONTAINERS.md`).
- [ ] **Worker source and egress boundaries** (v2.1.9.0 WI#539/WI#540; WI#424) — build every enabled lane from the operator-selected release ref and clean operator-owned archive; unknown, off-ref, dirty sources and unsafe archive entries must fail before image build. Audit `worker-listeners <tenant>` and `worker-egress <tenant>`: no self-published listener/share path, the per-tenant network and static hosts match configuration, live nftables rules allow only LLM/operator destinations, and a missing `nft` or unproven filter fails closed.

## Multi-Tenant and Init Coverage

- [ ] Provision a disposable tenant through `ic/scripts/lunarwing-mt-admin.sh` or its thin onboard wrapper.
- [ ] Verify port allocation and registry entries, including `vision_service` and `vision_health` when OCR is enabled.
- [ ] Run MT admin `status <tenant>` plus host `doctor`; do not substitute bare system-bus `systemctl`.
- [ ] Verify systemd user-manager behavior on systemd hosts.
- [ ] **Service-mode workload environment scrub** (v2.2.0.0, WI#612) — from `ic/` on a host with a working systemd user manager, run `XDG_RUNTIME_DIR=/run/user/$(id -u) LUNARWING_REQUIRE_SYSTEMD_TESTS=1 taskset -c 0-11 cargo test -j12 --lib tools::process::tests -- --test-threads=12`. The `XDG_RUNTIME_DIR` assignment reaches the user bus even from a `sudo -u` or `su` shell that does not inherit it; `LUNARWING_REQUIRE_SYSTEMD_TESTS=1` turns the exact-environment test's usual skip into a failure when no user manager is available. CI has no user manager and ordinarily reports that skip as `ok`; do not count it as coverage.
- [ ] Verify OpenRC service rendering and lifecycle on a Gentoo/OpenRC host, or record the tier as deferred.
- [ ] **systemd/launchd root-install refusal** (v2.2.0.0, WI#613) — on disposable single-node installs, a direct-root invocation is refused before a user unit or plist is written and names `LUNARWING_ALLOW_ROOT_SERVICE=1`; the explicit opt-in preserves the prior install behavior, a non-root service account installs normally, and the setup wizard suppresses an offer it would refuse.
- [ ] **OpenRC uid/gid-0 identity refusal** (v2.2.0.0, WI#574/WI#613) — on a disposable single-node install, verify literal `root`, a non-root-named uid-0 alias, and a non-root account with primary gid 0 are refused before service files are written unless `LUNARWING_ALLOW_ROOT_SERVICE=1` is explicit; a normal non-root account still installs, an unknown account fails closed, and the wizard suppresses a refused offer.
- [ ] Start, stop, restart, build, and remove the disposable tenant through MT admin.
- [ ] **Required tenant LLM endpoint** (v2.1.6.0, WI#330) — with both fleet vars (`LUNARWING_MT_TENSORZERO_URL`, `LUNARWING_MT_LLM_BASE_URL`) unset, `add-tenant` without `--tensorzero-url`/`--llm-base-url` aborts before creating tenant state; `--enable-proxy` without an upstream aborts; a rendered worker env never carries the baked `.invalid` placeholder (the render dies naming the fix), while a hand-started worker container without `<WORKER>_BASE_URL` refuses to start on the baked `.invalid` config naming the variable to fix; `upgrade-tenant` backfills an inheritable endpoint for enabled workers before rebuilding (see `MT-ADMIN-QUICKSTART.md` §3).
- [ ] Confirm host health discovery uses the tenant port registry rather than a hardcoded single-node port.

## Security and Recovery

- [ ] Search logs, settings exports, API responses, worker events, and downloaded diagnostics for credential leaks.
- [ ] Verify authentication, CORS/origin checks, body limits, rate limits, and webhook authorization on exposed listeners.
- [ ] Test SSRF rejection for remote HTTP private, loopback, link-local, and metadata targets where applicable.
- [ ] Verify sandbox and full-access double opt-in behavior.
- [ ] Restore the database backup or execute the documented rollback in staging.
- [ ] Confirm watchdog/self-heal alerts and restart limits behave as documented.
- [ ] **Pending-gates corrupt-file quarantine** (v2.1.5.0, WI#299) — with a gate paused in `pending-gates.json`, corrupt the file (invalid JSON) and restart the engine: the unparseable file is renamed to `pending-gates.json.corrupt-<unix-millis>` (warn-level log naming both paths), saves proceed against fresh empty state, and the orphaned Waiting threads fail at boot reconciliation. Also confirm temp/lock files are created `0600` and a save replaces the live inode atomically (see `../architecture/ENGINE-V2.md`).
- [ ] **Redacted mission-load diagnostics** (v2.2.0.0, WI#576) — remove `goal` from a disposable Active mission and restart: it stays dropped, WARN includes its validated id/path, `error_kind=data`, and `missing_field="goal"`, and no goal text appears. Unknown variants, invalid values, forged field names, and full Serde errors must not reach WARN; full detail remains DEBUG-only.
- [ ] **Startup version evidence** (v2.2.0.0, WI#579) — confirm the first INFO operational log after tracing starts is `Starting LunarWing v<package-version>` and matches the deployed binary, including one systemd-user and one OpenRC installation where available.

## Cleanup and Sign-Off

- [ ] Remove disposable tenants, routines, jobs, secrets, repositories, channels, tools, and containers.
- [ ] Stop harness services even after a failed verification.
- [ ] Confirm no orphaned processes, user units, OpenRC services, or port-registry entries remain.
- [ ] Attach redacted logs and command results to the release evidence.
- [ ] List every skipped or failed tier with owner, risk, and follow-up.
- [ ] Reconcile release notes and known issues with the tested commit.

A release candidate is fully verified only when every required tier has fresh evidence or an explicit reviewed deferral.
