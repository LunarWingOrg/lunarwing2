# LunarWing Release Testing Guide

Use this checklist for manual release validation. The resource-bounded commands, harness sequence, gateway smoke test, external-worker runner, and browser E2E instructions live in [`../ops/PRE-RELEASE-TESTING.md`](../ops/PRE-RELEASE-TESTING.md).

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-19 and 2026-09-27 (v2.1.3.0), and 2026-10-01 (v2.1.4.0).**
Referenced ops runbook,
`ic/scripts/release-test.sh`, and the channel/worker/multi-tenant tiers were
re-checked against the current tree. Added the v2.1.2.0 durable cross-channel
attachment tier below; worker cancellation now exercises the cancel-ack
handoff (bounded ~3s wait for the worker's terminal `task_result`). The
2026-09-19 repass updated the sandbox network-mode list for the WI#154/155
`proxy-enforced` mode (default `none` everywhere) and added the OpenCode 2
worker lane to the workers checklist (mt-admin `--with-opencode2`). The
2026-09-27 repass added the v2.1.3.0 WI#223 per-tenant sandbox-image tier
below (rendered env defaults + tenant rootless store + recovery verb). The
2026-10-01 repass added v2.1.4.0 tiers below: the tenant system-prompt
editor (WI#220), opt-in tool runtime-estimate hints (WI#191),
`routine_history` over Engine V2 missions (WI#265), and the mt-admin
read-only diagnostics verbs (WI#268).

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

### TensorZero / Tensormoon gateway bring-up

Before connecting tenants to a new gateway build, exercise its OpenAI-compatible
API against a loopback-only mock provider:

```bash
python3 ic/scripts/tests/gateway_compat_smoke.py \
  --gateway-binary /absolute/path/to/gateway --gateway-kind tensormoon
```

Use `--gateway-kind tensorzero` with an upstream TensorZero binary. The script
starts and stops a disposable gateway and mock provider, disables database
observability, and passes an isolated environment with no provider credentials.
It checks health, nonstreaming text, SSE text/usage, fragmented tool arguments,
tool-call/result round trips, model routing, and rejection of missing functions.
The Tensormoon lane exercises both native `tensormoon::` and legacy `tensorzero::`
request names and expects canonical Tensormoon response names.

Record the gateway commit and binary build command alongside the results.
A pass establishes gateway API behaviour with a mock provider; it does not prove
LunarWing's Rust client, external workers, database migrations, or a real model
provider work end to end. Keep those validation tiers separate.

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

## Tools, Extensions, and Skills

- [ ] Sandbox policy behavior matches the active runtime (`SANDBOX_RUNTIME` auto/podman/docker) and network mode (`SANDBOX_NETWORK` none/proxy-enforced/proxy). Default is `none` (no egress); `proxy-enforced` keeps `--network none` and reaches only the validating allowlist proxy through a per-execution Unix-socket relay; `proxy` is the legacy advisory mode.
- [ ] HTTP requests obey URL validation, private-address policy, redirects, and capability allowlists.
- [ ] Memory read, write, tree, and hybrid search preserve workspace semantics.
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

## Multi-Tenant and Init Coverage

- [ ] Provision a disposable tenant through `ic/scripts/lunarwing-mt-admin.sh` or its thin onboard wrapper.
- [ ] Verify port allocation and registry entries, including `vision_service` and `vision_health` when OCR is enabled.
- [ ] Run MT admin `status <tenant>` plus host `doctor`; do not substitute bare system-bus `systemctl`.
- [ ] Verify systemd user-manager behavior on systemd hosts.
- [ ] Verify OpenRC service rendering and lifecycle on a Gentoo/OpenRC host, or record the tier as deferred.
- [ ] Start, stop, restart, build, and remove the disposable tenant through MT admin.
- [ ] Confirm host health discovery uses the tenant port registry rather than a hardcoded single-node port.

## Security and Recovery

- [ ] Search logs, settings exports, API responses, worker events, and downloaded diagnostics for credential leaks.
- [ ] Verify authentication, CORS/origin checks, body limits, rate limits, and webhook authorization on exposed listeners.
- [ ] Test SSRF rejection for remote HTTP private, loopback, link-local, and metadata targets where applicable.
- [ ] Verify sandbox and full-access double opt-in behavior.
- [ ] Restore the database backup or execute the documented rollback in staging.
- [ ] Confirm watchdog/self-heal alerts and restart limits behave as documented.

## Cleanup and Sign-Off

- [ ] Remove disposable tenants, routines, jobs, secrets, repositories, channels, tools, and containers.
- [ ] Stop harness services even after a failed verification.
- [ ] Confirm no orphaned processes, user units, OpenRC services, or port-registry entries remain.
- [ ] Attach redacted logs and command results to the release evidence.
- [ ] List every skipped or failed tier with owner, risk, and follow-up.
- [ ] Reconcile release notes and known issues with the tested commit.

A release candidate is fully verified only when every required tier has fresh evidence or an explicit reviewed deferral.
