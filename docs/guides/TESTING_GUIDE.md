# LunarWing Release Testing Guide

Use this checklist for manual release validation. The resource-bounded commands, harness sequence, gateway smoke test, external-worker runner, and browser E2E instructions live in [`../ops/PRE-RELEASE-TESTING.md`](../ops/PRE-RELEASE-TESTING.md).

**Revalidated against source 2026-09-12 (v2.1.1.0).** Referenced ops runbook,
`ic/scripts/release-test.sh`, and the channel/worker/multi-tenant tiers were
re-checked against the current tree; no changes were needed.

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

- [ ] Read, write, list, patch, and shell behavior matches the active sandbox policy.
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
- [ ] OpenCode job: connection, event flow, completion, and failure handling.
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
