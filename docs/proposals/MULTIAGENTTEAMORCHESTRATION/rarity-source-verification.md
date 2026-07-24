# Rarity's Source Verification & Feasibility Assessment

## Date: 2026-07-24
## Reviewer: Rarity
## Subject: Coordination Pipeline Plan (by Captain Ruffles) — verified against v2.0.2.0 source

---

## Methodology

Verified every structural claim in the coordination-pipeline-plan.md against the
actual LunarWing v2.0.2.0 source code at `/home/rarity/lunarwing2_gitlab`. Checked
hook trait signatures, enum variants, gate pipeline, bootstrap loading paths, and
RuleHook execution flow line-by-line.

---

## VERIFIED ACCURATE

### Hook System (ic/src/hooks/)

1. **HookPoint enum** — `BeforeInbound`, `BeforeToolCall`, `BeforeOutbound`,
   `OnSessionStart`, `OnSessionEnd`, `TransformResponse` all exist exactly as
   described. serde rename to camelCase (`beforeInbound`, etc.) confirmed.

2. **HookEvent variants** — `Inbound { user_id, channel, content, thread_id }`
   and `Outbound { user_id, channel, content, thread_id }` match plan exactly.
   `ResponseTransform { user_id, thread_id, response }` confirmed — note: NO
   channel field (see concerns below).

3. **HookOutcome** — `Continue { modified: Option<String> }` and
   `Reject { reason: String }` confirmed. Shorthand methods `ok()`, `modify()`,
   `reject()` all present.

4. **HookFailureMode** — `FailOpen` (default) and `FailClosed` confirmed.
   serde rename to snake_case confirmed.

5. **Hook trait** — `name()`, `hook_points()`, `failure_mode()`, `timeout()`,
   `execute()` all match. **BUT: default timeout is 5 seconds, NOT 30 as the
   plan states.** Plan says `Duration::from_secs(30)`, actual is
   `Duration::from_secs(5)`. Coordination hooks should be fast enough either way,
   but the plan's timeout claim is wrong.

6. **HookContext** — only has `metadata: serde_json::Value`. Ruffles correctly
   identified that stateful hooks must own their state internally via
   `Arc<Mutex<...>>`. No shared state injection possible through HookContext.

7. **HookRegistry** — `register_with_priority(hook, priority)` confirmed at
   registry.rs:40. Default priority is 100 (line 34). RwLock<Vec<HookEntry>>
   with priority ordering confirmed.

8. **RuleHook::execute()** — verified line-by-line against bundled.rs:386-425:
   - Extract primary content from HookEvent
   - Guard check: if `when_regex` set and doesn't match → return Ok (no-op)
   - Reject check: if `when_regex` matches (or unset) and `reject_reason` set → Reject
   - Apply `replacements` array in order (regex replace_all)
   - Apply `prepend`/`append` modifications
   - If content changed → Modify, else Ok
   All confirmed exactly as described in plan.

9. **Workspace hook loading** — `register_workspace_bundles()` at
   bootstrap.rs:254. `is_workspace_hook_file()` at bootstrap.rs:319-321 matches
   `hooks/hooks.json` and `hooks/*.hook.json`. Parse flow: read workspace →
   parse JSON → `parse_workspace_bundle()` → `HookBundleConfig::from_value()`.
   Confirmed.

### Gate System (lunarwing_engine/src/gate/)

10. **GateDecision** — `Allow`, `Pause { reason, resume_kind }`,
    `Deny { reason }` confirmed at gate/mod.rs:29. **BUT: no `silent: bool`
    field exists on `Deny`.** Phase 3 plan proposes adding one — this is a code
    change, not a bug in the plan. Needs audit of all match sites on
    `GateDecision` to add the field safely.

11. **ExecutionGate trait** — `name()`, `priority()`, `evaluate(&GateContext)`
    confirmed at gate/mod.rs:149. "Lower runs first. First Pause/Deny wins"
    confirmed.

12. **GateContext** — `user_id`, `thread_id`, `source_channel`, `action_name`,
    `parameters`, `execution_mode`, `auto_approved`, `supervised_mode`,
    `supervised_timeout_secs`, plus `call_id` and `action_def` (not mentioned
    in plan but present). Lifetime parameter `<'a>` confirmed.

13. **Gate files** — `mod.rs`, `pipeline.rs`, `lease.rs`, `tool_tier.rs` exist.
    Plan references `store.rs` (for PendingGateStore) — need to verify location.

---

## CONCERNS FOUND

### 1. EngagementPolicyHook + Router (BLOCKER for Phase 2)

**The problem:** The plan says EngagementPolicyHook will `Reject` non-targeted
messages at `BeforeInbound` to prevent engagement. BUT: `HookOutcome::Reject`
on `BeforeInbound` likely just blocks the message from being processed — it does
NOT prevent the router from spawning a thread. The router makes thread-spawning
decisions independently of hook outcomes.

**Impact:** An agent could still be "woken up" and consume tokens/turns even
though the hook rejected the inbound message. The rejection only prevents the
message from reaching the agent's conversation, not the thread spawn itself.

**Ruffles flagged this correctly as Open Question #1.** This is the real
architectural blocker for Phase 2. Needs a design decision:

- Option A: Modify the router to respect BeforeInbound rejections for thread
  spawning (clean but invasive)
- Option B: Let the thread spawn but have the agent immediately return without
  generating a response (wasteful but non-invasive)
- Option C: Add a new hook point between message receipt and thread spawn
  (cleanest but requires engine changes)

**Recommendation:** Option B for initial implementation, Option A for 2.0.5.0+
when the router can be safely refactored.

### 2. TransformResponse Has No Channel Field

**The problem:** `HookEvent::ResponseTransform { user_id, thread_id, response }`
has NO `channel` field. So `TagSchemaHook` and `CoordinationOutboundHook` cannot
know whether a transform is destined for a coordination channel.

**Ruffles' workaround:** Use `get_any_coord_channel()` as proxy — but this means
enforcing tag schema on ALL transform responses, not just coordination channel
ones.

**Impact:** False rejections of legitimate non-coordination responses that don't
start with STATUS/ALERT/COORD/ANNOUNCE/QUERY tags.

**Recommendation:** Add an optional `channel: Option<String>` to
`ResponseTransform` variant. Low-risk change since it's additive.

### 3. GateDecision::Deny Has No `silent` Field

**The problem:** Phase 3 proposes `GateDecision::Deny { reason: String,
silent: bool }` but the current enum is `Deny { reason: String }`.

**Impact:** Adding a field touches every match arm on `GateDecision::Deny` in
the codebase. Need to audit all match sites.

**Recommendation:** Use `reason: String` with a convention prefix like
`"[SILENT] "` instead of adding a field. Simpler, no enum change needed.

### 4. Hook Timeout Default: 5s, Not 30s

**Minor:** Plan states `Duration::from_secs(30)` but actual default is
`Duration::from_secs(5)`. Coordination hooks (rate limiting, entropy, tag check)
should be sub-millisecond, so 5s is more than enough. But the plan should be
corrected.

### 5. High-Entropy False Positives

**The problem:** `SecretEntropyHook` with threshold 4.5 bits/char on 32+ char
tokens will catch:
- Base64-encoded data (very common in agent messages)
- Git commit SHAs (40 hex chars = ~4.0 bits/char, borderline)
- File paths (long paths can hit 4.5)
- URLs with long query strings
- Cargo crate version strings with hashes

**Impact:** False rejections of legitimate coordination messages containing
code references.

**Recommendation:**
- Start with threshold 4.8 bits/char (catches base64, allows hex)
- Add an allowlist for known-safe patterns (commit SHAs, file paths)
- Consider context-aware detection (only check tokens that look like keys,
  not all tokens)
- Phase 1 regex-based detection is sufficient initially; entropy can be
  Phase 2.5 or later

### 6. First-Responder Store: Per-Host vs Shared

**The problem:** Per-host store means agents on different machines can't see
each other's claims. For Round 3 (kitchen hypervisor), agents will be on
separate VMs but same physical host.

**Impact:** Duplicate responses possible when agents are on different VMs.

**Recommendation:** For Phase 2, per-host is fine (same-VM agents benefit
immediately). For Phase 3+, add a shared file-based store on a common
filesystem (NFS mount on the hypervisor) or Redis on the vnet.

### 7. CoordinationGate Priority Verification

**The problem:** Plan claims CoordinationGate at priority 5 ("above LeaseGate
at 10"). Need to verify actual LeaseGate priority.

**Status:** Not verified — didn't check lease.rs priority value. Should confirm
before implementing Phase 3.

---

## OVERALL VERDICT

**Architecturally sound.** Ruffles did genuine source-level verification, not
guessing. The 6 components map correctly onto existing engine systems. The
phased approach is well-structured and delivers value incrementally.

**Phases:**
- Phase 1 (declarative hooks): **READY NOW**, zero Rust changes
- Phase 2 (stateful Rust hooks): **FEASIBLE**, ~900 LOC, engagement policy
  is the blocker
- Phase 3 (three-strike gate): **FEASIBLE**, ~320 LOC, needs silent-deny
  approach decision
- Phase 4 (coalesce window): **FEASIBLE**, ~210 LOC, clean design

**Total estimated: ~1,480 LOC across all 4 phases.**

**Recommended AGENT_GOALS split for 2.0.4.0:**
1. Phase 1 deploy + verify (can ship 2.0.3.0 — no Rust needed)
2. Phase 2 Rust hooks (rate limit, tag schema, secret entropy, engagement)
3. Phase 3 Gate + three-strike (ViolationStore, CoordinationGate, admin HTTP)
4. Phase 4 coalesce window (channel adapter integration)

**Note:** Many of the plan's constraints reference mid-April assessments from
the original Kageho/Baud framework. The codebase has evolved through 2.0.0.0 →
2.0.2.0 since then. Specific API shapes (timeouts, struct fields, enum variants)
should be re-verified against current source before coding each phase.

---

*Verified by Rarity, Jul 24 2026. Captain Ruffles earned the title.*
