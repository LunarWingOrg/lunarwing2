# Reflex Compiler & Software Builder — v2 Upgrade Plan

**Date:** 2026-07-27
**Author:** Volta 🦊⚡
**Target Release:** v2.0.3.0 (Phases 1–4), v2.0.4.0 (Phase 5)
**Source Analysis:**
- `docs/proposals/REFLEXCOMPILERANDSOFTWAREBUILDERUPDATESNEEDED/REFLEXCOMPILERANALYSIS.md` (SweetieBot, 2026-07-23)
- `docs/proposals/REFLEXCOMPILERANDSOFTWAREBUILDERUPDATESNEEDED/SOFTWAREBUILDERANALYSIS.md` (SweetieBot, 2026-07-23)
- `docs/internal/history/proposals/reflex-compiler.md` (original implementation plan)

**Related Goal:** AGENT_GOALS_2.0.3.0.md item #23

---

## Executive Summary

The reflex compiler was well-architected in v1 but neglected during the v2 engine migration. SweetieBot's deep-dive analysis (July 23, 2026) identified **12 gaps in the reflex system** and **15 gaps in the Software Builder**. The core matching logic (exact → fuzzy → semantic) is sound and well-tested (30+ unit tests, 7 E2E tests), but critical wiring gaps prevent correct production behavior:

- Semantic matching (Tier 3) is fully implemented but **never activated** — the embedding provider is never connected to the router
- Match tracking is **broken** — routing hits are never recorded back to the database, so active patterns will be incorrectly evicted
- The evictor background task **exists but is never spawned** — stale patterns accumulate forever
- Multi-tenant support was **never updated** — user_id is hardcoded to `"default"`
- The Software Builder (reflex's compilation backend) has its own critical bugs including a serde naming mismatch that silently breaks LLM-generated build requirements

The system is disabled by default (`REFLEX_COMPILER_ENABLED=false`), so these issues are latent. Enabling it today would produce a partially-functional system with broken staleness tracking, no semantic matching, and no stale-pattern cleanup.

This plan sequences the fixes into five phases, ordered by severity and dependency.

---

## Current Architecture (Reference)

```
User Input
    │
    ▼
[ReflexRouter] ──(fast path)──▶ [Compiled WASM Tool] ──▶ Response
    │ (miss)                        ▲
    ▼                               │
[Normal LLM Path]                   │
    │                               │
    ▼                               │
[Background ReflexCompiler] ────────┘
    (periodic scan of agent_jobs)
```

**Three-tier matching:**
| Tier | Method | Mechanism | Status |
|------|--------|-----------|--------|
| 1 | Exact match | O(1) hash lookup on normalized input | ✅ Working |
| 2 | Fuzzy match | Jaro-Winkler + word-overlap pre-filter (threshold 0.85) | ✅ Working |
| 3 | Semantic match | Embedding cosine similarity (threshold 0.75) | ❌ Dead in prod |

**Key files:**
| File | Lines | Purpose |
|------|-------|---------|
| `ic/src/agent/reflex.rs` | 1,710 | Core router, compiler, evictor, normalization, tests |
| `ic/src/agent/dispatcher.rs` | ~2,584 | Fast-path check before LLM (lines 166-190) |
| `ic/src/agent/agent_loop.rs` | ~2,500+ | Background task spawning (lines 792-822) |
| `ic/src/cli/reflex.rs` | 331 | CLI management (list, show, delete, status, prune) |
| `ic/src/db/libsql/reflex.rs` | 334 | libSQL ReflexStore implementation |
| `ic/src/db/postgres.rs` | ~1,450 | PostgreSQL ReflexStore implementation |
| `ic/src/tools/builder/core.rs` | ~1,400 | SoftwareBuilder (reflex's compilation backend) |

---

## Phase 1 — Critical Wiring Fixes

**Severity:** 🔴 CRITICAL
**Estimated effort:** Small (~30 lines changed across 3 files)
**Branch suggestion:** `reflex/phase1-wiring/v2.0.3.0`

These are the "it's broken in production" fixes. All three are small, well-understood changes with existing test coverage for the underlying methods.

### 1a. Wire Embedding Provider to ReflexRouter

**Problem:** `agent_loop.rs:347` creates `ReflexRouter::new()` with `embedding_provider: None`. The `with_embedding_provider()` builder method exists and is tested (`reflex.rs:1578-1581`), but is never called in production. This means:
- Tier 3 semantic matching is permanently disabled
- `semantic_route()`, `pattern_embeddings` cache, `embed_missing_patterns()`, and the DB's `semantic_search_reflex_patterns()` + `update_reflex_pattern_embedding()` are all dead code in production
- The workspace's `EmbeddingProvider` (used for workspace search) is not shared with the reflex router

**Fix:** In `Agent::new()` (line 347) or `Agent::run()`, attach the workspace embedding provider:

```rust
// agent_loop.rs — replace line 347
let mut router = crate::agent::reflex::ReflexRouter::new();
if let Some(ws) = &deps.workspace {
    if let Some(provider) = ws.embedding_provider() {
        router = router.with_embedding_provider(provider);
    }
}
reflex_router: Arc::new(router),
```

**Verification:**
- Existing test `test_reflex_router_semantic_match` (`reflex.rs:1578+`) covers the routing logic
- Add integration test: create router with mock embedding provider → verify `has_embedding_provider()` returns true
- Manual: enable reflex compiler, create patterns, verify semantic tier activates in logs

### 1b. Call `bump_reflex_pattern_match()` in Dispatcher Hot Path

**Problem:** `dispatcher.rs:166-190` routes to a compiled tool but **never records the match** back to the database. Consequences:
- `match_count` and `last_matched_at` in `reflex_patterns` are never updated during routing
- The eviction sweep's staleness check (`last_matched_at < cutoff`) will evict patterns that are actively being used
- `match_count` only reflects compilation-time counts from `find_recurring_job_patterns`, not actual routing hits

The `bump_reflex_pattern_match()` method exists in both DB backends and is tested (`reflex.rs:1336-1340`), but is never invoked from the dispatcher.

**Fix:** After successful `execute_chat_tool()` in `dispatcher.rs` (line ~180):

```rust
// dispatcher.rs — after line 180 (Ok(output) match arm)
Ok(output) => {
    // Record the match for staleness tracking
    if let Some(store) = self.store() {
        let normalized = crate::agent::reflex::normalize_pattern(&message.content);
        if let Err(e) = store.bump_reflex_pattern_match(&message.user_id, &normalized).await {
            tracing::debug!("Failed to bump reflex pattern match: {}", e);
        }
    }
    return Ok(AgenticLoopResult::Response(output));
}
```

**Note:** Use `debug!` not `warn!` for the error case — this is telemetry, not a user-facing failure. The reflex still succeeded.

**Verification:**
- Existing test `test_reflex_store_bump_match` covers the DB method
- Add integration test: route a message → verify `match_count` incremented and `last_matched_at` updated in DB

### 1c. Spawn ReflexEvictor

**Problem:** `agent_loop.rs:792-822` spawns the compiler and cache refresh tasks, but **`spawn_reflex_evictor()` is never called**. The `ReflexEvictor` struct and spawn function exist (`reflex.rs:758-864`) and are tested, but production code only spawns compiler + cache refresh. Stale patterns accumulate indefinitely. The only eviction path is manual: `lunarwing reflex prune --stale-days N`.

**Fix:** Add evictor spawn in the `reflex.enabled` block of `Agent::run()` (after line 822):

```rust
// agent_loop.rs — inside the reflex.enabled block, after cache refresh spawn
let _reflex_evictor_handle = if self.config.reflex.enabled {
    if let Some(store) = self.store() {
        tracing::info!("Spawning reflex evictor (stale_after_days: 30)");
        Some(crate::agent::reflex::spawn_reflex_evictor(
            Arc::clone(store),
            crate::agent::reflex::EvictionConfig::default(),
        ))
    } else {
        None
    }
} else {
    None
};
```

**Verification:**
- Existing E2E test `e2e_reflex_compiler.rs` covers eviction logic
- Manual: enable reflex, create patterns with old `last_matched_at`, verify evictor logs appear after interval

---

## Phase 2 — Multi-Tenant & Retry Logic

**Severity:** 🟠 HIGH
**Estimated effort:** Medium (~50-80 lines, design decision required)
**Branch suggestion:** `reflex/phase2-multitenant/v2.0.3.0`

### 2a. Fix Hardcoded `"default"` User ID

**Problem:** `ReflexCompiler::compile_pattern()` hardcodes user_id as `"default"` at `reflex.rs:673` and `:705`:

```rust
match self.store.get_reflex_pattern("default", &normalized).await {  // line 673
...
self.store.upsert_reflex_pattern("default", &normalized, description, &safe_name).await  // line 705
```

Additionally, `find_recurring_job_patterns()` does not filter by user_id — it groups all completed jobs globally.

This is a v1-era single-user assumption that diverges from v2's multi-tenant architecture.

**Design decision required:**

**Option A — Per-user compilation (recommended for multi-tenant):**
- Thread `user_id` through `ReflexCompiler` (add field or pass per-sweep)
- `find_recurring_job_patterns()` gains a `user_id` parameter
- Each user gets their own pattern space
- Pro: true multi-tenant isolation
- Con: more DB queries (one per active user per sweep)

**Option B — Global compilation, per-user routing (simpler):**
- Compiler stays global (patterns compiled once for all users)
- `reflex_patterns` gains a `scope` column: `'global'` or user-specific
- Router checks user-specific patterns first, falls back to global
- Pro: efficient (compile once, route many)
- Con: patterns from one user's jobs could match another user's input

**Recommendation:** Option A for correctness. The compiler sweep interval is 30 minutes by default — the extra queries are negligible.

**Changes:**
- `ReflexCompiler` struct: add `user_id: String` field (or `Vec<String>` for multi-user sweep)
- `compile_pattern()`: use `self.user_id` instead of `"default"`
- `find_recurring_job_patterns()`: add `user_id` parameter to trait + both backends
- `spawn_reflex_compiler()`: accept user_id parameter
- `agent_loop.rs`: pass user_id when spawning (line 801)

### 2b. Increment `compilation_attempts` on Build Failure

**Problem:** `reflex.rs:715-721` logs a warning on build failure but never increments `compilation_attempts`:

```rust
} else {
    tracing::warn!(
        "Failed to compile Reflex MS for pattern: {} (error: {:?})",
        description, result.error
    );
    // ← No store call to increment compilation_attempts!
}
```

The `compilation_attempts` column exists in the schema and `ReflexPatternRecord` but is always 0. The original proposal says "Limit to 3 attempts, then disable" — this was never implemented.

**Fix:**

```rust
} else {
    tracing::warn!(
        "Failed to compile Reflex MS for pattern: {} (error: {:?})",
        description, result.error
    );
    // Increment attempt counter; disable after 3 failures
    if let Err(e) = self.store.increment_reflex_compilation_attempts("default", &normalized).await {
        tracing::debug!("Failed to increment compilation attempts: {}", e);
    }
}
```

**New DB trait method required:**

```rust
/// Increment compilation_attempts for a pattern. Disable (set status='disabled')
/// if attempts reach max_attempts (default: 3).
async fn increment_reflex_compilation_attempts(
    &self,
    user_id: &str,
    normalized_pattern: &str,
) -> Result<(), Error>;
```

Implement in both `db/postgres.rs` and `db/libsql/reflex.rs`.

**Alternative (simpler):** Reuse `upsert_reflex_pattern` with a status check — if `compilation_attempts >= 3`, set `status = 'disabled'` in the upsert. This avoids a new trait method but is less explicit.

**Note:** After Phase 2a (multi-tenant), replace `"default"` with `self.user_id`.

---

## Phase 3 — Configuration & Cleanup

**Severity:** 🟡 MEDIUM
**Estimated effort:** Small (~40 lines)
**Branch suggestion:** `reflex/phase3-config/v2.0.3.0`

### 3a. Wire `FuzzyConfig` into Router Construction

**Problem:** `FuzzyConfig` struct exists (`reflex.rs:578-600`) with `threshold`, `promotion_threshold`, and `max_candidates` fields, but is never used to configure the router. The router uses hardcoded constants.

**Fix:** Accept `FuzzyConfig` in router construction:

```rust
impl ReflexRouter {
    pub fn with_fuzzy_config(mut self, config: FuzzyConfig) -> Self {
        self.fuzzy_threshold = config.threshold;
        // promotion_threshold and max_candidates would need to become fields
        self
    }
}
```

Or refactor constants into the struct and use `FuzzyConfig::default()` in `ReflexRouter::new()`.

### 3b. Add Missing `ReflexConfig` Options

**Problem:** `ReflexConfig` (`config/agent.rs:44-55`) is missing several options that the code supports:

| Missing Option | Current Value | Env Var |
|---|---|---|
| `fuzzy_threshold` | Hardcoded 0.85 | `REFLEX_FUZZY_THRESHOLD` |
| `semantic_threshold` | Hardcoded 0.75 | `REFLEX_SEMANTIC_THRESHOLD` |
| `eviction_check_interval` | Hardcoded 24h | `REFLEX_EVICTION_INTERVAL_SECS` |
| `eviction_stale_days` | Hardcoded 30 | `REFLEX_EVICTION_STALE_DAYS` |
| `max_patterns_per_user` | Not implemented (proposal says 50) | `REFLEX_MAX_PATTERNS_PER_USER` |

**Fix:** Add fields to `ReflexConfig`, parse from env vars in `config/agent.rs:163-171`, thread through to router and evictor construction.

### 3c. Fix PG Prune Re-Query Missing Embedding Columns

**Problem:** `postgres.rs:1390` SELECT after eviction omits `embedding` and `embedding_model` columns. The `row_to_reflex_pattern()` function uses `try_get` so it won't crash, but returned records will have `embedding: None` even though data exists in DB.

**Fix:** Add `embedding, embedding_model` to the SELECT list at `postgres.rs:1390` (the re-query after UPDATE).

---

## Phase 4 — Software Builder Fixes

**Severity:** 🟠 HIGH (reflex depends on builder for compilation)
**Estimated effort:** Medium-Large (~100-150 lines)
**Branch suggestion:** `builder/fixes/v2.0.3.0`

The reflex compiler calls `builder.build()` to compile WASM tools. SweetieBot's builder analysis found critical issues that affect reflex compilation reliability.

### 4a. Fix Language Serde Naming Bug

**Problem:** `Language` enum uses `#[serde(rename_all = "snake_case")]` (`core.rs:88`), producing `"type_script"` and `"java_script"`. But the LLM analyze prompt (`core.rs:835`) instructs the model to return `"typescript"` and `"javascript"`. Deserialization silently fails.

**Fix:**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    Python,
    #[serde(alias = "typescript")]
    TypeScript,
    #[serde(alias = "javascript")]
    JavaScript,
    Go,
    Bash,
}
```

### 4b. Wire WasmValidator + TestHarness into Build Loop

**Problem:** `templates.rs`, `testing.rs`, and `validation.rs` are fully implemented and exported but **never invoked** during `execute_build_loop()`. The `validate_wasm` and `run_tests` config flags are set but never checked. `tests_passed`/`tests_failed` in `BuildResult` are always 0.

**Fix:** After successful build in `execute_build_loop()`:

```rust
// After build succeeds, before returning BuildResult:
if config.validate_wasm && requirement.software_type == SoftwareType::WasmTool {
    let validator = WasmValidator::new();
    match validator.validate(&artifact_path) {
        Ok(result) if result.is_valid => { /* proceed */ }
        Ok(result) => { /* add warnings to BuildResult */ }
        Err(e) => { /* log, optionally fail build */ }
    }
}

if config.run_tests {
    let harness = TestHarness::new();
    // Run test suite, populate tests_passed/tests_failed
}
```

### 4c. Fix WASM Templates

**Problem:** Templates in `templates.rs` use raw FFI ABI (`extern "C"` with `#[link(wasm_import_module = "env")]`) but the actual WASM tool system uses WIT bindings via `wit_bindgen`. Templates also use `edition = "2021"` while repo requires Rust 2024.

**Fix:**
- Replace `extern "C"` FFI imports with `wit_bindgen::generate!()` in all templates
- Update `edition = "2021"` → `edition = "2024"` in template Cargo.toml generation
- Update `core.rs:380` WASM tool context template similarly

### 4d. Implement WASM Auto-Registration

**Problem:** `BuilderConfig.auto_register` defaults to `true` and `BuildResult.registered` exists, but the build loop never registers built WASM tools with the `ToolRegistry`. `BuildResult.registered` is always `false`.

**Fix:** After successful build + validation:

```rust
if config.auto_register && artifact_path.exists() {
    if let Some(wasm_dir) = &config.wasm_output_dir {
        // Copy artifact to wasm_output_dir
        // Register with ToolRegistry
        result.registered = true;
    }
}
```

Requires threading `Arc<ToolRegistry>` into the builder (currently only has `Arc<dyn LlmProvider>`).

---

## Phase 5 — v2 Bridge Integration (Deferred to v2.0.4.0)

**Severity:** 🔵 LOW (feature gap, not a bug)
**Estimated effort:** Large (new subsystem)
**Branch suggestion:** `reflex/bridge-v2/v2.0.4.0`

### 5a. Create Reflex Bridge Adapter

**Problem:** `src/bridge/` has adapters for SSH, LLM, store, effects, skills, and routing. There is **zero integration** between the reflex system and the v2 engine bridge. Specifically:
- The `mission_manager_slot` has no connection to reflexes
- No bridge adapter exists for reflex patterns
- The reflex router is not exposed to the v2 engine's effect/observation system
- Reflex matches cannot be observed as events by the v2 engine
- Reflex tool execution bypasses v2 cost tracking and observability

**Proposed adapter:** `src/bridge/reflex_adapter.rs`

```rust
/// Bridge adapter exposing reflex system to the v2 engine.
pub struct ReflexBridgeAdapter {
    router: Arc<ReflexRouter>,
    store: Arc<dyn Database>,
}

impl ReflexBridgeAdapter {
    /// Report a reflex match as an observable event to the v2 engine.
    pub async fn report_match(&self, pattern: &str, tool: &str, score: f64) { ... }

    /// Check if the v2 gate/approval system should intercept this reflex.
    pub async fn check_gate(&self, tool: &str) -> GateDecision { ... }

    /// Route reflex execution through v2 cost tracking.
    pub async fn track_execution(&self, tool: &str, duration: Duration) { ... }
}
```

### 5b. Migrate Builder from v1 Reasoning API

**Problem:** The builder uses legacy v1 `Reasoning`/`LlmProvider` API directly (`core.rs:513, 581, 849`) instead of the v2 engine bridge. Other systems have migrated to v2 patterns.

**This is a large refactor** — the builder's entire build loop (`execute_build_loop`, ~265 lines) would need to be reworked to use v2 tool-calling patterns. Recommend deferring to 2.0.4.0 and tackling alongside the broader v1→v2 migration.

### 5c. Reflex Path Safety Integration

**Problem:** The reflex fast-path (`dispatcher.rs:166-190`) executes compiled tools via `execute_chat_tool()` directly, bypassing:
- The v2 agentic loop's 3-phase tool execution (preflight → parallel exec → post-flight)
- Hook pre/post-execution callbacks
- Approval flow (`ApprovalContext`)
- Cost tracking / `CostGuard` checks
- Skill-based tool attenuation

This is architecturally intentional (speed is the point), but means compiled reflex tools have no safety guardrails beyond the WASM sandbox.

**Proposed approach:** Add a lightweight pre-execution check for reflex tools:

```rust
// In dispatcher.rs, before execute_chat_tool:
if self.requires_reflex_approval(&tool_name).await {
    // Route through approval flow instead of fast-path
    return self.handle_reflex_with_approval(&tool_name, &params, &job_ctx).await;
}
```

This preserves the fast-path for safe tools while gating risky ones.

---

## Implementation Order & Dependencies

```
Phase 1 (Critical Wiring) ─── No dependencies, do first
  ├── 1a: Embedding provider wiring
  ├── 1b: Match tracking in dispatcher
  └── 1c: Evictor spawn

Phase 2 (Multi-Tenant + Retry) ─── Depends on Phase 1
  ├── 2a: User ID fix (design decision needed)
  └── 2b: Compilation attempt tracking

Phase 3 (Config + Cleanup) ─── Independent of Phase 2
  ├── 3a: FuzzyConfig wiring
  ├── 3b: ReflexConfig expansion
  └── 3c: PG embedding column fix

Phase 4 (Builder Fixes) ─── Independent, parallel with Phase 2-3
  ├── 4a: Language serde fix (do first — silently breaks parsing)
  ├── 4b: Validator + TestHarness wiring
  ├── 4c: Template fixes (FFI → WIT, edition 2024)
  └── 4d: Auto-registration

Phase 5 (v2 Bridge) ─── Deferred to v2.0.4.0
  ├── 5a: Reflex bridge adapter
  ├── 5b: Builder v1→v2 migration
  └── 5c: Reflex path safety integration
```

**Phases 1–4 target v2.0.3.0.** Phase 5 targets v2.0.4.0.

Phases 1, 3, and 4 can proceed in parallel. Phase 2 depends on Phase 1 (match tracking must work before multi-tenant routing makes sense).

---

## Testing Strategy

### Existing Coverage (do not regress)
- 30+ unit tests in `reflex.rs:868-1710` (normalization, routing, fuzzy, semantic, DB CRUD)
- 7 E2E integration tests in `tests/e2e_reflex_compiler.rs`
- Builder unit tests in `core.rs:1018-1400`

### New Tests Required

| Phase | Test | Type |
|-------|------|------|
| 1a | Router created with embedding provider → `has_embedding_provider()` true | Unit |
| 1a | Semantic route activates when provider attached | Integration |
| 1b | Dispatcher route → `match_count` incremented in DB | Integration |
| 1b | Dispatcher route → `last_matched_at` updated | Integration |
| 1c | Evictor spawned when reflex enabled | Integration |
| 1c | Evictor sweeps stale patterns after interval | E2E |
| 2a | Per-user pattern isolation (user A patterns don't match user B) | Integration |
| 2b | Failed build increments `compilation_attempts` | Unit |
| 2b | Pattern disabled after 3 failed attempts | Integration |
| 3a | `FuzzyConfig` values respected by router | Unit |
| 3b | Env var overrides for thresholds | Unit |
| 3c | PG prune returns embedding columns | Unit |
| 4a | `"typescript"` deserializes to `Language::TypeScript` | Unit |
| 4a | `"javascript"` deserializes to `Language::JavaScript` | Unit |
| 4b | `validate_wasm=true` triggers validation in build loop | Integration |
| 4b | `run_tests=true` triggers test harness | Integration |
| 4c | Generated WASM template compiles with current runtime | E2E |
| 4d | `auto_register=true` registers tool with ToolRegistry | Integration |

### Verification Commands

```bash
# Run all reflex tests
cd ic && taskset -c 0-5 cargo test -j6 --lib --features libsql reflex -- --nocapture

# Run builder tests
cd ic && taskset -c 0-5 cargo test -j6 --lib builder -- --nocapture

# Run E2E reflex tests
cd ic && taskset -c 0-5 cargo test -j6 --test e2e_reflex_compiler -- --nocapture

# Full compile check
cd ic && taskset -c 0-5 cargo check -j6 --all-features

# Clippy (zero warnings policy)
cd ic && taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples --all-features -- -D warnings
```

---

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| Embedding provider attachment changes router behavior | Tier 3 only activates when provider exists; existing tiers unaffected |
| `bump_reflex_pattern_match` adds latency to hot path | Single indexed UPDATE; ~1ms on PG; fire-and-forget with debug logging |
| Multi-tenant refactor breaks single-user deployments | Default user_id remains `"default"`; multi-user is opt-in |
| Builder serde fix changes LLM parsing behavior | Aliases are additive; existing `"type_script"` still works |
| Template rewrite (FFI → WIT) breaks existing generated tools | Templates are unused in production build loop; no existing tools depend on them |
| Evictor spawn could evict patterns during active use | Phase 1b (match tracking) must land first; evictor uses `last_matched_at` which will now be accurate |

---

## Files Modified (Summary)

### Phase 1
| File | Change |
|------|--------|
| `ic/src/agent/agent_loop.rs` | Wire embedding provider to router; spawn evictor |
| `ic/src/agent/dispatcher.rs` | Add `bump_reflex_pattern_match()` after successful route |

### Phase 2
| File | Change |
|------|--------|
| `ic/src/agent/reflex.rs` | Thread user_id through compiler; increment compilation_attempts |
| `ic/src/db/mod.rs` | Add `increment_reflex_compilation_attempts()` to trait; add user_id to `find_recurring_job_patterns()` |
| `ic/src/db/postgres.rs` | Implement new trait methods |
| `ic/src/db/libsql/reflex.rs` | Implement new trait methods |
| `ic/src/agent/agent_loop.rs` | Pass user_id to compiler spawn |

### Phase 3
| File | Change |
|------|--------|
| `ic/src/agent/reflex.rs` | Accept FuzzyConfig in router construction |
| `ic/src/config/agent.rs` | Expand ReflexConfig with new fields + env vars |
| `ic/src/db/postgres.rs` | Fix prune re-query SELECT list |

### Phase 4
| File | Change |
|------|--------|
| `ic/src/tools/builder/core.rs` | Fix Language serde; wire validator + test harness; auto-registration |
| `ic/src/tools/builder/templates.rs` | FFI → WIT bindings; edition 2021 → 2024 |

### Phase 5 (v2.0.4.0)
| File | Change |
|------|--------|
| `ic/src/bridge/reflex_adapter.rs` | **New** — v2 bridge adapter for reflex system |
| `ic/src/bridge/mod.rs` | Export reflex adapter |
| `ic/src/tools/builder/core.rs` | Migrate from v1 Reasoning API to v2 bridge |
| `ic/src/agent/dispatcher.rs` | Add reflex approval gate |

---

## Success Criteria

The reflex compiler upgrade is complete when:

1. ✅ `REFLEX_COMPILER_ENABLED=true` produces a fully functional system
2. ✅ All three matching tiers activate in production (exact, fuzzy, semantic)
3. ✅ Match counts and staleness tracking update correctly during routing
4. ✅ Stale patterns are automatically evicted by the background evictor
5. ✅ Multi-user deployments compile and route patterns per-user
6. ✅ Failed compilations are tracked and capped at 3 attempts
7. ✅ Fuzzy/semantic thresholds are configurable via environment variables
8. ✅ The Software Builder correctly parses LLM-generated `BuildRequirement` for all languages
9. ✅ Built WASM tools are validated and optionally auto-registered
10. ✅ All existing tests pass; new tests cover all fixes
11. ✅ `cargo clippy --all-features -- -D warnings` is clean

---

*⚡ "Chaos is just data we haven't organized yet." — Volta 🦊💙*
