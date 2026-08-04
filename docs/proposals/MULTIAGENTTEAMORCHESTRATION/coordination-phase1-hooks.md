# Coordination Pipeline — Phase 1 Declarative Hooks

**Status:** Implemented for v2.0.4.0; retained as a Phase 1 capability record.
The shipped bundle is `ic/config/hooks/coordination.hook.json`, whose rules are
scoped at startup to exact `COORD_CHANNELS` matches. The adjacent
`coordination-phase1-bundle.json` is the original unscoped design artifact and
must not be deployed. See [`DEPLOYMENT.md`](DEPLOYMENT.md) for current guidance.

Phase 1 uses ONLY the existing declarative hook system (HookBundleConfig).
Workspace hook files are loaded from `hooks/hooks.json` or `hooks/*.hook.json`.

## Hook System Capabilities (verified from source)

**RuleHook fields:**
- `name` — stable identifier (scoped with source during registration)
- `points` — lifecycle points: beforeInbound, beforeToolCall, beforeOutbound, onSessionStart, onSessionEnd, transformResponse
- `priority` — lower runs first (default 100)
- `failure_mode` — fail_open or fail_closed (default fail_open)
- `timeout_ms` — 1-30000ms
- `when_regex` — guard; if set and matches, action fires; if doesn't match, no-op
- `reject_reason` — if set with matching guard, Reject event
- `replacements` — array of {pattern, replacement} regex substitutions
- `prepend` — text prepended to content
- `append` — text appended to content

**Execution logic (RuleHook::execute):**
1. Extract primary content from HookEvent (Inbound/Outbound → content field)
2. If when_regex set and doesn't match → return Ok (no-op)
3. If when_regex matches (or not set) and reject_reason set → return Reject
4. Apply replacements in order (regex replace_all)
5. Apply prepend/append
6. If content changed → return Modify, else return Ok

**Key limitation:** Rust `regex` crate does NOT support lookahead/lookbehind.
This means we CANNOT do "reject if NOT matching pattern X" in declarative rules.
Tag enforcement needs Phase 2 (Rust hooks) for this reason.

## Phase 1 Scope (Declarative Only)

What Phase 1 CAN enforce:
1. ✅ Secret detection + rejection (inbound + outbound)
2. ✅ Newline stripping on inbound (regex replacement)
3. ✅ Newline rejection on outbound
4. ✅ 420-char limit rejection on outbound
5. ✅ Webhook notifications for violation tracking

What Phase 1 CANNOT enforce (needs Phase 2):
1. ❌ Tag schema enforcement (needs negative match)
2. ❌ Rate limiting (needs state)
3. ❌ First-responder correlation (needs state)
4. ❌ Three-strike enforcement (needs state)
5. ❌ Engagement policy (needs agent identity)
6. ❌ Coalesce window (needs timer)
