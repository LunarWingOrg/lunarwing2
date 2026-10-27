# WI#442 engine-v2 migration completeness — F3/F8 record

Tracker: <https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/442>
Findings origin: joint SweetieBot/Kumo 2.1.2.0 burn-down, re-verified at HEAD
`369152526` on 2026-10-18 (pass-237). Re-audited 2026-10-27 for this item.

F7 (v1-history import) was the one finding that needed code. F3 and F8 are
verification/decision findings; this note is their record. F7's implementation
is in `ic/src/bridge/router/v1_history_import.rs` and is described in
`docs/architecture/ENGINE-V2.md` ("Pre-switch history import").

## F3 — do_longpoll watermark: not in the DarkIRC lane (out of tree, deployment-only)

**Kumo's original finding** referred to a v1-era DarkIRC `do_longpoll` path that
tracked last-seen message ids in `LAST_SEEN_IDS`. In-tree at this HEAD the
symbols exist **nowhere in the DarkIRC lane**:

```
$ grep -rn "do_longpoll\|LAST_SEEN_IDS" ic/src ic/crates darkirc_channel_for_lunarwing
(no DarkIRC hits)
```

The only `do_longpoll` / `LAST_SEEN_IDS_PATH` in the whole repo is the
**WeeChat** relay (`lunarwing_weechat_wss/weechat_relay/src/lib.rs:496`,
`:1235`), which already carries the fix: it loads the watermarks at each
long-poll tick, drops already-seen line ids before `handle_inbound_line`, and
persists them after processing (release-v1.1.2 note). That is a different
channel from the one Kumo flagged.

The DarkIRC lane's modern inbox is **not a watermark/cursor long-poll at all**.
Its adapter (`darkirc_channel_for_lunarwing/darkirc/adapter/darkirc_adapter.py`)
uses an **at-least-once opaque batch** model: `/poll` hands out a
`pending_batch_id`, unacked messages are re-served on the next poll, and
`/ack` clears them (docstring lines 9–10; `pending_ack` queue lines 457–474).
There is no last-seen-id watermark to go stale, so the F3 hazard has no
mechanism on the current DarkIRC path.

**Deployment check (this host, 2026-10-27):**

- Ports registry `/etc/lunarwing/ports.json`: every provisioned tenant
  (`antelope`, `scootaloo`, `thunderlane`) has `enable_darkirc = false` and no
  `darkirc`/`darkirc_adapter` ports.
- No DarkIRC service or release selector is installed: `ls /etc/init.d |
  grep -c darkirc` → `0`; `/usr/local/lib/lunarwing/darkirc` does not exist.

**Verdict:** on this host no deployed DarkIRC lane carries old v1 watermark
code, because no DarkIRC lane is deployed at all, and the current lane's
transport is at-least-once batch rather than a watermark long-poll. Kumo's
index entry can be closed as **stale-context for master** and **not
reproducible on the deployed DarkIRC lane here**. Any OTHER tenant running a
pre-migration DarkIRC build from old source remains the only possible carrier;
that is an ops check per deployment, not an in-tree defect.

## F8 — `ENGINE_V2_OPT_IN_CHANNELS` allowlist (by design, decision recorded)

`ic/src/bridge/router.rs` keeps the eligible set at `["xmpp","darkirc","weechat"]`
and `engine_v2_channel_allowed` routes only exact names plus `gateway`
(`ENGINE_V2=false` is the only durable full opt-out).

**Decision (recorded here and in `ENGINE-V2.md`): keep the allowlist as-is.**

- It is a deliberate rollout control, not a gap. Non-opted channels continue on
  the v1 loop by design; engine coverage for them is WI#221/#222 scope.
- The operational consequence is stated for operators: v1-era findings on
  non-opted channels are **not** retired by an ENGINE_V2 switch — the allowlist
  is exactly why that v1 surface stays load-bearing (which is also why F3
  above is framed as a deployment check).
- No change to the allowlist is proposed here. Expanding it requires adding an
  eligible channel to the constant, the docs, and a coverage test together;
  retiring the v1 loop is a separate, larger decision.

This satisfies WI#442 F8's acceptance ("explicit operator decision recorded"):
the decision is to keep the allowlist by design, unchanged.

## F7 — v1-history import (implemented)

Forward-only dual-write (`router.rs` "Dual-write to v1 database", "Write to v1
DB so the history API") is complemented by a one-time import:

- `router/v1_history_import.rs::import_v1_history_if_needed` reads the v1
  conversation's `user`/`assistant` rows once, gated on the
  `engine_v2_v1_history_imported` metadata marker.
- The engine surface's `created_at` is the cutoff, so forward dual-write rows
  from v2 turns are never re-imported even when the marker write fails.
- `ConversationManager::import_v1_history` prepends them as
  `imported_from: v1` entries (`EntrySender::ImportedAgent` for agent rows);
  `build_history_from_entries` maps those to assistant history so a new thread
  inherits pre-switch context.
- Best-effort: any DB/store failure is logged and the turn proceeds.

`backfill_archived_threads` (`store_adapter.rs`) remains separate and unchanged;
it seeds archived-thread references, not v1 history.
