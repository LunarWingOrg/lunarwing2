# Coordination Pipeline — Phase 1: Declarative Hooks

## Status: IMPLEMENTED FOR v2.0.4.0 — HISTORICAL DESIGN RECORD
## Date: 2026-07-24

Phase 1 shipped as `ic/config/hooks/coordination.hook.json`. Since v2.0.7 it is
compiled in and registered automatically when coordination is actionable; the
manual workspace-copy steps below are historical. The current, source-verified
deployment contract is documented in [`DEPLOYMENT.md`](DEPLOYMENT.md); this plan
preserves the pre-implementation design and should not be used as an operator
runbook.

---

## Overview

Phase 1 implements coordination enforcement using the declarative hook system
(`HookBundleConfig`). The original deployment placed the bundle in workspace
`hooks/`; current startup embeds, scopes, and registers it directly, while
ignoring legacy workspace copies to avoid duplicate rules.

## Architecture (verified from source)

### Workspace Hook Loading Path
1. `bootstrap_hooks()` is called at startup (bootstrap.rs:38)
2. `register_workspace_bundles()` lists workspace paths (bootstrap.rs:254)
3. Files matching `hooks/hooks.json` or `hooks/*.hook.json` are loaded (bootstrap.rs:319-321)
4. Each file is parsed as `HookBundleConfig` → `register_bundle()` (bootstrap.rs:304)
5. Rules become `RuleHook` instances, webhooks become `OutboundWebhookHook` instances
6. Hooks are registered in the `HookRegistry` with priority ordering

### RuleHook Execution Flow (bundled.rs:386-425)
1. Extract primary content from HookEvent (Inbound/Outbound → content field)
2. Guard check: if `when_regex` set and doesn't match → return Ok (no-op)
3. Reject check: if `when_regex` matches (or unset) and `reject_reason` set → return Reject
4. Apply `replacements` array in order (regex replace_all)
5. Apply `prepend`/`append` modifications
6. If content changed → return Modify, else return Ok

### Priority Order (lower runs first)
- Bundled hooks: 25 (AuditLogHook)
- Rule hooks: 100 (default)
- Webhook hooks: 300 (default)

Our coordination hooks use priorities 10-80 for layered enforcement.

---

## What Phase 1 Enforces

### 1. Explicit Secret Detection (priority 10-12, fail_closed)
- API keys, tokens, passwords (regex pattern matching)
- Private key material (PEM headers)
- JWT tokens (eyJ... format)

The original priority-13 blanket base64 rule was removed in v2.0.7 because it
rejected exact git SHAs and other valid identifiers. Strike-aware entropy
checking now lives in the stateful Rust hook, with precise safe-token handling.

### 2. Inbound Newline Stripping (priority 50, fail_open)
- Replaces `\r\n`, `\r`, `\n` with spaces
- Runs BEFORE secret detection (lower priority = runs first... wait, 50 > 10, so this runs AFTER)

Actually: **Priority order means 10 runs BEFORE 50.** So secret detection runs first,
then newline stripping. This is correct — we want to catch secrets before transforming.

### 3. Outbound Newline Rejection (priority 50, fail_closed)
- Rejects any outbound message containing newlines
- Enforces single-line requirement

### 4. Outbound Max Length (priority 51, fail_closed)
- Rejects any outbound message >420 characters
- Regex: `[\s\S]{421,}` matches 421+ chars

---

## Limitations (needs Phase 2)

| Feature | Why Declarative Can't Do It |
|----------------------------|
| Tag schema enforcement | Needs "reject if NOT starts with tag" — negative match not possible without lookahead (Rust regex crate doesn't support it) |
| Rate limiting | Needs mutable state (per-agent message counters) |
| First-responder correlation | Needs mutable state (correlation ID map) |
| Three-strike enforcement | Needs mutable state (violation counters) + Gate integration |
| Engagement policy | Needs agent identity awareness + channel context |
| Coalesce window | Needs timer/delay mechanism |

---

## Historical deployment (superseded by managed startup registration)

1. Place `coordination-phase1-bundle.json` as `hooks/coordination.hook.json` in workspace
2. Restart daemon — hooks auto-load via `register_workspace_bundles()`
3. Monitor logs for hook registration: look for `hooks::audit` + coordination rule names
4. Test: send message with API key pattern → should be rejected silently
5. Test: agent sends message with newlines → should be rejected silently

## File: hooks/coordination.hook.json

```json
{
  "hooks": {
    "rules": [
      {
        "name": "secret-detect-api-keys-inbound",
        "points": ["beforeInbound"],
        "priority": 10,
        "failure_mode": "fail_closed",
        "when_regex": "(?i)(api[_-]?key|secret|token|password|bearer|authorization)\\s*[:=]\\s*['\"]?[A-Za-z0-9+/=_-]{16,}",
        "reject_reason": "coord: secret pattern detected in inbound message"
      },
      {
        "name": "secret-detect-api-keys-outbound",
        "points": ["beforeOutbound", "transformResponse"],
        "priority": 10,
        "failure_mode": "fail_closed",
        "when_regex": "(?i)(api[_-]?key|secret|token|password|bearer|authorization)\\s*[:=]\\s*['\"]?[A-Za-z0-9+/=_-]{16,}",
        "reject_reason": "coord: secret pattern detected in outbound message"
      },
      {
        "name": "secret-detect-private-keys",
        "points": ["beforeInbound", "beforeOutbound", "transformResponse"],
        "priority": 11,
        "failure_mode": "fail_closed",
        "when_regex": "-----BEGIN\\s+(RSA\\s+|EC\\s+|OPENSSH\\s+|PGP\\s+)?PRIVATE KEY-----",
        "reject_reason": "coord: private key material detected"
      },
      {
        "name": "secret-detect-jwt-tokens",
        "points": ["beforeInbound", "beforeOutbound", "transformResponse"],
        "priority": 12,
        "failure_mode": "fail_closed",
        "when_regex": "eyJ[A-Za-z0-9_-]{10,}\\.eyJ[A-Za-z0-9_-]{10,}\\.[A-Za-z0-9_-]{10,}",
        "reject_reason": "coord: JWT token detected"
      },
      {
        "name": "secret-detect-high-entropy-base64",
        "points": ["beforeInbound", "beforeOutbound", "transformResponse"],
        "priority": 13,
        "failure_mode": "fail_closed",
        "when_regex": "(?<![A-Za-z0-9+/=])[A-Za-z0-9+/]{48,}={0,2}(?![A-Za-z0-9+/=])",
        "reject_reason": "coord: high-entropy string detected (possible secret)"
      },
      {
        "name": "coord-newline-strip-inbound",
        "points": ["beforeInbound"],
        "priority": 50,
        "failure_mode": "fail_open",
        "replacements": [
          { "pattern": "\\r\\n|\\r|\\n", "replacement": " " }
        ]
      },
      {
        "name": "coord-newline-reject-outbound",
        "points": ["beforeOutbound", "transformResponse"],
        "priority": 50,
        "failure_mode": "fail_closed",
        "when_regex": "\\n|\\r",
        "reject_reason": "coord: outbound messages must be single-line"
      },
      {
        "name": "coord-max-length-reject-outbound",
        "points": ["beforeOutbound", "transformResponse"],
        "priority": 51,
        "failure_mode": "fail_closed",
        "when_regex": "[\\s\\S]{421,}",
        "reject_reason": "coord: outbound messages must not exceed 420 characters"
      }
    ]
  }
}
```

## Verification Plan

1. **Secret detection (inbound):** Send `api_key=sk-1234567890abcdefghijklmnop` → should be silently rejected
2. **Secret detection (outbound):** Agent generates output containing token pattern → should be silently rejected
3. **Private key detection:** Send PEM header → should be silently rejected
4. **Newline stripping (inbound):** Send message with `\n` → content should have newlines replaced with spaces
5. **Newline rejection (outbound):** Agent generates response with newlines → should be silently rejected
6. **Max length (outbound):** Agent generates 421+ char response → should be silently rejected
7. **Clean messages pass:** Normal coordination message with valid tag → should pass through normally

## Next Steps

After Phase 1 is verified:
- Phase 2: Rust hooks for tag schema, rate limiting, engagement policy
- Phase 3: Gate integration for three-strike enforcement
- Phase 4: Coalesce window in channel adapter
