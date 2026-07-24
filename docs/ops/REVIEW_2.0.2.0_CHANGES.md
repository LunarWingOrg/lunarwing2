# Review of Changes Since v2.0.1.0 (v2.0.2.0 "Dokutsu")

**Date:** 2026-07-24  
**Reviewer:** Rarity (agent)  
**Scope:** 83 non-merge commits across 56 source files (+5467 / -710 lines)  
**Method:** Full `git diff v2.0.1.0..HEAD` review against `ic/src/`

---

## Summary of Changes

| Area | Commits | Key Files |
|------|---------|-----------|
| MCP lifecycle (deactivate, re-enable, in-place update) | item-7, item-19 | `extensions/manager.rs`, `tools/mcp/config.rs`, `tools/mcp/client.rs`, `tools/mcp/http_transport.rs`, `cli/mcp.rs`, `cli/doctor.rs` |
| Memory tool hardening | f82feed, 406cffa, 7326461 | `tools/builtin/memory.rs`, `workspace/mod.rs`, `workspace/document.rs` |
| Engine V2 cadence fixes | 1025fac, bd302e3, 68f1085 | `bridge/effect_adapter.rs`, `bridge/router.rs`, `gate/store.rs` |
| Timeout alignment | d127268 | `config/agent.rs`, `config/llm.rs`, `llm/timeout.rs`, `app.rs` |
| LLM retry/oversized-request | 7326461 | `llm/retry.rs`, `llm/rig_adapter.rs` |
| Cavepony skill + WASM tool | 2d40beb, f095c52 | `skills/bundled.rs`, `tools/wasm/capabilities_schema.rs` |
| Skill proposal bug fix | 5d10f13, a341c99 | `channels/web/static/app.js` |
| Kawarimi (Hermes migration) | b1cddb8 | `docs/proposals/` |
| WeeChat buffer restore | ce058b7, 4883ba9 | `scripts/` |
| DarkIRC key exchange | ec4a2d5, 636bd4c | integration tests |
| CI/CD pipeline | ca48e80 | `.gitlab-ci.yml`, `scripts/` |
| Version bump | 07b4605 | `Cargo.toml` files |

---

## Findings

### 1. ToolError Derived Clone — Unnecessary and Potentially Problematic

**Severity:** Low  
**Files:** `ic/src/tools/tool.rs:156`

`ToolError` was changed from `#[derive(Debug, Error)]` to `#[derive(Debug, Error, Clone)]`. This was likely done to support cloning the shutdown result in `http_transport.rs` (`shutdown_session` returns `Result<(), ToolError>` via `OnceCell::get_or_init`, which yields a reference, and `.clone()` is called on the consumer side).

**Concern:** Every variant is a `String`-based enum, so `Clone` is technically sound. However, this broadens the public API surface of a core error type for a single internal use case. A more targeted approach would be to store the shutdown result as `Result<(), String>` in the `OnceCell` and construct the `ToolError` at the call site.

**Recommendation:** Consider reverting the `Clone` derive and using `String` in the `OnceCell` instead, or document why `ToolError: Clone` is now an intended API contract.

---

### 2. Memory Tool Constants — Magic Numbers Without Centralized Budget Source

**Severity:** Low  
**Files:** `ic/src/tools/builtin/memory.rs:22-30`

The memory tool hardening introduces many hardcoded byte constants:
```rust
const MAX_MEMORY_TOOL_OUTPUT_BYTES: usize = 8_000;
const MAX_MEMORY_SEARCH_CONTENT_JSON_BYTES: usize = 2_000;
const MAX_MEMORY_SEARCH_QUERY_BYTES: usize = 1_024;
// ... etc
```

These interact with the Engine V2 action-result budget but are not derived from any shared constant or configuration. If the engine's budget changes, these must be manually kept in sync.

**Recommendation:** Consider sourcing `MAX_MEMORY_TOOL_OUTPUT_BYTES` from the engine's action-result budget constant (if one exists) or making it configurable. At minimum, add a comment linking these values to the engine budget they are designed to fit within.

---

### 3. Oversized Request Detection — Fragile String Matching

**Severity:** Medium  
**Files:** `ic/src/llm/retry.rs:59-77`

The `is_oversized_request` function uses a hardcoded list of substring patterns to detect context-length-exceeded errors:
```rust
"context length exceeded",
"context_length_exceeded",
"maximum context length",
// ... 13 patterns
```

This is inherently fragile — any new provider or proxy error format will silently fall through and be retried. The patterns also overlap (e.g., `"413 payload too large"` and `"status code: 413"`).

**Recommendation:**
- The HTTP status code check (`413`) on `LlmError::Http` is good and robust.
- For `RequestFailed`, consider also checking for known status-code substrings more generically (e.g., `r"\b413\b"`) rather than enumerating provider-specific message formats.
- Document that this list is best-effort and will need maintenance as new providers are added.

---

### 4. MCP Config Write Guard — fs4 File Lock Without Cleanup

**Severity:** Low  
**Files:** `ic/src/tools/mcp/config.rs:440-460`

`acquire_mcp_config_write_guard` creates a `.lock` file and uses `fs4::FileExt::lock_exclusive` for cross-process mutual exclusion. The lock file is never explicitly deleted (it persists across runs). This is intentional for advisory locks but means orphaned lock files accumulate if the directory path changes.

**Recommendation:** Acceptable as-is for the current use case. If MCP config paths are ever per-tenant or rotated, stale lock files should be cleaned up on startup.

---

### 5. Per-User Tool Visibility — Potential Information Leak via Error Messages

**Severity:** Low  
**Files:** `ic/src/tools/tool.rs:282-284`, `ic/src/tools/registry.rs:113-115`, `ic/src/tools/builtin/tool_info.rs:142-146`

The new `owner_user_id()` trait method and `is_visible_to` / `get_for_user` filtering correctly hide owner-bound tools from other users in discovery and execution. However, `ToolInfoTool::execute` returns `"No tool named '{name}' is registered"` when a tool exists but belongs to another user. This is the correct approach (don't reveal existence), but verify that no other code path leaks the tool's presence through side channels (e.g., timing, SSE events, or extension list responses).

**Recommendation:** Audit all code paths that enumerate tools to ensure they consistently use `tool_definitions_for_user` or `get_for_user`. The changes in `dispatcher.rs`, `routine_engine.rs`, `worker/job.rs`, and the web handlers look correct.

---

### 6. Pending Gate Expiry — Race Between Memory and Persistence Cleanup

**Severity:** Low  
**Files:** `ic/src/gate/store.rs:119-135, 180-188`

The expired-gate replacement logic in `insert()` removes the expired entry from memory before releasing the lock, then persists the new gate. If persistence fails, the rollback correctly removes the new gate from memory. Good.

However, in the `peek()` expiry path (`store.rs:180-188`), when an expired gate is detected during peek, the in-memory entry is removed and the persistence removal is done **after** dropping the inner lock (`drop(inner)`). Between the lock drop and the persistence call, another insert for the same thread could succeed in memory while the old entry's persistence removal hasn't completed yet. This is benign because the old entry's `request_id` differs from the new one, but it's worth noting.

**Recommendation:** No action needed — the design is sound. The comment above is purely informational.

---

### 7. Cadence Parsing — Interval Range Validation Asymmetry

**Severity:** Low  
**Files:** `ic/src/bridge/effect_adapter.rs:930-960`

The `interval_cadence` function validates:
- Minutes: 1m–59m → `*/{N} * * * *`
- Hours: 1h–23h → `0 */{N} * * *`

But the cron translation for minutes (`*/{N} * * * *`) actually allows values like 60m via `*/60 * * * *` which would fire only at minute 0. The range check prevents this, which is correct. However, the upper bound for hours (23h) means `0 */23 * * *` fires once per day at midnight — semantically identical to `daily`. This is not wrong but could confuse users.

**Recommendation:** Consider documenting that `23h` is equivalent to `daily` or rejecting it in favor of the `daily` keyword.

---

### 8. HTTP MCP Transport Shutdown — No Timeout on DELETE Request

**Severity:** Medium  
**Files:** `ic/src/tools/mcp/http_transport.rs:230-260`

The `perform_shutdown` method sends an HTTP DELETE to terminate the MCP session. This request uses the shared `reqwest::Client` which has its own timeout, but the shutdown path does not apply an explicit timeout. If the MCP server hangs on DELETE, `wait_until_drained()` has already completed (no in-flight requests), but the DELETE itself could block.

**Recommendation:** Add an explicit timeout to the DELETE request, e.g.:
```rust
let response = request
    .timeout(std::time::Duration::from_secs(10))
    .send()
    .await
```
This prevents a misbehaving MCP server from hanging the deactivation path indefinitely.

---

### 9. MCP Lifecycle Lock HashMap — Unbounded Growth

**Severity:** Low  
**Files:** `ic/src/extensions/manager.rs:164-165, 795-803`

`mcp_lifecycle_locks: Mutex<HashMap<String, Arc<Mutex<()>>>` creates a new `Arc<Mutex<()>>` entry for every unique server name ever locked and never removes entries. Over a long-running process with many MCP server add/remove cycles, this map grows unboundedly.

**Recommendation:** This is low-impact in practice (MCP server names are few and stable). If it ever matters, add cleanup in the `remove` path or use a weak-reference map. Not urgent.

---

### 10. LibSQL Migration 23 — Direct Path Comparison Instead of Normalized

**Severity:** Low  
**Files:** `ic/src/db/libsql_migrations.rs:930-940`

Migration 23 cleans up Engine runtime state from search:
```sql
WHERE path = 'engine/.runtime'
   OR substr(path, 1, length('engine/.runtime/')) = 'engine/.runtime/'
```

This assumes the runtime path is always stored without leading slashes. The `normalize_path` function in `workspace/mod.rs` strips leading/trailing slashes, so this should be consistent. However, if any historical data was stored with different path formats, the migration could miss some entries.

**Recommendation:** Verify that all workspace writes go through `normalize_path` consistently. The migration is one-shot and the runtime path detection logic in `workspace/mod.rs` (`is_engine_runtime_path`) should be the single source of truth for this check.

---

### 11. Worker Job Tool Definitions — Empty Vec on Missing Context

**Severity:** Low  
**Files:** `ic/src/worker/job.rs:304-310, 1239-1252`

When `job_context` is `None` or `get_context` fails, the worker falls back to an empty `Vec<ToolDefinition>`:
```rust
reason_ctx.available_tools = match self.worker.context_manager()
    .get_context(self.worker.job_id).await {
    Ok(context) => self.worker.tools().tool_definitions_for_user(&context.user_id).await,
    Err(_) => Vec::new(),
};
```

This means a transient context-manager failure during a worker iteration would cause the LLM to see zero tools and likely fail or produce a poor response.

**Recommendation:** Consider logging at `warn!` level when this fallback is hit, so operators can diagnose context-manager reliability issues. Currently it silently degrades.

---

### 12. i18n Keys for Deactivate — Missing zh-CN Completeness Check

**Severity:** Low  
**Files:** `ic/src/channels/web/static/i18n/en.js`, `ic/src/channels/web/static/i18n/zh-CN.js`

The new i18n keys `mcp.deactivated` and `mcp.deactivateFailed` were added to both `en.js` and `zh-CN.js`. Good. However, `common.deactivate` is referenced in `app.js` but was not shown in the diff — verify it exists in both locale files.

**Recommendation:** Run a quick check that `I18n.t('common.deactivate')` resolves in both locales.

---

## Positive Observations

1. **MCP lifecycle is well-designed**: The per-server lifecycle lock, exact-tool ownership tracking, and clean shutdown (client → transport → session → process) are solid. The `register_if_vacant` / `unregister_if_same` pattern prevents name collisions during concurrent activation.

2. **Memory tool budget enforcement is thorough**: The JSON-aware truncation functions (`truncate_for_json`, `json_char_encoded_len`) correctly handle multi-byte characters and produce valid JSON output within byte budgets. Good defensive engineering.

3. **Cadence parsing is now strict**: The old `parse_cadence` silently defaulted unknown strings to `Manual`, which could hide user typos. The new version returns an explicit error with a helpful message.

4. **Gate store expiry handling is correct**: The expired-gate replacement, failed-persistence rollback, and stale-gate cleanup on peek are all properly tested.

5. **Engine V2 error type improvement**: Replacing the fake `JobError::ContextError` with `Error::Engine` removes the misleading nil UUID from error messages.

6. **Timeout alignment is verified by test**: The `engine_timeout_fits_default_message_timeout` test in `app.rs` ensures the three-tier timeout hierarchy (orchestrator 600s → message timeout 630s → LLM turn budget 370s) stays consistent.

---

## Action Items (Prioritized)

| # | Priority | Item | Effort |
|---|----------|------|--------|
| 1 | Medium | Add explicit timeout to MCP HTTP transport DELETE shutdown request | Small |
| 2 | Medium | Consider generic status-code regex for oversized-request detection instead of fragile string list | Small |
| 3 | Low | Document or revert `ToolError: Clone` derive — clarify intent | Small |
| 4 | Low | Add `warn!` logging when worker falls back to empty tool list | Trivial |
| 5 | Low | Verify `common.deactivate` i18n key exists in all locale files | Trivial |
| 6 | Low | Link memory tool byte constants to the engine budget they target | Trivial |
| 7 | Low | Document `23h` ≡ `daily` cadence equivalence | Trivial |

---

## Conclusion

The v2.0.2.0 changes are generally well-structured with good test coverage. The MCP lifecycle work is the largest and most complex addition, and it handles concurrency correctly. The main areas for improvement are the HTTP DELETE timeout (#8), the fragile oversized-request string matching (#3), and minor documentation/logging gaps. No critical security issues were found.
