# P1 Gap Findings — v2.0.3.0

> **Deliverable for item #17** in `docs/ops/AGENT_GOALS_2.0.3.0.md`.
> Addresses P1 items (G4–G8) from `docs/ops/GOAL_GAP_SUGGESTIONS.md`.
>
> **Verified:** 2026-07-26 against branch `rarity/item-17-20260726-1501`
> (merge-base `010987c`).

---

## G4. libSQL backend drift audit

**Verdict: NO REGRESSION FOUND.** libSQL compiles cleanly and implements the
full `Database` trait surface.

### Methodology

- Ran `cargo check -j6 --no-default-features --features libsql` — passes.
- Extracted all `async fn` signatures from the `Database` trait
  (`ic/src/db/mod.rs`) and compared against both backends.
- Searched for `todo!()` / `unimplemented!()` in `ic/src/db/` — zero matches.

### Findings

- The trait declares 148 `async fn` methods (including private helpers).
- libsql implements 161 methods (trait + internal helpers); postgres
  implements 141 (trait + internal helpers).
- Four `_multi` methods (`hybrid_search_multi`, `list_all_paths_multi`,
  `get_document_by_path_multi`, `list_directory_multi`) have **default trait
  implementations** that delegate to single-scope versions. Postgres overrides
  them with combined queries; libSQL uses the defaults. This is intentional
  and documented at `ic/src/db/mod.rs:1025-1113`.
- No `todo!()` or `unimplemented!()` stubs exist in any DB module file.

### Action required

- None for v2.0.3.0. libSQL parity is maintained.
- Created `ic/src/db/CLAUDE.md` to record the verified feature surface (see
  below).

---

## G5. Worker / orchestrator lifecycle bug sweep

**Verdict: 3 of 7 bugs CLOSED, 3 PARTIALLY-FIXED, 1 STILL-OPEN (architectural).**

### Bug-by-bug status

| Bug doc | Status | Summary |
|---------|--------|---------|
| `BUG-unbounded-mpsc-recv-in-spawned-tasks.md` | **FIXED/INVALIDATED** | Channels are bounded, receiver loops close on sender drop. No task leak from the cited pattern. |
| `BUG-subagent-worker-hang.md` | **FIXED** | Fire-and-forget completion race resolved. Stale `ContextManager` state fixed. |
| `BUG-agent-loop-blocks-on-worker-jobs.md` | **STILL-OPEN** | Pointer to canonical doc. `wait=true` still occupies the originating turn. This is an architectural limitation, not a regression. |
| `BUG-agent-worker-lifecycle.md` | **PARTIALLY-FIXED** | Fire-and-forget race fixed, priority interrupts handled during active turns. Synchronous `wait=true` still serializes ordinary message handling. |
| `BUG-external-worker-config-persistence.md` | **PARTIALLY-FIXED** | Provisioning fixed (mt-admin generates endpoint blocks). Auth-token loss on `save_toml` round-trips is STILL-OPEN: `ExternalWorkerSettings.auth_token` uses `skip_serializing`, so full TOML rewrites (e.g., `/model` command) drop the token from disk. |
| `BUG-opencode-worker-tilde-expansion.md` | **PARTIALLY-FIXED** | Pointer to canonical. Structured dirs fixed, prompt-generated paths still open. |
| `BUG-worker-workspace-path-expansion.md` | **PARTIALLY-FIXED** | Structured worker project directories normalize `~`. Prompt-text paths and Pebble executor still pass user-supplied paths without normalization. |

### Consolidated assessment

The worker bug cluster has seen significant incremental fixes. The remaining
open items are:

1. **`wait=true` serialization** (`BUG-agent-worker-lifecycle.md` §1) — This is
   an architectural limitation in the turn model, not a bug fix candidate for
   v2.0.3.0. The 256-entry FIFO overflow handling and exact-interrupt delivery
   during active turns mitigate the worst symptoms.
2. **Auth-token persistence** (`BUG-external-worker-config-persistence.md` §2)
   — `Settings::merge_from` restores tokens during in-memory merges, but a
   prior `save_toml` rewrite already removed the token from disk. This needs a
   round-trip test and is the most actionable remaining fix.
3. **Prompt-text path expansion** (`BUG-worker-workspace-path-expansion.md`) —
   Structured directories are fixed; prompt-generated paths remain unexpanded
   in opencode/Nanocode executors and Pebble.

### Action required

- The auth-token persistence issue (`BUG-external-worker-config-persistence.md`
  §2) is the highest-value fix remaining. Recommend a follow-up to add a
  secret-preserving config merge or an env/DB source of truth for worker
  tokens.
- The `wait=true` and prompt-path items are deferrable to v2.0.4.0.

---

## G6. Tunnel provider cutover verification (NetBird)

**Verdict: PREMISE INVALID — providers were NOT gutted.** NetBird was ADDED
alongside existing providers; no cutover occurred.

### Findings

The G6 suggestion states "Item #17 gutted proprietary providers and added
NetBird." This is incorrect:

- Commit `cbc0040` ("add self-hosted NetBird tunnel provider") **adds**
  `netbird.rs` and wires it into the existing tunnel factory. It does **not**
  remove cloudflare, ngrok, or tailscale.
- All four legacy providers remain fully implemented:
  - `ic/src/tunnel/cloudflare.rs` — CloudflareTunnel (cloudflared)
  - `ic/src/tunnel/ngrok.rs` — NgrokTunnel (ngrok)
  - `ic/src/tunnel/tailscale.rs` — TailscaleTunnel (tailscale serve/funnel)
  - `ic/src/tunnel/netbird.rs` — NetBirdTunnel (netbird expose)
- `ic/src/tunnel/mod.rs:1-13` documents all six providers as supported.
- The tunnel factory (`mod.rs:230-272`) dispatches to all of them.

### Stale AGENTS.md reference

Both `AGENTS.md` (root) and `ic/AGENTS.md` line 40/44 say:

> Tunnel abstraction (cloudflare, ngrok, tailscale): `src/tunnel/`

This omits NetBird. Updated to include NetBird.

### Action required

- Updated `AGENTS.md` and `ic/AGENTS.md` "Where to Work" to include NetBird.
- No stale config, secret keys, or dangling references found — nothing was
  removed, so there is no cleanup needed.

---

## G7. OMEMO + XMPP backpressure bugs

**Verdict: Polling supervision FIXED, OMEMO fallback UNVERIFIED, backpressure
residual risk remains.**

### BUG-xmpp-polling-and-backpressure.md

**Status: PARTIALLY-FIXED.**

- **Fixed:** `WasmChannel::start_polling` creates an outer supervisor that
  respawns the inner poll loop with exponential backoff after errors/panics
  (`ic/src/channels/wasm/wrapper.rs:2366-2527`). Dead/stale poll tasks are
  detected by `health_check()` (`:2946-2987`). Watchdog deep channel-health
  checks exist for both systemd and OpenRC.
- **Residual:** The bounded message queue (capacity 256) can still leave a poll
  task pending in `tx.send` if downstream consumption stalls or sustained
  ingress exceeds drain rate. The supervisor does not detect this state because
  `last_poll_epoch_ms` is updated before dispatch. Health metrics do not expose
  queue backpressure explicitly.

### BUG-xmpp-omemo-warmup-and-processing.md

**Status: UNVERIFIED (fallback spam) / FIXED (processing stall).**

- **Stuck processing loop:** FIXED. The agent loop now runs `handle_message` in
  a spawned task with soft-timeout + hard-kill recovery
  (`ic/src/agent/agent_loop.rs:972-1034,1204-1284`). Regression tests cover
  resetting stuck threads and draining queued messages.
- **OMEMO fallback spam:** UNVERIFIED. The historical symptom (20+ plaintext
  fallback notices in 1:1 JID after posting in encrypted MUC) has not been
  reproduced against the current bridge. The `allow_plaintext_fallback`
  config knob and DM guard exist, but no test covers encrypted MUC routing or
  cross-JID fan-out. A live bridge run is required to close this.

### Action required

- OMEMO fallback spam needs a live XMPP bridge reproduction to verify or
  close. Deferrable to v2.0.4.0 if no live bridge is available this cycle.
- Backpressure queue-full detection in health metrics would be a valuable
  follow-up but is not a release blocker.

---

## G8. SSH-git ref/head bug cluster

**Verdict: 2 of 3 bugs FIXED, 1 STILL-OPEN (diagnostics/UX only).**

### Bug-by-bug status

| Bug doc | Status | Summary |
|---------|--------|---------|
| `BUG-ssh-git-null-ref-serialization.md` | **FIXED** | Null-like ref values normalized in `ssh_git.rs:68-74`. Trims and drops missing/JSON-null/empty/whitespace/case-insensitive "null". |
| `BUG-ssh-git-bare-repo-head-mismatch.md` | **STILL-OPEN** | Pointer to canonical §2. |
| `BUG-ssh-git-ref-and-remote-head.md` | **PARTIALLY-FIXED** | §1 (null refs) FIXED. §2 (bare repo HEAD mismatch) STILL-OPEN: Git exits 0 but leaves empty/unborn checkout. Tool marks success with only raw stderr warning. |

### Consolidated assessment

- The null-ref serialization bug is fully fixed with regression tests
  (`ic/src/tools/builtin/ssh_git.rs:640-661`).
- The bare-repo HEAD mismatch is a diagnostics/UX issue, not a data-integrity
  bug. Git itself exits 0; the tool faithfully reports what Git produces. A
  future enhancement could parse the stderr warning and return a structured
  warning, but this is low priority.
- Fix commit `c8b277d` is confirmed in HEAD (`merge-base --is-ancestor` passed).

### SSH harness proposals

`docs/proposals/SSH_HARNESS_OPTION_2_3_IMPLEMENTATION.md` and
`SSH_HARNESS_DELIVERY_OPTIONS.md` describe a tracked, partially-scoped change
to SSH delivery. These are enhancement proposals, not bug fixes, and should be
triaged separately for v2.0.4.0 fit.

### Action required

- The bare-repo HEAD mismatch is low-priority UX polish. Deferrable.
- The SSH harness proposals need a triage decision for v2.0.4.0.

---

## Summary

| ID | Priority | Verdict | Action for v2.0.3.0 |
|----|----------|---------|---------------------|
| G4 | P1 | No regression | None — libSQL compiles, full trait coverage |
| G5 | P1 | 3 fixed, 3 partial, 1 open | Auth-token persistence is highest-value remaining fix |
| G6 | P1 | Premise invalid | Updated AGENTS.md to include NetBird |
| G7 | P1 | Polling fixed, OMEMO unverified | OMEMO needs live bridge repro; deferrable |
| G8 | P1 | 2 fixed, 1 open (UX only) | Bare-repo HEAD is low-priority; deferrable |

### Files changed in this task

- `docs/ops/P1_GAP_FINDINGS_v2.0.3.0.md` — this document (new)
- `ic/src/db/CLAUDE.md` — recorded libSQL feature surface (new)
- `AGENTS.md` — updated tunnel provider list to include NetBird
- `ic/AGENTS.md` — updated tunnel provider list to include NetBird
- `docs/ops/AGENT_GOALS_2.0.3.0.md` — checked off item #17
