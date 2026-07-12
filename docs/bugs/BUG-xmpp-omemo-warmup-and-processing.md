# XMPP OMEMO fallback spam and processing stall

> **Status: UNVERIFIED (checked against HEAD 2026-07-12).** The historical
> fallback-spam symptom has not been reproduced against the current bridge and
> no current source path proves that it is resolved. The separate stuck
> `Processing`-thread failure has a current static fix and is recorded below.

This is the active form of `history/XMPP-OMEMO-BUG-TO-DO.md`. The two symptoms
were reported together, but they have different evidence and should not share a
single resolved status.

## 1. OMEMO fallback spam

**Status: UNVERIFIED.**

### Historical symptom

The v1.0.7 known-issues record says that posting in an encrypted MUC produced
approximately 20 plaintext OMEMO fallback notices in the private 1:1 JID chat,
even when OMEMO was disabled for that JID (`docs/releases/RELEASE-v1.0.7.md:81-86`,
discovered 2026-05-12). The original bug note also described repeated
`I sent you an OMEMO encrypted message but your client doesn't seem to support
that` messages after a bridge/daemon restart, with the first few MUC messages
failing before later messages appeared to decrypt.

The release note is the strongest concrete evidence. The current tree contains
no live bridge capture or MUC regression test that establishes whether this
specific cross-JID fan-out still occurs. The prior note's claim that this is
always expected warmup is therefore not verified; the cited `CLAUDE.md` OMEMO
section is not present in this checkout.

### Current code checks

- The WASM XMPP config accepts `allow_plaintext_fallback` and defaults it to
  `true` (`ic/channels-src/xmpp/src/lib.rs:271-292`).
- The setting is forwarded to the bridge configure request
  (`ic/channels-src/xmpp/src/lib.rs:295-313`).
- Unit tests cover direct-message fallback when enabled and suppress it when
  disabled (`ic/src/channels/xmpp/mod.rs:3261-3334`). These tests do not cover
  encrypted MUC routing or repeated notices to a separate 1:1 JID.

These checks show that the configuration knob and DM guard exist; they do not
prove the reported MUC behavior is fixed.

### Reproduction evidence needed

To close or reproduce this item, capture the bridge `/v1/status` response,
`allow_plaintext_fallback` and encrypted-room configuration, the source room
and destination JIDs, message counts/timestamps, and daemon logs around the
first post-restart messages (`RUST_LOG=lunarwing=debug`). A live bridge run is
required; source inspection alone is insufficient.

## 2. Stuck processing loop

**Status: FIXED in the current timeout/recovery path (static verification; no
live OMEMO run).**

The original symptom was `Message queued — will be processed after the current
turn` followed by no reply. The documented root cause was cancellation of
`handle_message` while a thread was already `Processing`
(`docs/internal/history/architecture/HANDLE_MESSAGE_FIX.md:5-20`). The current
agent loop runs `handle_message` in a spawned task, keeps it alive through the
soft timeout, and uses a hard-kill recovery that calls `fail_turn` while
preserving queued messages (`ic/src/agent/agent_loop.rs:934-1023`). The
regression tests cover resetting a stuck thread and draining its queued
messages (`ic/src/agent/session.rs:1641-1670`).

This fixes the generic processing-state failure; it does not establish that the
historical OMEMO fallback spam was caused by that failure.

## Related reports and verification

The separate polling/supervision issue is tracked in
[`BUG-xmpp-polling-and-backpressure.md`](BUG-xmpp-polling-and-backpressure.md).
No Cargo command, live XMPP bridge run, or full E2E run was performed in this
documentation pass.
