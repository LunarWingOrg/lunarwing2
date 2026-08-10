# Coordination Pipeline — Phase 1 Declarative Hooks

**Status:** Implemented for v2.0.4.0; retained as a Phase 1 capability record.
The shipped bundle is `ic/config/hooks/coordination.hook.json`, compiled into the
daemon and scoped at startup to exact `COORD_CHANNELS` matches. The adjacent
`coordination-phase1-bundle.json` is synchronized with that guarded source
artifact. A legacy workspace copy is ignored to prevent duplicate enforcement.
See [`DEPLOYMENT.md`](DEPLOYMENT.md) for current guidance.

Phase 1 uses the existing declarative hook system (`HookBundleConfig`). Generic
workspace hook files still load from `hooks/hooks.json` or `hooks/*.hook.json`;
the coordination bundle is managed directly by `bootstrap_hooks()`.

## Hook System Capabilities (verified from source)

**RuleHook fields:**
- `name` — stable identifier (scoped with source during registration)
- `points` — lifecycle points: beforeInbound, beforeToolCall, beforeOutbound, onSessionStart, onSessionEnd, transformResponse
- `priority` — lower runs first (default 100)
- `failure_mode` — fail_open or fail_closed (default fail_open)
- `timeout_ms` — 1-30000ms
- `when_regex` — content guard; if set and matches, action fires; otherwise no-op
- `when_channel_regex` — optional destination/policy-scope guard
- `reject_reason` — if set with matching guards, Reject event
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
1. ✅ Exact coordination-scope guards derived from `COORD_CHANNELS`
2. ✅ Explicit API-key/private-key/JWT rejection (inbound + outbound); stateful entropy detection is Phase 2
3. ✅ Newline stripping on inbound (regex replacement)
4. ✅ Newline rejection on outbound
5. ✅ 420-char limit rejection on outbound

What Phase 1 CANNOT enforce (needs Phase 2):
1. ❌ Tag schema enforcement (needs negative match)
2. ❌ Rate limiting (needs state)
3. ❌ First-responder correlation (needs state)
4. ❌ Three-strike enforcement (needs state)
5. ❌ Engagement policy (needs agent identity)
6. ❌ Coalesce window (needs timer)
